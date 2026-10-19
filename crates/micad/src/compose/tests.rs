use std::os::unix::fs::PermissionsExt;

use super::*;

/// A fake mica-containerd: `compose check` refuses a file naming
/// `privileged`, and answers anything else with one container per project.
pub(crate) fn fake_containerd(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().join("mica-containerd");
    let script = r#"#!/bin/sh
case "$1" in
compose)
  text=$(cat)
  case "$text" in
  *privileged*)
    echo '{"error":"compose","detail":"web: privileged is not supported","problems":[{"service":"web","field":"privileged","detail":"privileged is not supported"}]}' >&2
    exit 1 ;;
  esac
  dir=/mica/volumes/$4
  [ "$5" = --project-dir ] && dir=$6
  echo '{"project":"'"$4"'","projectDir":"'"$dir"'","containers":[{"name":"'"$4"'-web-1","dir":"'"$dir"'"}]}' ;;
*) exit 2 ;;
esac
"#;
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A thread forking while the file was open for writing holds it open in
    // its child until that child execs, and the kernel refuses to run a file
    // open for writing (ETXTBSY). Once one run succeeds, no child can hold it.
    for _ in 0..200 {
        match std::process::Command::new(&path).arg("-").output() {
            Err(err) if err.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => break,
        }
    }
    path
}

#[tokio::test]
async fn a_file_is_checked_and_its_problems_answered() {
    let dir = tempfile::tempdir().unwrap();
    let check = ContainerdCompose::at(fake_containerd(&dir));

    let ok = check
        .check("app", "services:\n  web:\n    image: a:1\n", None)
        .await
        .unwrap();
    assert_eq!(
        ok,
        json!({ "ok": true, "containers": [{ "name": "app-web-1", "dir": "/mica/volumes/app" }] })
    );
    assert_eq!(refusal("app", &ok), None);

    let refused = check
        .check("app", "services:\n  web:\n    privileged: true\n", None)
        .await
        .unwrap();
    assert_eq!(refused["ok"], json!(false));
    assert_eq!(refused["problems"][0]["field"], json!("privileged"));
    let sentence = refusal("app", &refused).unwrap();
    assert!(
        sentence.contains("\"app\"") && sentence.contains("privileged"),
        "{sentence}"
    );
}

/// A binary that is not there, or a build that checks nothing, is an error,
/// never a passing check.
#[tokio::test]
async fn a_check_that_cannot_run_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let absent = ContainerdCompose::at(dir.path().join("absent"));
    assert!(absent.check("app", "services: {}\n", None).await.is_err());
    assert!(NoCompose.check("app", "x", None).await.is_err());
}

/// A project directory is the check's `--project-dir`.
#[tokio::test]
async fn a_project_directory_is_checked_in() {
    let dir = tempfile::tempdir().unwrap();
    let check = ContainerdCompose::at(fake_containerd(&dir));
    let ok = check
        .check(
            "site",
            "services:\n  web:\n    image: a:1\n",
            Some("/srv/site"),
        )
        .await
        .unwrap();
    assert_eq!(ok["containers"][0]["dir"], json!("/srv/site"));
}
