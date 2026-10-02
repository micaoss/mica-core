//! Applying a document, idempotently, by its digest.

use micad_settings::ProvisioningState;
use micad_settings::Settings;
use std::fs;
use tempfile::TempDir;

use super::*;

// The happy path, end to end: every section lands where it belongs, the
// record names the document, and the tree really reached STATE.
#[test]
pub(super) fn a_boot_document_is_applied_and_persisted() {
    let dir = TempDir::new().expect("tempdir");
    let (store, settings, outcome) = import_body(dir.path(), Source::Boot, &full_document());

    let Outcome::Applied {
        source,
        version,
        digest,
    } = outcome
    else {
        panic!("expected an applied document, got {outcome:?}");
    };
    assert_eq!(source, Source::Boot);
    assert_eq!(version, DOCUMENT_VERSION);

    assert_eq!(
        settings.provisioning.device_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert!(
        settings
            .access
            .web_admin
            .as_ref()
            .expect("the admin credential is set")
            .password_hash
            .starts_with("$argon2id$"),
        "the bootstrap password must be stored as an Argon2id hash"
    );
    assert_eq!(settings.access.ssh.authorized_keys.len(), 1);
    assert!(settings.network.get("eth0").expect("eth0 configured").dhcp);
    assert!(settings.wifi.client.enabled);
    assert_eq!(settings.wifi.client.networks[0].ssid, "site-ap");
    assert_eq!(settings.time.timezone, "Europe/Berlin");
    assert_eq!(settings.time.ntp.servers.len(), 2);

    let record = settings
        .provisioning
        .document
        .as_ref()
        .expect("a document record");
    assert_eq!(record.applied_version, Some(DOCUMENT_VERSION));
    assert_eq!(record.applied_digest.as_deref(), Some(digest.as_str()));
    let import_record = record.last_import.as_ref().expect("an import record");
    assert_eq!(import_record.source, "boot");
    assert_eq!(import_record.outcome, "applied");
    assert_eq!(import_record.reason, None);

    // The one save really happened, and it holds the same tree.
    assert_eq!(store.load().expect("reload"), settings);

    // Nothing seeding owns was moved: this ran before seeding, so the
    // state is still pending and the generation still zero.
    assert_eq!(settings.provisioning.state, ProvisioningState::Pending);
    assert_eq!(settings.provisioning.seeded_generation, 0);
}

// The idempotence criterion, asserted on the bytes: a second import writes
// NOTHING at all, so a device left with the medium in place does not burn
// a flash write per boot.
#[test]
pub(super) fn re_applying_an_identical_document_is_a_proven_no_op() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(dir.path(), Source::Boot, &full_document());
    let mut settings = Settings::default();

    let first = import(&store, &mut settings, &root).expect("first import");
    assert!(matches!(first, Outcome::Applied { .. }), "{first:?}");
    let first_tree = settings.clone();
    let first_bytes = fs::read(settings_path(dir.path())).expect("read settings file");

    // Reload from STATE, exactly as the next boot would.
    let mut second = store.load().expect("reload");
    assert_eq!(second, first_tree);
    let outcome = import(&store, &mut second, &root).expect("second import");
    let Outcome::Unchanged { source, digest } = outcome else {
        panic!("expected `unchanged`, got {outcome:?}");
    };
    assert_eq!(source, Source::Boot);
    assert_eq!(
        Some(digest.as_str()),
        first_tree
            .provisioning
            .document
            .as_ref()
            .and_then(|record| record.applied_digest.as_deref())
    );

    // NOTHING the document names moved. The one thing a second import does
    // write is the attempt record, so it is excluded here and asserted
    // directly below — the tree is otherwise the tree the first import
    // produced, byte for byte in the settings the document owns.
    let mut second_without_record = second.clone();
    let mut first_without_record = first_tree;
    for tree in [&mut second_without_record, &mut first_without_record] {
        if let Some(document) = tree.provisioning.document.as_mut() {
            document.last_import = None;
        }
    }
    assert_eq!(
        second_without_record, first_without_record,
        "a second import must move nothing the document names"
    );
    let attempt = second
        .provisioning
        .document
        .as_ref()
        .and_then(|record| record.last_import.as_ref())
        .expect("an import record");
    assert_eq!(attempt.outcome, "unchanged");
    assert_eq!(attempt.reason, None);
    assert_ne!(
        fs::read(settings_path(dir.path())).expect("re-read settings file"),
        first_bytes,
        "the second import records that it found nothing to do"
    );

    // And a THIRD import writes nothing at all: a device left with the
    // medium in its socket must not burn a flash write per boot.
    let second_bytes = fs::read(settings_path(dir.path())).expect("read settings file");
    let mut third = store.load().expect("reload");
    assert!(matches!(
        import(&store, &mut third, &root).expect("third import"),
        Outcome::Unchanged { .. }
    ));
    assert_eq!(third, second);
    assert_eq!(
        fs::read(settings_path(dir.path())).expect("re-read settings file"),
        second_bytes,
        "settings.toml must be byte-identical from the second import on"
    );
}

// The digest is over the CANONICAL document: the same configuration
// written differently is the same document. Without this, an operator who
// reformatted the file on the medium would re-claim the device.
#[test]
pub(super) fn the_digest_ignores_comments_whitespace_and_section_order() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let original = "version = 1\n\n[identity]\ndeviceId = \"0123456789abcdef0123456789abcdef\"\n\n                        [time]\ntimezone = \"Europe/Berlin\"\n";
    let root = stage(dir.path(), Source::Boot, original);
    let mut settings = Settings::default();
    assert!(matches!(
        import(&store, &mut settings, &root).expect("first"),
        Outcome::Applied { .. }
    ));

    // The same document, rewritten by hand: a comment, different spacing,
    // and the two sections in the other order.
    let rewritten = "# rewritten by an operator\nversion=1\n\n[time]\n\ntimezone   =   \
                     \"Europe/Berlin\"\n\n[identity]\ndeviceId=\"0123456789abcdef0123456789abcdef\"\n";
    let root = stage(dir.path(), Source::Boot, rewritten);
    let mut reloaded = store.load().expect("reload");
    let outcome = import(&store, &mut reloaded, &root).expect("second");
    assert!(
        matches!(outcome, Outcome::Unchanged { .. }),
        "a reformatted document is the same document, got {outcome:?}"
    );

    // And a document that differs in a VALUE is a different document.
    let root = stage(
        dir.path(),
        Source::Boot,
        "version = 1\n\n[time]\ntimezone = \"UTC\"\n",
    );
    let mut reloaded = store.load().expect("reload");
    let outcome = import(&store, &mut reloaded, &root).expect("third");
    assert!(
        matches!(outcome, Outcome::Applied { .. }),
        "a changed value must not be short-circuited, got {outcome:?}"
    );
    assert_eq!(reloaded.time.timezone, "UTC");
}
