//! The board's presence declaration.

use crate::routes::{MarkerPresence, NoPresence, Presence};
use serde_json::json;

use super::*;

/// A reader over a board that declares `declaration`, or over one that ships
/// none at all, with `marker` as its assertion.
pub(super) fn shipped(
    declaration: Option<&str>,
    marker: Option<&str>,
) -> (TempDir, MarkerPresence) {
    let dir = TempDir::new().unwrap();
    let declaration_path = dir.path().join("recovery-actions.conf");
    let marker_path = dir.path().join("presence");
    if let Some(text) = declaration {
        std::fs::write(&declaration_path, text).unwrap();
    }
    if let Some(text) = marker {
        std::fs::write(&marker_path, text).unwrap();
    }
    (dir, MarkerPresence::at(marker_path, declaration_path))
}

/// A marker standing for `seconds` more.
pub(super) fn marker(mechanism: &str, seconds: u64) -> String {
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + seconds;
    json!({ "mechanism": mechanism, "channel": "/dev/tty0", "expires": expires }).to_string()
}

/// **The shipped state of both mica boards.** A board that declares no physical
/// recovery action refuses presence, and the refusal says so — an operator
/// told only "none is asserted" would go looking for a door that does not
/// exist on this hardware.
#[test]
pub(super) fn a_board_that_declares_no_action_refuses_and_says_which() {
    for declaration in [Some(DECLARES_NONE), None] {
        let (_dir, presence) = shipped(declaration, None);
        let refusal = presence
            .assert()
            .err()
            .expect("presence is not established");
        assert_eq!(refusal, NoPresence::BoardDeclaresNone);
        let message = format!("{refusal:?}");
        assert_eq!(message, "BoardDeclaresNone");
    }

    // And a marker cannot buy presence on such a board: the declaration is
    // read first, so a file left in /run by anything at all reaches nothing.
    let (_dir, presence) = shipped(Some(DECLARES_NONE), Some(&marker(FIXTURE_MECHANISM, 600)));
    assert_eq!(presence.assert().err(), Some(NoPresence::BoardDeclaresNone));
}

/// Every state the shipped reader can be in, enumerated: exactly one of them
/// establishes presence, and each refusal is reached.
#[test]
pub(super) fn the_shipped_reader_establishes_presence_only_for_a_declared_live_assertion() {
    /// One reader state: what it is called, what the board declares, what the
    /// marker holds, and the refusal it must produce (`None` = presence).
    struct Case {
        label: &'static str,
        declaration: Option<&'static str>,
        assertion: Option<String>,
        want: Option<NoPresence>,
    }
    fn case(
        label: &'static str,
        declaration: Option<&'static str>,
        assertion: Option<String>,
        want: Option<NoPresence>,
    ) -> Case {
        Case {
            label,
            declaration,
            assertion,
            want,
        }
    }

    let cases = [
        case(
            "a declared mechanism, still standing",
            Some(DECLARES_ONE),
            Some(marker(FIXTURE_MECHANISM, 600)),
            None,
        ),
        case(
            "no assertion at all",
            Some(DECLARES_ONE),
            None,
            Some(NoPresence::Absent),
        ),
        case(
            "an assertion whose window has passed",
            Some(DECLARES_ONE),
            Some(
                json!({
                    "mechanism": FIXTURE_MECHANISM,
                    "channel": "/dev/tty0",
                    "expires": 1_u64,
                })
                .to_string(),
            ),
            Some(NoPresence::Expired),
        ),
        case(
            "an assertion with no deadline",
            Some(DECLARES_ONE),
            Some(json!({ "mechanism": FIXTURE_MECHANISM, "channel": "/dev/tty0" }).to_string()),
            Some(NoPresence::Malformed),
        ),
        case(
            "an assertion that is not JSON",
            Some(DECLARES_ONE),
            Some("not an assertion".to_string()),
            Some(NoPresence::Malformed),
        ),
        case(
            "a mechanism no declared action uses",
            Some(DECLARES_ONE),
            Some(marker("some-other-door", 600)),
            Some(NoPresence::UnknownMechanism(vec![
                FIXTURE_MECHANISM.to_string(),
            ])),
        ),
        case(
            "a declaration this build cannot read",
            Some("BOARD_RECOVERY_ACTIONS=\"A\"\nRECOVERY_A_INTENT=recovery\n"),
            Some(marker(FIXTURE_MECHANISM, 600)),
            Some(NoPresence::DeclarationUnreadable),
        ),
        case(
            "a board that declares none",
            Some(DECLARES_NONE),
            None,
            Some(NoPresence::BoardDeclaresNone),
        ),
    ];

    let mut established = 0_usize;
    let mut refusals = std::collections::BTreeSet::new();
    for Case {
        label,
        declaration,
        assertion,
        want,
    } in cases
    {
        let (_dir, presence) = shipped(declaration, assertion.as_deref());
        match (presence.assert(), &want) {
            (Ok(_), None) => established += 1,
            (Err(got), Some(expected)) => {
                assert_eq!(&got, expected, "{label}");
                refusals.insert(format!("{got:?}"));
            }
            (got, want) => panic!("{label}: got ok={}, wanted {want:?}", got.is_ok()),
        }
    }
    assert_eq!(established, 1, "exactly one input establishes presence");
    assert_eq!(
        refusals.len(),
        6,
        "every refusal the reader can produce must be reached: {refusals:?}"
    );
}
