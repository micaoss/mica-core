use super::*;

fn status() -> DmStatus {
    DmStatus {
        device: 0xfd00,
        name: "mica-root".into(),
        uuid: "CRYPT-VERITY-owned".into(),
        targets: 2,
        open_count: 0,
        event: 7,
    }
}
fn reply(table: bool) -> DmBuffer {
    let mut b = DmBuffer::request(4096, 0xfd00, None, table).unwrap();
    b.header.version = [4, 48, 0];
    b.header.data_size = 305;
    b.header.flags = DM_READONLY | DM_ACTIVE | if table { DM_TABLE } else { 0 };
    b.header.target_count = 2;
    b.header.event_nr = 7;
    put_string(&mut b.header.name, "mica-root").unwrap();
    put_string(&mut b.header.uuid, "CRYPT-VERITY-owned").unwrap();
    b
}
fn targets(b: &mut DmBuffer) {
    let mut at: usize = 0;
    for (sector, parameters) in [(0_u64, "1 7:0 7:0"), (8, "1 7:1 7:1 extended")] {
        let used = at + 40 + parameters.len() + 1;
        let next = used.next_multiple_of(8);
        b.data[at..at + 8].copy_from_slice(&sector.to_ne_bytes());
        b.data[at + 8..at + 16].copy_from_slice(&8_u64.to_ne_bytes());
        b.data[at + 20..at + 24].copy_from_slice(&(next as u32).to_ne_bytes());
        b.data[at + 24..at + 31].copy_from_slice(b"verity\0");
        b.data[at + 40..used - 1].copy_from_slice(parameters.as_bytes());
        b.header.data_size = (312 + used) as u32;
        at = next;
    }
}
#[test]
fn dm_status_and_table_use_kernel_lengths_and_absolute_next_offsets() {
    let mut b = reply(false);
    assert_eq!(parse_dm_status(&b, 4096, false).unwrap(), status());
    b = reply(true);
    targets(&mut b);
    let t = parse_dm_table(&b, 4096, &status()).unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t[1].sector, 8);
    assert_eq!(t[1].length, 8);
    assert_eq!(t[1].parameters, "1 7:1 7:1 extended");
    assert!(!b.header.data_size.is_multiple_of(8));
}
#[test]
fn dm_readonly_flag_is_required_in_status_and_table() {
    for table in [false, true] {
        let mut b = reply(table);
        assert!(parse_dm_status(&b, 4096, table).is_ok());
        b.header.flags &= !DM_READONLY;
        assert_eq!(
            parse_dm_status(&b, 4096, table).unwrap_err(),
            io::Errno::PROTO
        );
    }
}

#[test]
fn dm_status_rejects_non_kernel_reply_lengths() {
    for size in [304, 306, 312, 313] {
        let mut b = reply(false);
        b.header.data_size = size;
        assert!(parse_dm_status(&b, 312, false).is_err(), "size={size}");
    }
}

#[test]
fn dm_refuses_malformed_header_flags_counts_and_identity() {
    for change in 0..13 {
        let mut b = reply(true);
        targets(&mut b);
        match change {
            0 => b.header.version[0] = 5,
            1 => b.header.data_size = 304,
            2 => b.header.data_size = 65537,
            3 => b.header.data_start = 311,
            4 => b.header.flags |= 1 << 17,
            5 => b.header.flags |= 1 << 6,
            6 => b.header.flags &= !DM_TABLE,
            7 => b.header.target_count = 0,
            8 => b.header.target_count = 17,
            9 => b.header.name.fill(b'x'),
            10 => b.header.uuid.fill(b'x'),
            11 => b.header.dev = 0xfd01,
            _ => b.header.open_count = -1,
        }
        assert!(
            parse_dm_table(&b, 4096, &status()).is_err(),
            "change={change}"
        );
    }
    for change in 0..4 {
        let mut b = reply(true);
        targets(&mut b);
        match change {
            0 => b.header.name[0] = b'x',
            1 => b.header.uuid[0] = b'x',
            2 => b.header.event_nr += 1,
            _ => b.header.target_count = 1,
        }
        assert!(parse_dm_table(&b, 4096, &status()).is_err());
    }
}
#[test]
fn dm_refuses_truncated_unterminated_overflowing_and_incomplete_targets() {
    for change in 0..12 {
        let mut b = reply(true);
        targets(&mut b);
        let second = u32::from_ne_bytes(b.data[20..24].try_into().unwrap()) as usize;
        let end = b.header.data_size as usize - 312;
        match change {
            0 => b.header.data_size = 312 + 39,
            1 => b.data[24..40].fill(b'x'),
            2 => b.data[40..second].fill(b'x'),
            3 => b.data[20..24].copy_from_slice(&0_u32.to_ne_bytes()),
            4 => b.data[20..24].copy_from_slice(&41_u32.to_ne_bytes()),
            5 => b.data[20..24].copy_from_slice(&u32::MAX.to_ne_bytes()),
            6 => b.data[second + 20..second + 24].copy_from_slice(&64_u32.to_ne_bytes()),
            7 => b.data[second..second + 8].copy_from_slice(&9_u64.to_ne_bytes()),
            8 => b.data[8..16].copy_from_slice(&u64::MAX.to_ne_bytes()),
            9 => b.data[16..20].copy_from_slice(&1_i32.to_ne_bytes()),
            10 => b.data[second + 40..end].fill(b'x'),
            _ => b.data[8..16].fill(0),
        }
        assert!(
            parse_dm_table(&b, 4096, &status()).is_err(),
            "change={change}"
        );
    }
}
#[test]
fn dm_buffer_growth_and_interruptions_are_bounded() {
    let mut calls = Vec::new();
    let result = read_dm_table(&status(), &mut |kind, b| {
        assert_eq!(kind, DmCommand::Table);
        calls.push(b.header.data_size);
        *b = reply(true);
        b.header.flags |= DM_FULL;
        Ok(())
    });
    assert_eq!(result.unwrap_err(), io::Errno::OVERFLOW);
    assert_eq!(calls, [1024, 4096, 16384, 65536]);
    let mut count = 0;
    assert_eq!(
        read_dm_status(0xfd00, &mut |_, _| {
            count += 1;
            Err(io::Errno::INTR)
        })
        .unwrap_err(),
        io::Errno::INTR
    );
    assert_eq!(count, 4);
    count = 0;
    assert_eq!(
        read_dm_table(&status(), &mut |_, _| {
            count += 1;
            Err(io::Errno::INTR)
        })
        .unwrap_err(),
        io::Errno::INTR
    );
    assert_eq!(count, 4);
}
#[test]
fn dm_remove_refuses_busy_identity_races_and_bad_replies() {
    for case in 0..5 {
        let mut calls = 0;
        let result = remove_dm(&status(), &mut |kind, b| {
            calls += 1;
            if kind == DmCommand::Status {
                *b = reply(false);
                if case == 0 {
                    b.header.open_count = 1;
                }
                if case == 1 {
                    b.header.uuid[0] = b'x';
                }
            } else {
                assert_eq!(kind, DmCommand::Remove);
                assert_eq!(b.header.dev, 0);
                assert_eq!(b.header.flags, 0);
                assert!(b.header.name.iter().all(|c| *c == 0));
                assert_eq!(dm_string(&b.header.uuid).unwrap(), "CRYPT-VERITY-owned");
                if case == 2 {
                    return Err(io::Errno::BUSY);
                }
                *b = reply(false);
                b.header.data_size = 305;
                b.header.dev = 0;
                b.header.flags = 1 << 13;
                b.header.target_count = 0;
                b.header.event_nr = 0;
                if case == 3 {
                    b.header.uuid[0] = b'x';
                }
            }
            Ok(())
        });
        assert_eq!(result.is_ok(), case == 4);
        assert_eq!(calls, if case < 2 { 1 } else { 2 });
    }
}
