//! Transactions interrupted at every IO boundary.

use mica_deploy::{
    components::authenticate_deployment,
    deployments::{BootBackend, BootReceipt, Target},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

use super::*;

#[test]
#[ignore = "requires tests/file-ab-faults/run.sh and its isolated IO fault shim"]
pub(super) fn transactions_survive_each_boundary() {
    use std::os::unix::process::ExitStatusExt;
    if let Ok(root) = std::env::var("MICA_FAULT_WORK") {
        run_operation(
            Path::new(&root),
            std::env::var("MICA_FAULT_FIT").unwrap() == "1",
            &std::env::var("MICA_FAULT_OPERATION").unwrap(),
        );
        return;
    }
    let shim = std::env::var("MICA_TEST_FAULT_SHIM").expect("run tests/file-ab-faults/run.sh");
    let evidence = PathBuf::from(std::env::var("MICA_TEST_FAULT_EVIDENCE").unwrap());
    let mut summaries = Vec::new();
    for fit in [false, true] {
        for operation in ["install", "confirm", "gc"] {
            let invoke = |root: &Path, at: usize, phase: &str, enospc: bool| {
                let mut cmd = Command::new(std::env::current_exe().unwrap());
                cmd.args([
                    "--ignored",
                    "--exact",
                    "transactions::transactions_survive_each_boundary",
                    "--test-threads=1",
                ])
                .env("LD_PRELOAD", &shim)
                .env("MICA_FAULT_WORK", root)
                .env("MICA_FAULT_OPERATION", operation)
                .env("MICA_FAULT_FIT", if fit { "1" } else { "0" })
                .env("MICA_FAULT_PREFIX", root.join("store"))
                .env("MICA_FAULT_LOG", root.join("trace"))
                .env("MICA_FAULT_AT", at.to_string())
                .env("MICA_FAULT_WHEN", phase);
                if enospc {
                    cmd.env("MICA_FAULT_ENOSPC", "1");
                }
                cmd.output().unwrap()
            };
            let baseline = TempDir::new_in(&evidence).unwrap();
            seed(baseline.path(), fit, operation);
            let result = invoke(baseline.path(), 0, "before", false);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
            validate(baseline.path(), fit);
            let trace = fs::read_to_string(baseline.path().join("trace")).unwrap();
            assert!(
                trace.contains("fsync"),
                "missing persistence instrumentation"
            );
            if operation == "install" {
                assert!(trace.contains("copy_file_range") || trace.contains("write"));
                assert!(trace.contains("rename"));
            }
            if operation == "gc" {
                assert!(trace.contains("unlink") && trace.contains("rmdir"));
            }
            let count = trace.lines().count();
            fs::write(evidence.join(format!("{fit}-{operation}.trace")), trace).unwrap();
            for at in 1..=count {
                for (phase, enospc) in [("before", false), ("after", false), ("before", true)] {
                    let case = TempDir::new_in(&evidence).unwrap();
                    seed(case.path(), fit, operation);
                    let result = invoke(case.path(), at, phase, enospc);
                    let check = std::panic::catch_unwind(|| {
                        if enospc {
                            assert!(result.status.success() || result.status.code() == Some(101));
                        } else {
                            assert_eq!(
                                result.status.signal(),
                                Some(9),
                                "boundary {at}/{count}: {phase} {operation}"
                            );
                        }
                        validate(case.path(), fit);
                        // Restart the interrupted transaction with its durable
                        // state; an already published candidate is reconciled.
                        let restarted = store(case.path(), fit);
                        if operation != "install"
                            || restarted.effective_state().unwrap().candidate.is_none()
                        {
                            run_operation(case.path(), fit, operation);
                        } else {
                            let receipt = serde_json::from_slice(
                                &fs::read(case.path().join("current-receipt.json")).unwrap(),
                            )
                            .unwrap();
                            restarted.confirm(&receipt).unwrap();
                        }
                        validate(case.path(), fit);
                    });
                    if let Err(error) = check {
                        eprintln!("failed fixture retained: {}", case.keep().display());
                        std::panic::resume_unwind(error);
                    }
                }
            }
            let summary = format!(
                "fit={fit} {operation}: {count} IO boundaries, {} interrupted/error cases",
                count * 3
            );
            println!("{summary}");
            summaries.push(summary);
        }
    }
    fs::write(evidence.join("results.txt"), summaries.join("\n")).unwrap();
}

#[test]
pub(super) fn install_replaces_the_inactive_deployment_and_preserves_shared_objects() {
    for fit in [false, true] {
        for operation in ["install", "reuse"] {
            let root = TempDir::new().unwrap();
            seed(root.path(), fit, operation);
            let store = store(root.path(), fit);
            let old = fs::read_to_string(root.path().join("retired-id")).unwrap();
            let old_descriptor = store.system.join(format!("deployments/{old}.json"));
            let public: [u8; 32] = fs::read(root.path().join("public"))
                .unwrap()
                .try_into()
                .unwrap();
            let old_deployment =
                authenticate_deployment(&fs::read(&old_descriptor).unwrap(), &[public]).unwrap();
            if operation == "reuse" {
                // Acquisition omitted an object already verified on SYSTEM.
                fs::remove_file(
                    root.path()
                        .join("objects")
                        .join(&old_deployment.rootfs.content.image.sha256),
                )
                .unwrap();
            }
            run_operation(root.path(), fit, "install");
            validate(root.path(), fit);
            assert!(!old_descriptor.exists());
            assert_eq!(store.state().unwrap().fallback, None);
            assert_eq!(store.entries().unwrap().len(), 2);
            assert_eq!(
                store
                    .system
                    .join(format!("roots/{}", old_deployment.rootfs.id))
                    .exists(),
                operation == "reuse"
            );
            assert_eq!(
                fs::read_dir(store.system.join("deployments"))
                    .unwrap()
                    .count(),
                2
            );
        }
    }
}

#[test]
pub(super) fn invalid_update_preserves_both_installed_deployments() {
    for fit in [false, true] {
        let root = TempDir::new().unwrap();
        seed(root.path(), fit, "install");
        let store = store(root.path(), fit);
        let public: [u8; 32] = fs::read(root.path().join("public"))
            .unwrap()
            .try_into()
            .unwrap();
        let envelope = fs::read(root.path().join("candidate.json")).unwrap();
        let candidate = authenticate_deployment(&envelope, &[public]).unwrap();
        fs::write(
            root.path()
                .join("objects")
                .join(&candidate.rootfs.content.image.sha256),
            b"corrupt",
        )
        .unwrap();
        let before = serde_json::to_vec(&store.entries().unwrap()).unwrap();
        assert!(
            store
                .install(
                    &envelope,
                    &[public],
                    &Target {
                        board: if fit { "cx3576" } else { "uefi-x64" },
                        arch: if fit { "arm64" } else { "amd64" },
                        product: "uefi-x64-dev",
                        features: &[],
                    },
                    &root.path().join("objects"),
                    &serde_json::from_slice(
                        &fs::read(root.path().join("current-receipt.json")).unwrap()
                    )
                    .unwrap()
                )
                .is_err()
        );
        assert_eq!(
            before,
            serde_json::to_vec(&store.entries().unwrap()).unwrap()
        );
        validate(root.path(), fit);
    }
}

#[test]
pub(super) fn install_requires_the_confirmed_running_receipt_and_reconciles_activation() {
    for fit in [false, true] {
        let root = TempDir::new().unwrap();
        seed(root.path(), fit, "install");
        let store = store(root.path(), fit);
        let public: [u8; 32] = fs::read(root.path().join("public"))
            .unwrap()
            .try_into()
            .unwrap();
        let envelope = fs::read(root.path().join("candidate.json")).unwrap();
        let mut current: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
                .unwrap();
        let install = |receipt: &BootReceipt| {
            store.install(
                &envelope,
                &[public],
                &Target {
                    board: if fit { "cx3576" } else { "uefi-x64" },
                    arch: if fit { "arm64" } else { "amd64" },
                    product: "uefi-x64-dev",
                    features: &[],
                },
                &root.path().join("objects"),
                receipt,
            )
        };
        current.content_verified = false;
        assert!(
            install(&current)
                .unwrap_err()
                .to_string()
                .contains("authenticated")
        );
        current.content_verified = true;
        let candidate_receipt: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("candidate-receipt.json")).unwrap())
                .unwrap();
        assert!(install(&candidate_receipt).is_err());
        let pending = install(&current).unwrap();
        let candidate = pending.candidate.unwrap();
        let mut interrupted = store.state().unwrap();
        interrupted.candidate = None;
        interrupted.highest_generation = 2;
        store.save_state(&interrupted).unwrap();
        let recovered = store.effective_state().unwrap();
        assert_eq!(recovered.candidate.as_deref(), Some(candidate.as_str()));
        assert_eq!(recovered.highest_generation, 3);
        assert_eq!(
            store.confirm(&current).unwrap().candidate,
            Some(candidate.clone())
        );
        assert_eq!(store.describe(&[public]).unwrap().len(), 2);
        assert!(install(&current).is_err());
        assert!(
            store.confirm(&candidate_receipt).is_err(),
            "unlaunched trial was confirmed"
        );
        if let BootBackend::Fit { firmware, .. } = &store.boot {
            let mut env =
                mica_deploy::fit_env::Environment::load(firmware, boards::layout("cx3576"))
                    .unwrap();
            env.records[0].tries_left = Some(2);
            env.save(firmware).unwrap();
        } else {
            fs::rename(
                root.path()
                    .join(format!("store/esp/loader/entries/mica-{candidate}+3.conf")),
                root.path().join(format!(
                    "store/esp/loader/entries/mica-{candidate}+2-1.conf"
                )),
            )
            .unwrap();
        }
        assert!(install(&candidate_receipt).is_err());
        let confirmed = store.confirm(&candidate_receipt).unwrap();
        assert_eq!(confirmed.current, Some(candidate));
        assert_eq!(confirmed.fallback, Some(current.deployment_id));
        assert!(confirmed.candidate.is_none());
        validate(root.path(), fit);
    }
}
