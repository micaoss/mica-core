use super::*;

/// A board that declares one action, spelled the way a BSP renders it.
const ONE_ACTION: &str = "\
# rendered from boards/example/board.env
BOARD_RECOVERY_ACTIONS=\"GRUB_RECOVERY_ENTRY\"
RECOVERY_GRUB_RECOVERY_ENTRY_INTENT=recovery
RECOVERY_GRUB_RECOVERY_ENTRY_MECHANISM=grub-entry
RECOVERY_GRUB_RECOVERY_ENTRY_CHANNEL=/dev/tty0
RECOVERY_GRUB_RECOVERY_ENTRY_TIER=none
";

fn one_action() -> Declaration {
    Declaration::parse(ONE_ACTION)
}

#[test]
fn a_declared_action_maps_from_its_intent() {
    let declaration = one_action();
    let action = declaration.map_intent("recovery").expect("it maps");
    assert_eq!(action.name, "GRUB_RECOVERY_ENTRY");
    assert_eq!(action.mechanism, "grub-entry");
    assert_eq!(action.channel, PathBuf::from("/dev/tty0"));
    assert_eq!(action.tier, None);
    assert!(declaration.declares_mechanism("grub-entry"));
    assert_eq!(declaration.mechanisms(), vec!["grub-entry"]);
}

#[test]
fn every_tier_a_board_may_name_maps_to_the_tier_the_flows_run() {
    for (declared, want) in [
        (TIER_NONE, None),
        ("configuration", Some(ResetTier::Configuration)),
        ("application-data", Some(ResetTier::ApplicationData)),
        ("full-factory", Some(ResetTier::FullFactory)),
    ] {
        let text = ONE_ACTION.replace("_TIER=none", &format!("_TIER={declared}"));
        let declaration = Declaration::parse(&text);
        assert_eq!(
            declaration.map_intent("recovery").expect("it maps").tier,
            want,
            "declared tier {declared}"
        );
    }
}

#[test]
fn an_absent_declaration_is_a_board_that_declares_none() {
    let dir = tempfile::tempdir().unwrap();
    let declaration = Declaration::read(&dir.path().join("nothing-here.conf"));
    assert_eq!(declaration, Declaration::None);
    assert_eq!(
        declaration.map_intent("recovery"),
        Err(NoAction::BoardDeclaresNone)
    );
}

#[test]
fn an_empty_list_is_a_board_that_declares_none() {
    // The shipped spelling on both mica boards: the key is declared, and
    // what it declares is that there is no action.
    let declaration = Declaration::parse("BOARD_RECOVERY_ACTIONS=\"\"\n");
    assert_eq!(declaration, Declaration::None);
    assert_eq!(
        declaration.map_intent("anything"),
        Err(NoAction::BoardDeclaresNone)
    );
    assert!(declaration.mechanisms().is_empty());
}

#[test]
fn an_intent_no_action_declares_maps_to_nothing() {
    assert_eq!(
        one_action().map_intent("factory-reset"),
        Err(NoAction::UnknownIntent)
    );
    assert!(!one_action().declares_mechanism("uboot-menu"));
}

/// Every way a declaration can be malformed fails CLOSED, and each is
/// reached: a case that stopped being malformed would stop being tested
/// here rather than start passing somewhere else.
#[test]
fn every_malformed_declaration_maps_no_intent() {
    let cases: &[(&str, &str)] = &[
        ("no list at all", "RECOVERY_X_INTENT=recovery\n"),
        (
            "a name that is not a name",
            "BOARD_RECOVERY_ACTIONS=\"grub entry\"\n",
        ),
        (
            "a missing key",
            "BOARD_RECOVERY_ACTIONS=\"A\"\nRECOVERY_A_INTENT=recovery\n",
        ),
        (
            "a key declared empty",
            &ONE_ACTION.replace("_MECHANISM=grub-entry", "_MECHANISM=\"\""),
        ),
        (
            "an intent that is not a command-line token",
            &ONE_ACTION.replace("_INTENT=recovery", "_INTENT=Recovery!"),
        ),
        (
            "a mechanism that is not a mechanism name",
            &ONE_ACTION.replace("_MECHANISM=grub-entry", "_MECHANISM=GRUB_ENTRY"),
        ),
        (
            "a channel that is not a device",
            &ONE_ACTION.replace("_CHANNEL=/dev/tty0", "_CHANNEL=/var/lib/mica/console"),
        ),
        (
            "a tier no flow implements",
            &ONE_ACTION.replace("_TIER=none", "_TIER=secure-wipe"),
        ),
        (
            "a line that is not an assignment",
            &format!("{ONE_ACTION}source /etc/passwd\n"),
        ),
        (
            "a value a shell would expand",
            &ONE_ACTION.replace("_INTENT=recovery", "_INTENT=$(id)"),
        ),
        (
            "two actions sharing an intent",
            &format!(
                "{}{}",
                ONE_ACTION.replace(
                    "BOARD_RECOVERY_ACTIONS=\"GRUB_RECOVERY_ENTRY\"",
                    "BOARD_RECOVERY_ACTIONS=\"GRUB_RECOVERY_ENTRY BUTTON\"",
                ),
                "RECOVERY_BUTTON_INTENT=recovery\nRECOVERY_BUTTON_MECHANISM=button\n\
                 RECOVERY_BUTTON_CHANNEL=/dev/tty0\nRECOVERY_BUTTON_TIER=none\n",
            ),
        ),
        (
            "two actions sharing a mechanism",
            &format!(
                "{}{}",
                ONE_ACTION.replace(
                    "BOARD_RECOVERY_ACTIONS=\"GRUB_RECOVERY_ENTRY\"",
                    "BOARD_RECOVERY_ACTIONS=\"GRUB_RECOVERY_ENTRY BUTTON\"",
                ),
                "RECOVERY_BUTTON_INTENT=factory\nRECOVERY_BUTTON_MECHANISM=grub-entry\n\
                 RECOVERY_BUTTON_CHANNEL=/dev/tty0\nRECOVERY_BUTTON_TIER=none\n",
            ),
        ),
    ];
    for (label, text) in cases {
        let declaration = Declaration::parse(text);
        let Declaration::Unreadable(reason) = &declaration else {
            panic!("{label} was accepted: {declaration:?}");
        };
        assert!(!reason.is_empty(), "{label} refused without a reason");
        assert_eq!(
            declaration.map_intent("recovery"),
            Err(NoAction::Unreadable(reason.clone())),
            "{label}",
        );
        assert!(
            declaration.actions().is_empty() && declaration.mechanisms().is_empty(),
            "{label} left an action reachable",
        );
    }
}

#[test]
fn an_intent_is_read_off_the_command_line_and_only_from_the_parameter() {
    assert_eq!(
        intent_from_cmdline("root=/dev/mmcblk0p6 mica.recovery=recovery quiet"),
        Ok(Some("recovery"))
    );
    assert_eq!(intent_from_cmdline("root=/dev/mmcblk0p6 quiet"), Ok(None));
    // A parameter that merely contains the name is not the parameter.
    assert_eq!(intent_from_cmdline("not.mica.recovery=recovery"), Ok(None));
    assert_eq!(intent_from_cmdline("mica.recoveryx=recovery"), Ok(None));
}

#[test]
fn a_malformed_command_line_yields_no_intent_at_all() {
    for cmdline in [
        "mica.recovery=recovery mica.recovery=factory",
        "mica.recovery=",
        "quiet mica.recovery",
    ] {
        assert!(
            intent_from_cmdline(cmdline).is_err(),
            "`{cmdline}` must not produce an intent"
        );
    }
}
