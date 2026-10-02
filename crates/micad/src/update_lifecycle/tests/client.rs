//! The subprocess client against a real native protocol.

use super::*;

#[tokio::test]
pub(super) async fn the_subprocess_client_bounds_output_and_stops_its_transport_group() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("transport");
    let pid = dir.path().join("child.pid");
    std::fs::write(&stub, "#!/bin/sh\nif [ \"$1\" = large ]; then head -c 70000 /dev/zero; else sleep 60 & echo $! > \"$1\"; wait; fi\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let client = Arc::new(SubprocessClient::new(stub));
    assert!(
        client
            .run(&["large".into()], Duration::from_secs(5))
            .await
            .is_err()
    );
    assert!(
        client
            .run(
                &[pid.to_string_lossy().into_owned()],
                Duration::from_millis(150)
            )
            .await
            .is_err()
    );
    let child = std::fs::read_to_string(pid).unwrap();
    assert_transport_stopped(child.trim()).await;
    let cancelled_pid = dir.path().join("cancelled.pid");
    let args = vec![cancelled_pid.to_string_lossy().into_owned()];
    let task = tokio::spawn(async move { client.run(&args, Duration::from_secs(30)).await });
    for _ in 0..100 {
        if cancelled_pid.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let child = std::fs::read_to_string(cancelled_pid).unwrap();
    task.abort();
    let _ = task.await;
    assert_transport_stopped(child.trim()).await;
}

pub(super) async fn assert_transport_stopped(pid: &str) {
    // SIGKILL delivery precedes the child's final scheduler transition.
    // Bound that transition instead of racing one immediate /proc read.
    for _ in 0..100 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        if stat.is_err() || stat.unwrap().split_once(") ").unwrap().1.starts_with('Z') {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("transport child survived client termination");
}

#[tokio::test]
pub(super) async fn the_subprocess_client_runs_a_real_native_protocol_and_reports_absence() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("mica-deploy");
    std::fs::write(&stub, format!("#!/bin/sh\ncase \"$1\" in\ncheck) echo '{}' ;;\nfetch) echo '{}' ;;\nprobe) echo 'DATA is not mounted' >&2; exit 1 ;;\n*) echo boom >&2; exit 1 ;;\nesac\n", selection_output().stdout, fetch_output(&staged_path()).stdout)).unwrap();
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let client = SubprocessClient::new(stub);
    assert_eq!(client.unavailable(), None);
    let checked = client
        .run(&["check".into()], Duration::from_secs(10))
        .await
        .unwrap();
    assert!(matches!(
        parse_check(&checked),
        Ok(CheckOutcome::Selected(_))
    ));
    let fetched = client
        .run(&["fetch".into()], Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        parse_fetch(&fetched, Path::new("/mica/updates/verified")),
        Ok(FetchOutcome::Staged(staged_path()))
    );
    let probed = client
        .run(&["probe".into()], Duration::from_secs(10))
        .await
        .unwrap();
    assert!(matches!(parse_probe(&probed), Ok(ProbeOutcome::Unready(_))));
    let absent = SubprocessClient::new(dir.path().join("gone"));
    assert!(absent.unavailable().is_some());
    assert!(
        absent
            .run(&["status".into()], Duration::from_secs(10))
            .await
            .unwrap_err()
            .is::<ClientUnavailable>()
    );
}
