use super::*;
use crate::deployment::Status;

const MANIFEST: &str = "#package\tversion\tarchitecture\n\
    base-files\t13.8\tarm64\n\
    mica-apid\t0.1.1-1\tarm64\n\
    mica-deploy\t0.1.1-1\tarm64\n\
    micad\t0.1.1-1\tarm64\n\
    systemd\t257.7-1\tarm64\n";

fn daemon() -> DaemonIdentity {
    DaemonIdentity {
        name: "micad",
        version: "0.1.1-1",
    }
}

/// A fixture tree carrying every seam, so the assembly — the part where
/// this module's bugs would live — runs end to end without a device.
fn fixture_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let write = |relative: &str, contents: &[u8]| {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write fixture");
    };
    write(MACHINE_ID_PATH, b"0123456789abcdef0123456789abcdef\n");
    write(MANIFEST_PATH, MANIFEST.as_bytes());
    write(
        OS_RELEASE_PATH,
        b"PRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"13\"\nID=debian\n# a comment\n",
    );
    write(DT_MODEL_PATH, b"Vendor CX3576 Board\0");
    write(KERNEL_RELEASE_PATH, b"6.1.115-mica\n");
    write(
        KERNEL_VERSION_PATH,
        b"#1 SMP PREEMPT Mon Sep 1 00:00:00 UTC 2026\n",
    );
    write(UPTIME_PATH, b"12345.67 8888.00\n");
    // PRODUCTION-shaped: the baked public set's required member is there
    // and no marker is beside it, because that is the released state. A
    // test that wants the development branch writes the marker itself.
    write(
        BAKED_META_MANIFEST_PATH,
        b"{ \"schema\": \"mica/meta/v1\", \"update\": { \"source\": null } }\n",
    );
    dir
}

#[test]
fn the_manifest_parses_by_the_shipped_shape() {
    let manifest = parse_manifest(MANIFEST);
    assert_eq!(manifest.rows.len(), 5);
    assert_eq!(manifest.malformed, 0);
    assert!(!manifest.truncated);
    assert_eq!(manifest.rows[1].name, "mica-apid");
    assert_eq!(manifest.rows[1].version, "0.1.1-1");
    assert_eq!(manifest.rows[1].architecture, "arm64");
    assert!(manifest.rows[1].is_mica());
    assert!(!manifest.rows[0].is_mica());
}

/// A row that is not three fields is counted, not silently dropped and
/// not allowed to poison the rows around it.
#[test]
fn a_malformed_row_is_counted_and_skipped() {
    let manifest = parse_manifest("a\t1\n\nb\t2\tamd64\tx\nc\t3\tamd64\n#h\n\t\t\n");
    assert_eq!(manifest.rows.len(), 1);
    assert_eq!(manifest.rows[0].name, "c");
    assert_eq!(manifest.malformed, 3);
}

/// The cap is enforced, and crossing it is reported rather than hidden.
#[test]
fn rows_beyond_the_cap_are_dropped_and_flagged() {
    let text: String = (0..(MAX_MANIFEST_ROWS + 5))
        .map(|i| format!("p{i}\t1\tamd64\n"))
        .collect();
    let manifest = parse_manifest(&text);
    assert_eq!(manifest.rows.len(), MAX_MANIFEST_ROWS);
    assert!(manifest.truncated);
}

/// The core components of the running deployment are packages too: they are
/// composed over the root and its manifest does not list them.
#[test]
fn the_running_deployments_core_components_are_rows() {
    use base64::Engine;
    let payload = serde_json::json!({
        "schema": "mica/deployment/v1",
        "core": [
            { "package": "mica-apid-ui", "version": "0.0.1", "arch": "arm64" },
            { "package": "micad", "version": "0.0.1", "arch": "arm64" },
        ],
    });
    let envelope = serde_json::json!({
        "payload": base64::engine::general_purpose::STANDARD.encode(payload.to_string()),
    })
    .to_string();
    let rows = core_rows(&envelope);
    assert_eq!(
        rows.iter()
            .map(|r| (r.name.as_str(), r.version.as_str(), r.architecture.as_str()))
            .collect::<Vec<_>>(),
        [
            ("mica-apid-ui", "0.0.1", "arm64"),
            ("micad", "0.0.1", "arm64")
        ]
    );
    assert!(rows.iter().all(PackageRow::is_mica));
    // A descriptor without core components, or anything that is not a
    // descriptor, gives none.
    assert!(core_rows("not json").is_empty());
    assert!(core_rows(r#"{"payload":"e30="}"#).is_empty());
}

#[test]
fn os_release_values_lose_their_quotes() {
    let fields = parse_os_release("A=\"x y\"\nB='z'\nC=plain\n# c\n\nD\n");
    assert_eq!(fields["A"], "x y");
    assert_eq!(fields["B"], "z");
    assert_eq!(fields["C"], "plain");
    assert!(!fields.contains_key("D"));
}

#[test]
fn a_machine_id_is_thirty_two_lowercase_hex() {
    assert!(is_machine_id("0123456789abcdef0123456789abcdef"));
    assert!(!is_machine_id("0123456789ABCDEF0123456789abcdef"));
    assert!(!is_machine_id("0123456789abcdef0123456789abcde"));
    assert!(!is_machine_id(""));
}

/// The assembly over a full fixture tree: every seam read from where it
/// lives, and the pinned file epoch read from the manifest's mtime.
#[tokio::test]
async fn a_full_root_yields_every_member_available() {
    let root = fixture_root();
    let evidence = HostSystemInfo::at(root.path())
        .observe()
        .await
        .expect("observe");
    let deployment = Status::parse(&crate::deployment::tests::fixture().to_string()).unwrap();
    let info = info_json(&evidence, Some(&deployment), &daemon());

    assert_eq!(info["machineId"]["available"], true);
    assert_eq!(info["machineId"]["id"], "0123456789abcdef0123456789abcdef");
    assert_eq!(info["board"]["model"], "Vendor CX3576 Board");
    assert_eq!(info["board"]["source"], "devicetree");
    assert_eq!(info["kernel"]["release"], "6.1.115-mica");
    assert!(
        info["kernel"]["version"]
            .as_str()
            .is_some_and(|v| v.starts_with("#1 SMP"))
    );
    assert_eq!(info["release"]["id"], "debian");
    assert_eq!(
        info["release"]["prettyName"],
        "Debian GNU/Linux 13 (trixie)"
    );
    assert_eq!(info["release"]["versionId"], "13");
    assert_eq!(info["system"]["available"], true);
    assert_eq!(info["system"]["package"], "micad");
    assert_eq!(info["system"]["version"], "0.1.1-1");
    assert!(info["system"]["gitStamp"].is_null());
    assert!(info["system"]["commitDate"].is_null());
    // The file epoch is the manifest's mtime, which the fixture wrote just
    // now: a real epoch, rendered RFC 3339 beside it. On a composed image
    // it is the pinned SOURCE_DATE_EPOCH instead.
    let epoch = info["system"]["fileEpoch"]["epoch"]
        .as_u64()
        .expect("file epoch");
    assert!(epoch > 1_700_000_000, "epoch {epoch}");
    assert!(
        info["system"]["fileEpoch"]["date"]
            .as_str()
            .is_some_and(|d| d.ends_with('Z')),
        "{}",
        info["system"]
    );
    assert_eq!(info["daemon"]["name"], "micad");
    assert_eq!(info["daemon"]["version"], "0.1.1-1");
    assert!(info["daemon"]["commit"].is_null());
    assert_eq!(info["packages"]["count"], 5);
    assert_eq!(info["packages"]["micaCount"], 3);
    assert_eq!(info["packages"]["entries"][3]["name"], "micad");
    assert_eq!(info["packages"]["entries"][3]["mica"], true);
    assert_eq!(info["deployment"]["id"], "a".repeat(64));
    assert_eq!(info["deployment"]["kernelId"], "c".repeat(64));
    assert_eq!(info["deployment"]["rootfsId"], "d".repeat(64));
    assert_eq!(info["uptime"]["seconds"], 12345);
}

/// The empty root: every member is present and says WHY it is absent.
/// Nothing is manufactured, and nothing panics.
#[tokio::test]
async fn an_empty_root_reports_every_member_absent_with_a_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let evidence = HostSystemInfo::at(dir.path())
        .observe()
        .await
        .expect("observe");
    let info = info_json(&evidence, None, &daemon());
    for member in [
        "machineId",
        "board",
        "kernel",
        "release",
        "system",
        "packages",
        "deployment",
        "uptime",
    ] {
        assert_eq!(
            info[member]["available"], false,
            "{member}: {}",
            info[member]
        );
        assert!(
            info[member]["detail"]
                .as_str()
                .is_some_and(|d| !d.is_empty()),
            "{member} carries no reason: {}",
            info[member]
        );
    }
    // The daemon's own identity needs no file and is always there.
    assert_eq!(info["daemon"]["version"], "0.1.1-1");
}

/// A machine id that is not the systemd shape is absent evidence, not a
/// value that looks like one.
#[tokio::test]
async fn a_malformed_machine_id_is_absent_with_the_reason() {
    let root = fixture_root();
    std::fs::write(root.path().join(MACHINE_ID_PATH), "not-an-id\n").expect("write");
    let evidence = HostSystemInfo::at(root.path())
        .observe()
        .await
        .expect("observe");
    let info = info_json(&evidence, None, &daemon());
    assert_eq!(info["machineId"]["available"], false);
    assert!(
        info["machineId"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("32-character")),
        "{}",
        info["machineId"]
    );
}

/// x64 has no device tree: the board comes from DMI, vendor first.
#[tokio::test]
async fn a_dmi_board_is_read_when_there_is_no_device_tree() {
    let root = fixture_root();
    std::fs::remove_file(root.path().join(DT_MODEL_PATH)).expect("drop dt model");
    let write = |relative: &str, contents: &str| {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    };
    write(DMI_PRODUCT_PATH, "NUC13ANHi5\n");
    write(DMI_VENDOR_PATH, "Intel Corporation\n");
    let evidence = HostSystemInfo::at(root.path())
        .observe()
        .await
        .expect("observe");
    let info = info_json(&evidence, None, &daemon());
    assert_eq!(info["board"]["model"], "Intel Corporation NUC13ANHi5");
    assert_eq!(info["board"]["source"], "dmi");
}

/// The unavailable default refuses rather than inspecting the host.
#[tokio::test]
async fn the_unavailable_source_is_an_error() {
    assert!(UnavailableSystemInfo.observe().await.is_err());
}

/// The released state: the baked set is there and no marker is beside it.
#[tokio::test]
async fn a_root_with_no_marker_beside_its_baked_manifest_is_production() {
    let root = fixture_root();
    let evidence = HostSystemInfo::at(root.path())
        .observe()
        .await
        .expect("observe");
    assert_eq!(evidence.trust, Some(TrustGrade::Production));
    let info = info_json(&evidence, None, &daemon());
    assert_eq!(info["trust"]["available"], true);
    assert_eq!(info["trust"]["grade"], "production");
    // No marker path and no domain list on a production image: they are
    // facts about a marker, and there is none.
    assert!(info["trust"]["marker"].is_null());
    assert!(info["trust"]["developmentDomains"].is_null());
}

/// The marker is baked, so the device says so — and names WHICH half,
/// because the mixed tree is real: a content signing ceremony's output
/// copied in while the package signing key is still development-grade.
#[tokio::test]
async fn a_baked_marker_reports_development_and_the_domains_it_names() {
    let root = fixture_root();
    std::fs::write(
        root.path().join(BAKED_META_MARKER_PATH),
        "DEVELOPMENT-GRADE\nDOMAINS=boot verity updates\n",
    )
    .expect("write");
    let evidence = HostSystemInfo::at(root.path())
        .observe()
        .await
        .expect("observe");
    assert_eq!(
        evidence.trust,
        Some(TrustGrade::Development {
            domains: vec![
                "boot".to_string(),
                "verity".to_string(),
                "updates".to_string()
            ],
        })
    );
    let info = info_json(&evidence, None, &daemon());
    assert_eq!(info["trust"]["grade"], "development");
    assert_eq!(info["trust"]["developmentDomains"][2], "updates");
    assert_eq!(
        info["trust"]["marker"],
        format!("/{BAKED_META_MARKER_PATH}")
    );
}

/// **The vacuity case, and the one this member exists to get right.** No
/// baked `meta/` tree at all — an image built before the seam, or one
/// whose staging broke — has no marker for the same reason a production
/// image has none. Reporting it as production would have the surface
/// claim a production CA on a device that provisions no anchor, so it is
/// reported ABSENT with the reason.
#[tokio::test]
async fn a_root_with_no_baked_meta_at_all_is_absent_and_never_production() {
    let root = fixture_root();
    std::fs::remove_file(root.path().join(BAKED_META_MANIFEST_PATH)).expect("rm");
    let evidence = HostSystemInfo::at(root.path())
        .observe()
        .await
        .expect("observe");
    assert_eq!(evidence.trust, None);
    let info = info_json(&evidence, None, &daemon());
    assert_eq!(info["trust"]["available"], false);
    assert_ne!(info["trust"]["grade"], "production");
    assert!(
        info["trust"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains(BAKED_META_MANIFEST_PATH)),
        "{}",
        info["trust"]
    );
}

/// A marker with no `DOMAINS=` line is still a marker. The presence is the
/// claim; the line only says which half.
#[test]
fn a_marker_naming_no_domain_is_still_development() {
    assert_eq!(marker_domains("no domains here\n"), Vec::<String>::new());
    assert_eq!(marker_domains("DOMAINS=unknown\n"), Vec::<String>::new());
    assert_eq!(marker_domains("DOMAINS=boot boot\n"), Vec::<String>::new());
    // A duplicate assignment is not an alternate key-generation format.
    assert_eq!(
        marker_domains("DOMAINS=boot\nDOMAINS=boot verity updates\n"),
        Vec::<String>::new()
    );
}
