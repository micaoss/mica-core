use super::*;
use std::fs;

/// A fake mica-deploy at `dir/deploy` that appends its arguments to
/// `dir/calls`, lists what `--objects` holds there too, and answers `answer`
/// or fails with `refusal`.
fn fake(dir: &Path, answer: &str, refusal: Option<&str>) -> Drivers {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("deploy");
    let calls = dir.join("calls");
    let end = match refusal {
        Some(refusal) => format!("echo 'Error: {refusal}' >&2\nexit 1\n"),
        None => format!("echo '{answer}'\n"),
    };
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{calls}'\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --objects ]; then ls \"$2\" >> '{calls}'; fi\n  case \"$1\" in *.cert) cat \"$1\" >> '{calls}'; echo >> '{calls}';; esac\n  shift\ndone\n{end}",
            calls = calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    Drivers::new(script)
}

fn calls(dir: &Path) -> String {
    fs::read_to_string(dir.join("calls")).unwrap_or_default()
}

#[tokio::test]
async fn a_certificate_is_handed_over_in_a_file_that_goes_after() {
    let dir = tempfile::tempdir().unwrap();
    let drivers = fake(dir.path(), r#"{"sha256":"ab","applies":"next-boot"}"#, None);
    let uploads = dir.path().join("uploads");
    fs::create_dir(&uploads).unwrap();
    let added = drivers
        .add_owner_cert(&uploads, b"PEM", true, "console", "192.0.2.1")
        .await
        .unwrap();
    assert_eq!(added["sha256"], "ab");
    let calls = calls(dir.path());
    assert!(calls.starts_with("owner-cert-add "), "{calls}");
    assert!(
        calls.contains(" --by console --source 192.0.2.1 --clock-synced\nPEM\n"),
        "{calls}"
    );
    assert_eq!(fs::read_dir(&uploads).unwrap().count(), 0);
}

#[tokio::test]
async fn a_refusal_is_mica_deploys_last_word() {
    let dir = tempfile::tempdir().unwrap();
    let drivers = fake(dir.path(), "", Some("the certificate is enrolled already"));
    let error = drivers
        .remove_owner_cert(&"a".repeat(64), "api", "token")
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "the certificate is enrolled already");
}

#[tokio::test]
async fn three_files_are_laid_out_as_extension_install_reads_them() {
    let dir = tempfile::tempdir().unwrap();
    let drivers = fake(dir.path(), r#"{"extensionId":"cd"}"#, None);
    let uploads = dir.path().join("uploads");
    fs::create_dir(&uploads).unwrap();
    let (descriptor, image, signature) = (
        uploads.join("1.json"),
        uploads.join("2.img"),
        uploads.join("3.p7s"),
    );
    for path in [&descriptor, &image, &signature] {
        fs::write(path, b"x").unwrap();
    }
    let installed = drivers
        .install_extension(&descriptor, &image, &signature)
        .await
        .unwrap();
    assert_eq!(installed["extensionId"], "cd");
    let calls = calls(dir.path());
    assert!(calls.starts_with("extension-install "), "{calls}");
    assert!(calls.contains("ext.img\next.roothash.p7s\n"), "{calls}");
    assert_eq!(fs::read_dir(&uploads).unwrap().count(), 0, "all of it goes");
}

#[tokio::test]
async fn the_extensions_are_the_status_s_with_the_running_kernel() {
    let dir = tempfile::tempdir().unwrap();
    let drivers = fake(
        dir.path(),
        r#"{"boot":{"kernelId":"k"},"extensions":{"installed":{},"waitsForExtension":[]}}"#,
        None,
    );
    let extensions = drivers.extensions().await.unwrap();
    assert_eq!(extensions["kernel"], "k");
    assert_eq!(extensions["waitsForExtension"], serde_json::json!([]));
    assert_eq!(calls(dir.path()), "status\n");
}
