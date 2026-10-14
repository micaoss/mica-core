use super::*;

fn sample(leap: u32, stratum: u32, offset_seconds: f64) -> NtpSample {
    NtpSample {
        leap,
        stratum,
        spike: false,
        offset_seconds,
        packet_count: 1,
    }
}

/// The online path: a reachable daemon, a selected server, and the
/// kernel's synchronized bit — the state a healthy networked device
/// settles into.
#[test]
fn a_disciplined_clock_classifies_synchronized() {
    let evidence = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: Some(true),
        server_name: Some("0.pool.ntp.org".to_string()),
        server_address: Some("192.0.2.7".to_string()),
        sample: Some(sample(0, 2, 0.012)),
    };
    assert_eq!(classify(&evidence), SyncStatus::Synchronized);
}

/// A server is selected and packets are being exchanged, but the kernel
/// bit is not set: polling, not degraded.
#[test]
fn a_selected_server_without_the_kernel_bit_is_polling() {
    let evidence = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: Some(false),
        server_name: Some("0.pool.ntp.org".to_string()),
        ..TimesyncEvidence::default()
    };
    assert_eq!(classify(&evidence), SyncStatus::Polling);
}

/// The bench state: timesyncd reachable, a server selected, a
/// usable reply in hand, and timedate1's bit clear.
#[test]
fn a_usable_sample_without_the_kernel_bit_does_not_claim_convergence() {
    let evidence = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: Some(false),
        server_name: Some("0.pool.ntp.org".to_string()),
        server_address: Some("192.0.2.7".to_string()),
        sample: Some(sample(0, 2, 0.004)),
    };
    let value = status_json(&evidence);
    assert_eq!(value["status"], "polling");
    assert_eq!(value["synchronized"], json!(false));
    assert_eq!(value["sample"]["stratum"], 2);
    assert_eq!(value["server"]["name"], "0.pool.ntp.org");
}

/// The offline path: the daemon runs, nothing resolved, no reply ever.
/// The classification is degraded — and it is ONLY a classification;
/// retries are timesyncd's pinned 30-second policy and nothing in this
/// module has a handle with which to stop them.
#[test]
fn no_usable_server_is_offline_degraded() {
    let evidence = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: Some(false),
        ..TimesyncEvidence::default()
    };
    assert_eq!(classify(&evidence), SyncStatus::OfflineDegraded);
}

/// A source that answers garbage outranks "polling": leap 3 and the
/// out-of-range strata each mark the reply unusable on its own.
#[test]
fn an_unusable_reply_is_invalid_source() {
    for bad in [sample(3, 2, 0.0), sample(0, 0, 0.0), sample(0, 16, 0.0)] {
        let evidence = TimesyncEvidence {
            service_reachable: true,
            ntp_synchronized: Some(false),
            server_name: Some("bad.example".to_string()),
            sample: Some(bad),
            ..TimesyncEvidence::default()
        };
        assert_eq!(classify(&evidence), SyncStatus::InvalidSource, "{bad:?}");
    }
    // And a healthy reply is not: the positive control. The kernel bit
    // is stated rather than left at its default, because the state this
    // control names is one of the three that rest on it having been read
    //  -- omitting it would make the control assert `unknown`
    // for a reason that has nothing to do with the sample.
    let evidence = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: Some(false),
        server_name: Some("good.example".to_string()),
        sample: Some(sample(0, 2, 0.0)),
        ..TimesyncEvidence::default()
    };
    assert_eq!(classify(&evidence), SyncStatus::Polling);
}

/// No observer contact at all is `unknown`, not `offline-degraded`:
/// claiming network evidence nobody has would send the operator to the
/// wrong cable.
#[test]
fn an_unreachable_daemon_is_unknown() {
    assert_eq!(classify(&TimesyncEvidence::default()), SyncStatus::Unknown);
}

/// timedate1's property read did not answer. `None` and
/// `Some(false)` are different facts — one is a device nobody could
/// query, the other a device that was queried and is not synchronized —
/// and every state below `invalid-source` rests on that bit: the first
/// asserts it, the other two assert its absence. So an unread bit is
/// `unknown`, and the states that would claim something about the clock
/// are unreachable without it.
#[test]
fn an_unread_kernel_bit_is_unknown_not_a_state_that_asserts_one() {
    // What would have been `polling`.
    let polling_shaped = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: None,
        server_name: Some("0.pool.ntp.org".to_string()),
        server_address: Some("192.0.2.7".to_string()),
        sample: Some(sample(0, 2, 0.004)),
    };
    assert_eq!(classify(&polling_shaped), SyncStatus::Unknown);

    // What would have been `offline-degraded`.
    let degraded_shaped = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: None,
        ..TimesyncEvidence::default()
    };
    assert_eq!(classify(&degraded_shaped), SyncStatus::Unknown);

    // The source's own verdict still stands: it never rested on the bit.
    let unusable = TimesyncEvidence {
        sample: Some(sample(3, 2, 0.0)),
        ..polling_shaped.clone()
    };
    assert_eq!(classify(&unusable), SyncStatus::InvalidSource);

    // The controls: the same two shapes with the bit actually read.
    assert_eq!(
        classify(&TimesyncEvidence {
            ntp_synchronized: Some(false),
            ..polling_shaped
        }),
        SyncStatus::Polling
    );
    assert_eq!(
        classify(&TimesyncEvidence {
            ntp_synchronized: Some(false),
            ..degraded_shaped
        }),
        SyncStatus::OfflineDegraded
    );
}

/// The raw signal stays faithfully absent, and the `detail` says WHICH
/// read did not answer.
#[test]
fn the_unread_signal_stays_absent_and_names_itself() {
    let evidence = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: None,
        server_name: Some("0.pool.ntp.org".to_string()),
        server_address: Some("192.0.2.7".to_string()),
        sample: Some(sample(0, 2, 0.004)),
    };
    let value = status_json(&evidence);
    assert_eq!(value["status"], "unknown");
    assert_eq!(value.get("synchronized"), None);
    // Observed evidence is still reported; it is simply not a state.
    assert_eq!(value["server"]["name"], "0.pool.ntp.org");
    assert_eq!(value["sample"]["stratum"], 2);

    // The two ways this state is reached name different services, so an
    // operator is not sent to look at a daemon that answered.
    let detail = value["detail"].as_str().expect("unknown carries a detail");
    assert!(detail.contains("timedated"), "{detail}");
    assert!(!detail.contains("timesyncd"), "{detail}");
    let off_bus = status_json(&TimesyncEvidence::default());
    assert!(
        off_bus["detail"]
            .as_str()
            .expect("unknown carries a detail")
            .contains("timesyncd")
    );
}

/// The step-versus-drift distinction, at timesyncd's own boundary: an
/// offset past 0.4 s is a step, inside it a slew, and no sample means no
/// `correction` member at all rather than a guessed one.
#[test]
fn the_correction_member_tells_a_step_from_a_slew() {
    let stepped = TimesyncEvidence {
        service_reachable: true,
        server_name: Some("s".to_string()),
        sample: Some(sample(0, 2, -3.2)),
        ..TimesyncEvidence::default()
    };
    assert_eq!(status_json(&stepped)["sample"]["correction"], "step");

    let slewed = TimesyncEvidence {
        sample: Some(sample(0, 2, 0.05)),
        ..stepped
    };
    assert_eq!(status_json(&slewed)["sample"]["correction"], "slew");

    let no_reply = TimesyncEvidence {
        service_reachable: true,
        ..TimesyncEvidence::default()
    };
    assert_eq!(status_json(&no_reply).get("sample"), None);
}

/// The served shape, key by key: the consumer is apid's time status route
/// in another crate and reads these names off the bus string.
#[test]
fn the_status_json_carries_the_documented_members() {
    let evidence = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: Some(true),
        server_name: Some("0.pool.ntp.org".to_string()),
        server_address: Some("192.0.2.7".to_string()),
        sample: Some(sample(0, 2, 0.012)),
    };
    let value = status_json(&evidence);
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["sample", "server", "status", "synchronized"]);
    assert_eq!(value["status"], "synchronized");
    assert_eq!(value["server"]["name"], "0.pool.ntp.org");
    assert_eq!(value["sample"]["stratum"], 2);

    // The unknown case names itself instead of dressing up as evidence.
    let unknown = status_json(&TimesyncEvidence::default());
    assert_eq!(unknown["status"], "unknown");
    assert!(unknown["detail"].as_str().unwrap().contains("timesyncd"));
}

/// The `NTPMessage` decode over a literal field slice: the offset is the
/// standard four-timestamp calculation and the indexes are the ABI's.
#[test]
fn an_ntp_message_structure_decodes_into_a_sample() {
    // origin 1000, receive 1600, transmit 1700, destination 1100 (µs):
    // offset = ((1600-1000)+(1700-1100))/2 = 600 µs.
    let fields: Vec<Value<'_>> = vec![
        Value::U32(0),            // leap
        Value::U32(4),            // version
        Value::U32(4),            // mode
        Value::U32(2),            // stratum
        Value::I32(-24),          // precision
        Value::U64(0),            // root delay
        Value::U64(0),            // root dispersion
        Value::new(vec![0u8; 4]), // reference id
        Value::U64(1_000),        // origin
        Value::U64(1_600),        // receive
        Value::U64(1_700),        // transmit
        Value::U64(1_100),        // destination
        Value::Bool(false),       // spike
        Value::U64(7),            // packet count
        Value::U64(0),            // jitter
    ];
    let sample = parse_ntp_message(&fields).expect("the documented shape decodes");
    assert_eq!(sample.leap, 0);
    assert_eq!(sample.stratum, 2);
    assert!(!sample.spike);
    assert_eq!(sample.packet_count, 7);
    assert!((sample.offset_seconds - 0.0006).abs() < 1e-9);

    // A shape that is not the ABI's is absent evidence, not a panic.
    assert_eq!(parse_ntp_message(&[Value::U32(1)]), None);
    assert_eq!(parse_ntp_message(&[]), None);
}

#[test]
fn a_server_address_formats_by_family() {
    assert_eq!(
        format_server_address(2, &[192, 0, 2, 7]).as_deref(),
        Some("192.0.2.7")
    );
    let mut v6 = vec![0u8; 16];
    v6[15] = 1;
    assert_eq!(format_server_address(10, &v6).as_deref(), Some("::1"));
    assert_eq!(format_server_address(2, &[1, 2]), None);
    assert_eq!(format_server_address(99, &[0; 4]), None);
}

/// The second limb, and the case it exists for: a device with
/// no STATE bind.
#[test]
fn the_saved_floor_advances_only_when_the_file_moved_after_this_boot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = dir.path().join("clock");
    // An hour into this boot, which is what /proc/uptime's first field
    // carries; the boot instant the comparison uses is derived from it,
    // so moving the uptime is how a test moves the boot rather than the
    // file.
    let an_hour_in = dir.path().join("uptime-3600");
    std::fs::write(&an_hour_in, "3600.00 7200.00\n").expect("seed uptime");
    // A machine that booted just now: a floor file already on the disk
    // was last written before this boot, not during it.
    let just_booted = dir.path().join("uptime-0");
    std::fs::write(&just_booted, "0.00 0.00\n").expect("seed uptime");

    assert!(
        !saved_floor_advanced(&clock, &an_hour_in),
        "a device with no STATE bind has no floor file, and an absent \
         signal is not an advance"
    );

    std::fs::write(&clock, b"").expect("seed the floor");
    assert!(
        !saved_floor_advanced(&clock, &just_booted),
        "STATE is bound but nothing has written the floor since boot"
    );
    assert!(
        saved_floor_advanced(&clock, &an_hour_in),
        "the floor moved during this boot: the mechanism is alive"
    );

    // An unreadable uptime is an unread signal, and the closed side here
    // is deferring the install.
    assert!(!saved_floor_advanced(&clock, &dir.path().join("absent")));
}

/// The predicate the automatic install is gated on, and the sentence an
/// operator is given when it refuses.
#[test]
fn the_clock_is_believed_on_either_limb_and_the_refusal_names_both() {
    let degraded = ClockTrust {
        status: Some(SyncStatus::OfflineDegraded),
        floor_advanced: false,
    };
    assert!(!degraded.believed());
    let reason = degraded
        .untrusted_reason()
        .expect("an unbelieved clock says why");
    assert!(reason.contains("offline-degraded"), "{reason}");
    assert!(reason.contains(SAVED_CLOCK_PATH), "{reason}");
    assert!(
        reason.contains("checks and fetches are unaffected"),
        "the refusal must not read as a device that stopped discovering \
         updates: {reason}"
    );

    for believed in [
        ClockTrust {
            status: Some(SyncStatus::Synchronized),
            floor_advanced: false,
        },
        ClockTrust {
            status: Some(SyncStatus::OfflineDegraded),
            floor_advanced: true,
        },
    ] {
        assert!(believed.believed(), "{believed:?}");
        assert_eq!(believed.untrusted_reason(), None);
    }

    // Unread evidence is not a claim: a daemon with no observer at all
    // and no floor does not get to call its clock believable.
    let unread = ClockTrust {
        status: None,
        floor_advanced: false,
    };
    assert!(!unread.believed());
    assert!(
        unread
            .untrusted_reason()
            .expect("says why")
            .contains("`unknown`")
    );
}
