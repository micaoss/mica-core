use super::*;

/// `mica help` lists every operator verb once, in full words, and never
/// `internal`, an abbreviation or another spelling.
#[test]
fn the_help_lists_every_verb_in_full_words() {
    let text = help();
    for (verb, _) in VERBS {
        assert_eq!(
            text.lines()
                .filter(|line| line.split_whitespace().next() == Some(verb))
                .count(),
            1,
            "{verb}: {text}"
        );
    }
    for absent in [
        "internal",
        "ctr",
        "ps",
        "diag",
        "diagnostics",
        "daemon",
        "apid",
    ] {
        assert!(!listed(absent), "{absent}");
        assert!(
            !text
                .lines()
                .any(|line| line.split_whitespace().next() == Some(absent)),
            "{absent}: {text}"
        );
    }
}

fn fdo(error: zbus::fdo::Error) -> zbus::Error {
    zbus::Error::FDO(Box::new(error))
}

/// A bus error's exit code: the service not there is 3, a credential
/// refusal 4, any other refusal 1, with the code a script reads.
#[test]
fn bus_errors_map_onto_the_exit_codes() {
    for (error, exit, code) in [
        (
            fdo(zbus::fdo::Error::ServiceUnknown("no micad".into())),
            EXIT_UNREACHABLE,
            "ServiceUnknown",
        ),
        (
            fdo(zbus::fdo::Error::NoReply("gone".into())),
            EXIT_UNREACHABLE,
            "NoReply",
        ),
        (
            fdo(zbus::fdo::Error::AccessDenied("an update runs".into())),
            EXIT_FAILED,
            "AccessDenied",
        ),
        (
            fdo(zbus::fdo::Error::Failed(
                r#"{"code":"password_incorrect","message":"the password is incorrect"}"#.into(),
            )),
            EXIT_CREDENTIAL,
            "password_incorrect",
        ),
        (
            fdo(zbus::fdo::Error::Failed(
                r#"{"code":"login_throttled","message":"try again in 30 s"}"#.into(),
            )),
            EXIT_CREDENTIAL,
            "login_throttled",
        ),
        (
            fdo(zbus::fdo::Error::Failed(
                r#"{"code":"update_in_progress","message":"an update runs"}"#.into(),
            )),
            EXIT_FAILED,
            "update_in_progress",
        ),
    ] {
        let failure = bus_failure(&error);
        assert_eq!(
            (failure.exit, failure.code.as_str()),
            (exit, code),
            "{error}"
        );
    }
    let refused = bus_failure(&fdo(zbus::fdo::Error::Failed(
        r#"{"code":"password_incorrect","message":"the password is incorrect"}"#.into(),
    )));
    assert_eq!(refused.message, "the password is incorrect");
}

/// The exit code survives the context a command adds, and an error that is
/// neither micad's nor the bus's is a failure.
#[test]
fn an_error_is_classified_through_its_context() {
    let error = bus_error(fdo(zbus::fdo::Error::ServiceUnknown("no micad".into())))
        .context("read the units");
    let failure = classify(&error);
    assert_eq!(failure.exit, EXIT_UNREACHABLE);
    assert!(
        failure.message.starts_with("read the units: "),
        "{}",
        failure.message
    );

    let failure = classify(&anyhow::anyhow!("no such file"));
    assert_eq!(
        (failure.exit, failure.code.as_str()),
        (EXIT_FAILED, "failed")
    );

    // A refusal the command printed itself adds nothing more.
    let failure = classify(&Failure::printed("refused").into());
    assert_eq!((failure.exit, failure.message.as_str()), (EXIT_FAILED, ""));
}

#[test]
fn credential_codes_exit_4_and_others_1() {
    for code in CREDENTIAL_CODES {
        assert_eq!(exit_for(code), EXIT_CREDENTIAL, "{code}");
    }
    assert_eq!(exit_for("validation_failed"), EXIT_FAILED);
}
