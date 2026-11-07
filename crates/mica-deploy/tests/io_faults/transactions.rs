//! Transactions interrupted at every IO boundary.

use mica_deploy::{
    components::authenticate_deployment,
    deployments::{BootReceipt, Target},
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
            Kind::named(&std::env::var("MICA_FAULT_KIND").unwrap()),
            &std::env::var("MICA_FAULT_OPERATION").unwrap(),
        );
        return;
    }
    let shim = std::env::var("MICA_TEST_FAULT_SHIM").expect("run tests/file-ab-faults/run.sh");
    let evidence = PathBuf::from(std::env::var("MICA_TEST_FAULT_EVIDENCE").unwrap());
    let mut summaries = Vec::new();
    for kind in KINDS {
        for operation in [
            "install",
            "confirm",
            "gc",
            "import",
            "reset-stage",
            "reset-finish",
            "ext-install",
            "ext-pair",
        ] {
            // Only a `ubi` board writes an import straight into its store.
            if operation == "import" && kind != Kind::Ubi {
                continue;
            }
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
                .env("MICA_FAULT_KIND", format!("{kind:?}"))
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
            seed(baseline.path(), kind, operation);
            let result = invoke(baseline.path(), 0, "before", false);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
            validate(baseline.path(), kind);
            validate_reset(baseline.path(), kind, operation);
            validate_extensions(baseline.path(), kind, operation, true);
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
            fs::write(evidence.join(format!("{kind:?}-{operation}.trace")), trace).unwrap();
            for at in 1..=count {
                for (phase, enospc) in [("before", false), ("after", false), ("before", true)] {
                    let case = TempDir::new_in(&evidence).unwrap();
                    seed(case.path(), kind, operation);
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
                        validate(case.path(), kind);
                        validate_extensions(case.path(), kind, operation, false);
                        // Restart the interrupted transaction with its durable
                        // state; an already published candidate is reconciled.
                        let restarted = store(case.path(), kind);
                        if !["install", "ext-pair"].contains(&operation)
                            || restarted.effective_state().unwrap().candidate.is_none()
                        {
                            run_operation(case.path(), kind, operation);
                        } else {
                            let receipt = serde_json::from_slice(
                                &fs::read(case.path().join("current-receipt.json")).unwrap(),
                            )
                            .unwrap();
                            restarted.confirm(&receipt).unwrap();
                        }
                        validate(case.path(), kind);
                        if operation == "import" {
                            validate_import(case.path());
                        }
                        validate_reset(case.path(), kind, operation);
                        validate_extensions(case.path(), kind, operation, true);
                    });
                    if let Err(error) = check {
                        eprintln!("failed fixture retained: {}", case.keep().display());
                        std::panic::resume_unwind(error);
                    }
                }
            }
            let summary = format!(
                "{kind:?} {operation}: {count} IO boundaries, {} interrupted/error cases",
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
    for kind in KINDS {
        for operation in ["install", "reuse"] {
            let root = TempDir::new().unwrap();
            seed(root.path(), kind, operation);
            let store = store(root.path(), kind);
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
            run_operation(root.path(), kind, "install");
            validate(root.path(), kind);
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
            if kind == Kind::Ubi {
                let device = ubi::DirUbi::open_at(&root.path().join("store/ubi"));
                assert_eq!(
                    device
                        .names()
                        .contains(&format!("r-{}", old_deployment.rootfs.id)),
                    operation == "reuse"
                );
                assert!(!device.names().iter().any(|name| name.starts_with("p.")));
            }
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
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "install");
        let store = store(root.path(), kind);
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
                        board: if kind.fit() { "cx3576" } else { "uefi-x64" },
                        arch: if kind.fit() { "arm64" } else { "amd64" },
                        product: "uefi-x64-dev",
                        features: &[],
                    },
                    &root.path().join("objects"),
                    &serde_json::from_slice(
                        &fs::read(root.path().join("current-receipt.json")).unwrap()
                    )
                    .unwrap(),
                    None
                )
                .is_err()
        );
        assert_eq!(
            before,
            serde_json::to_vec(&store.entries().unwrap()).unwrap()
        );
        validate(root.path(), kind);
    }
}

#[test]
pub(super) fn install_requires_the_confirmed_running_receipt_and_reconciles_activation() {
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "install");
        let store = store(root.path(), kind);
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
                    board: if kind.fit() { "cx3576" } else { "uefi-x64" },
                    arch: if kind.fit() { "arm64" } else { "amd64" },
                    product: "uefi-x64-dev",
                    features: &[],
                },
                &root.path().join("objects"),
                receipt,
                None,
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
        if store.boot.is_fit() {
            let mut env = store.boot.records().unwrap();
            env.records[0].tries_left = Some(2);
            store.boot.save_records(&env).unwrap();
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
        validate(root.path(), kind);
    }
}

/// On UBI, collection removes every volume being written and every object
/// volume no retained deployment names, and nothing else.
#[test]
pub(super) fn ubi_collection_removes_unreferenced_and_pending_volumes_only() {
    let root = TempDir::new().unwrap();
    seed(root.path(), Kind::Ubi, "gc");
    let device = ubi::DirUbi::open_at(&root.path().join("store/ubi"));
    let unreachable = format!("r-{}", "f".repeat(64));
    let pending = format!("p.k-{}", "f".repeat(64));
    assert!(device.names().contains(&unreachable) && device.names().contains(&pending));
    run_operation(root.path(), Kind::Ubi, "gc");
    let names = device.names();
    assert!(!names.contains(&unreachable) && !names.contains(&pending));
    for fixed in ["system", "data", "records-a", "records-b"] {
        assert!(names.contains(&fixed.to_owned()), "{fixed}");
    }
    validate(root.path(), Kind::Ubi);
}

/// On UBI the system reserve is kept in the device's free erase blocks: an
/// installation that would eat into it is refused before anything changes.
#[test]
pub(super) fn ubi_installation_keeps_the_reserve_in_free_erase_blocks() {
    use mica_deploy::board::Reserves;
    let root = TempDir::new().unwrap();
    seed(root.path(), Kind::Ubi, "install");
    let device = ubi::DirUbi::open_at(&root.path().join("store/ubi"));
    let free = mica_deploy::ubi::UbiDevice::available_lebs(&device).unwrap();
    let store = store(root.path(), Kind::Ubi).with_reserves(Reserves {
        system: Some((free + 1) * ubi::LEB),
        ..Reserves::default()
    });
    let public: [u8; 32] = fs::read(root.path().join("public"))
        .unwrap()
        .try_into()
        .unwrap();
    let before = (
        device.names(),
        serde_json::to_vec(&store.entries().unwrap()).unwrap(),
    );
    let error = store
        .install(
            &fs::read(root.path().join("candidate.json")).unwrap(),
            &[public],
            &Target {
                board: "cx3576",
                arch: "arm64",
                product: "uefi-x64-dev",
                features: &[],
            },
            &root.path().join("objects"),
            &serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
                .unwrap(),
            None,
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("insufficient UBI space"),
        "{error:#}"
    );
    assert_eq!(
        before,
        (
            device.names(),
            serde_json::to_vec(&store.entries().unwrap()).unwrap()
        )
    );
    validate(root.path(), Kind::Ubi);
}

/// A fetch on UBI that must make room retires the fallback as installing
/// would: its descriptor and its volumes go, the confirmed running deployment
/// and its objects stay.
#[test]
pub(super) fn ubi_releasing_the_fallback_keeps_the_running_deployment() {
    let root = TempDir::new().unwrap();
    seed(root.path(), Kind::Ubi, "install");
    let store = store(root.path(), Kind::Ubi);
    let public: [u8; 32] = fs::read(root.path().join("public"))
        .unwrap()
        .try_into()
        .unwrap();
    let receipt: BootReceipt =
        serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
            .unwrap();
    let candidate = authenticate_deployment(
        &fs::read(root.path().join("candidate.json")).unwrap(),
        &[public],
    )
    .unwrap();
    let retired = fs::read_to_string(root.path().join("retired-id")).unwrap();
    let retained = fs::read_to_string(root.path().join("retained-id")).unwrap();
    let device = ubi::DirUbi::open_at(&root.path().join("store/ubi"));
    let free = mica_deploy::ubi::UbiDevice::available_lebs(&device).unwrap();
    store
        .release_fallback(&receipt, &[public], &candidate)
        .unwrap();
    let entries = store.entries().unwrap();
    assert_eq!(
        entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        [retained.as_str()]
    );
    let state = store.effective_state().unwrap();
    assert_eq!(
        (state.current.as_deref(), state.fallback),
        (Some(retained.as_str()), None)
    );
    assert!(
        !store
            .system
            .join(format!("deployments/{retired}.json"))
            .exists()
    );
    assert!(mica_deploy::ubi::UbiDevice::available_lebs(&device).unwrap() > free);
    validate(root.path(), Kind::Ubi);
    // Nothing is released while a deployment is on trial.
    let root = TempDir::new().unwrap();
    seed(root.path(), Kind::Ubi, "confirm");
    let on_trial = super::store(root.path(), Kind::Ubi);
    assert!(
        on_trial
            .release_fallback(&receipt, &[public], &candidate)
            .is_err()
    );
}

#[test]
pub(super) fn a_factory_reset_is_staged_only_over_a_confirmed_running_deployment() {
    for kind in KINDS {
        // The candidate runs on trial: nothing is staged.
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "confirm");
        let trial_store = store(root.path(), kind);
        let trial: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("candidate-receipt.json")).unwrap())
                .unwrap();
        let error = trial_store.prepare_factory_reset(&trial).unwrap_err();
        assert!(
            format!("{error:#}").contains("confirm the running deployment"),
            "{kind:?}"
        );
        // The confirmed one runs with the candidate pending: the candidate is
        // rejected, so the reset boots the running one, and the snapshot is
        // on SYSTEM before the request.
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "reset-stage");
        run_operation(root.path(), kind, "reset-stage");
        let store = store(root.path(), kind);
        let state = store.effective_state().unwrap();
        assert!(state.candidate.is_none(), "{kind:?}");
        let candidate: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("candidate-receipt.json")).unwrap())
                .unwrap();
        assert!(state.failed.contains(&candidate.deployment_id));
        validate(root.path(), kind);
        validate_reset(root.path(), kind, "reset-stage");
        let again = mica_deploy::factory_reset::stage(
            &store.system,
            &store.meta,
            &reset_request(&fs::read_to_string(root.path().join("retained-id")).unwrap()),
        );
        assert!(format!("{:#}", again.unwrap_err()).contains("staged already"));
        // Cancelling removes the request first, then the rest.
        assert!(mica_deploy::factory_reset::unstage(&store.system).unwrap());
        assert!(!store.system.join("factory-reset").exists());
        assert!(!mica_deploy::factory_reset::unstage(&store.system).unwrap());
    }
}

/// A driver extension's next build for the running kernel is a change on
/// trial; a healthy boot of it replaces the last build, whose objects are
/// then collected.
#[test]
pub(super) fn an_extension_install_replaces_the_build_and_collects_the_last() {
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "ext-install");
        validate_extensions(root.path(), kind, "ext-install", false);
        run_operation(root.path(), kind, "ext-install");
        validate(root.path(), kind);
        validate_extensions(root.path(), kind, "ext-install", true);
        // Run again, it changes nothing, not even the trial.
        let store = store(root.path(), kind);
        assert!(mica_deploy::set_state::spend_attempt(
            &store.meta,
            mica_deploy::set_state::TrialKind::Extensions,
            &mica_deploy::extensions::read_state(&store.system)
                .unwrap()
                .pending_id()
                .unwrap()
        ));
        run_operation(root.path(), kind, "ext-install");
        validate_extensions(root.path(), kind, "ext-install", true);
        assert_eq!(
            mica_deploy::set_state::read_trial(&store.meta)
                .unwrap()
                .attempts_left,
            mica_deploy::set_state::TRIAL_ATTEMPTS - 1
        );
        // A healthy boot that mapped it commits it.
        let public: [u8; 32] = fs::read(root.path().join("public"))
            .unwrap()
            .try_into()
            .unwrap();
        let running: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
                .unwrap();
        let state = mica_deploy::extensions::read_state(&store.system).unwrap();
        let trial = mica_deploy::extensions::BootTrial {
            paired: false,
            id: state.pending_id().unwrap(),
        };
        assert_eq!(
            store
                .settle_extensions(&running.deployment_id, Some(&trial), &[public])
                .unwrap(),
            mica_deploy::deployments::SetSettlement::Committed
        );
        assert!(mica_deploy::set_state::read_trial(&store.meta).is_none());
        let state = mica_deploy::extensions::read_state(&store.system).unwrap();
        assert_eq!(state.builds, state_after(&state, root.path()));
        let old = fs::read_to_string(root.path().join("ext-old-id")).unwrap();
        assert_eq!(
            store
                .collect_extensions(&std::collections::BTreeSet::new(), &[public])
                .unwrap(),
            1
        );
        assert!(!mica_deploy::extensions::object_dir(&store.system, &old).exists());
    }
}

/// The confirmed builds once the new build of `rtl8125` replaced the old.
fn state_after(
    state: &mica_deploy::extensions::State,
    root: &Path,
) -> mica_deploy::extensions::Builds {
    assert!(state.pending.is_none());
    let kernel = fs::read_to_string(root.join("ext-kernel")).unwrap();
    let new = fs::read_to_string(root.join("ext-new-id")).unwrap();
    [(kernel, [("rtl8125".to_owned(), new)].into())].into()
}

/// A system update with a new kernel waits for a build of each extension the
/// running kernel runs; given one, the build is paired with the deployment.
#[test]
pub(super) fn a_system_update_with_a_new_kernel_waits_for_its_extensions() {
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "ext-pair");
        let store = store(root.path(), kind);
        let public: [u8; 32] = fs::read(root.path().join("public"))
            .unwrap()
            .try_into()
            .unwrap();
        let running: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
                .unwrap();
        let target = Target {
            board: if kind.fit() { "cx3576" } else { "uefi-x64" },
            arch: if kind.fit() { "arm64" } else { "amd64" },
            product: "uefi-x64-dev",
            features: &[],
        };
        let candidate = fs::read(root.path().join("candidate.json")).unwrap();
        let error = store
            .install(
                &candidate,
                &[public],
                &target,
                &root.path().join("objects"),
                &running,
                None,
            )
            .unwrap_err();
        let waits = error
            .downcast_ref::<mica_deploy::deployments::WaitsForExtension>()
            .unwrap();
        let next = fs::read_to_string(root.path().join("ext-next-kernel")).unwrap();
        assert_eq!(
            waits.0,
            [mica_deploy::deployments::Wait {
                name: "rtl8125".into(),
                kernel: next.clone()
            }]
        );
        assert_eq!(
            store
                .waits_for_extension(&running.kernel_id, &next)
                .unwrap(),
            waits.0
        );
        assert!(store.effective_state().unwrap().candidate.is_none());
        // Dropped, it installs with nothing for the new kernel.
        let dropped = TempDir::new().unwrap();
        seed(dropped.path(), kind, "ext-pair");
        let other = super::store(dropped.path(), kind);
        other
            .install_with_extensions(
                &fs::read(dropped.path().join("candidate.json")).unwrap(),
                &[fs::read(dropped.path().join("public"))
                    .unwrap()
                    .try_into()
                    .unwrap()],
                &target,
                &dropped.path().join("objects"),
                &serde_json::from_slice(
                    &fs::read(dropped.path().join("current-receipt.json")).unwrap(),
                )
                .unwrap(),
                None,
                &mica_deploy::deployments::KernelExtensions {
                    descriptors: Vec::new(),
                    dropped: vec!["rtl8125".into()],
                },
            )
            .unwrap();
        let state = mica_deploy::extensions::read_state(&other.system).unwrap();
        let next = fs::read_to_string(dropped.path().join("ext-next-kernel")).unwrap();
        assert!(!state.pending.unwrap().contains_key(&next));
        // Given, it is paired; the confirmed builds stay for the running kernel.
        run_operation(root.path(), kind, "ext-pair");
        validate(root.path(), kind);
        validate_extensions(root.path(), kind, "ext-pair", true);
        let candidate_id = fs::read_to_string(root.path().join("candidate-id")).unwrap();
        // A healthy boot of the pair that mapped it commits it.
        let state = mica_deploy::extensions::read_state(&store.system).unwrap();
        let trial = mica_deploy::extensions::BootTrial {
            paired: true,
            id: state.pending_id().unwrap(),
        };
        assert_eq!(
            store
                .settle_extensions(&candidate_id, Some(&trial), &[public])
                .unwrap(),
            mica_deploy::deployments::SetSettlement::Committed
        );
        let state = mica_deploy::extensions::read_state(&store.system).unwrap();
        let new = fs::read_to_string(root.path().join("ext-new-id")).unwrap();
        assert_eq!(state.builds[&next]["rtl8125"], new);
        assert!(state.pending.is_none() && state.pair.is_none());
    }
}

#[test]
pub(super) fn a_finished_factory_reset_leaves_the_running_deployment_alone() {
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "reset-finish");
        let store = store(root.path(), kind);
        let retired = fs::read_to_string(root.path().join("retained-id")).unwrap();
        let fallback = store.effective_state().unwrap().fallback.unwrap();
        assert_ne!(fallback, retired);
        // A set the state no longer names goes with the reset.
        let sets = store.system.join("sets/core");
        let stale = "e".repeat(64);
        fs::write(sets.join(format!("{stale}.json")), b"unnamed").unwrap();
        run_operation(root.path(), kind, "reset-finish");
        validate(root.path(), kind);
        validate_reset(root.path(), kind, "reset-finish");
        assert!(
            !store
                .system
                .join(format!("deployments/{fallback}.json"))
                .exists(),
            "{kind:?}"
        );
        assert!(!sets.join(format!("{stale}.json")).exists());
        let state: Value =
            serde_json::from_slice(&fs::read(sets.join("state.json")).unwrap()).unwrap();
        assert_eq!(state.as_object().unwrap().len(), 1, "{state}");
        // Run again, it does nothing more.
        run_operation(root.path(), kind, "reset-finish");
        validate_reset(root.path(), kind, "reset-finish");
    }
}

/// With a system on trial, an extension change is for its kernel and paired
/// with it; the running kernel takes none, nor does a kernel only the
/// fallback runs, and the system on trial must not be the one running.
#[test]
pub(super) fn an_extension_change_with_a_system_on_trial_is_paired_with_it() {
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        // The candidate is installed and on trial, then the running kernel
        // gets its confirmed build.
        seed(root.path(), kind, "ext-trial");
        let store = store(root.path(), kind);
        let public: [u8; 32] = fs::read(root.path().join("public"))
            .unwrap()
            .try_into()
            .unwrap();
        let running: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
                .unwrap();
        let candidate = fs::read_to_string(root.path().join("candidate-id")).unwrap();
        assert_eq!(
            store.effective_state().unwrap().candidate.as_deref(),
            Some(candidate.as_str())
        );
        let arch = if kind.fit() { "arm64" } else { "amd64" };
        let install = |dir: &str, kernel: &str, fill: u8, booted: &str| {
            let descriptor = extension(root.path(), dir, kernel, arch, fill);
            store.install_extension(
                &descriptor,
                &root.path().join(dir).join("ext.img"),
                &root.path().join(dir).join("ext.roothash.p7s"),
                &[public],
                booted,
            )
        };
        for kernel in ["ext-kernel", "fallback-kernel"] {
            let kernel = fs::read_to_string(root.path().join(kernel)).unwrap();
            let error = install("ext-3", &kernel, 52, &running.deployment_id)
                .unwrap_err()
                .to_string();
            assert!(error.contains("only for the running kernel"), "{error}");
        }
        let next = fs::read_to_string(root.path().join("ext-next-kernel")).unwrap();
        let error = install("ext-3", &next, 52, &candidate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("confirm the running deployment"), "{error}");
        let id = install("ext-3", &next, 52, &running.deployment_id).unwrap();
        let state = mica_deploy::extensions::read_state(&store.system).unwrap();
        assert_eq!(state.paired(), Some(candidate.as_str()));
        assert_eq!(state.pending.as_ref().unwrap()[&next]["rtl8125"], id);
        assert!(
            mica_deploy::set_state::read_trial(&store.meta).is_none(),
            "a pair's trial is its deployment's"
        );
        // The runkit maps it on a boot of the pair alone.
        let (builds, trial) =
            mica_deploy::extensions::boot_choice(&state, &candidate, &next, |_| unreachable!());
        assert_eq!(
            (builds["rtl8125"].clone(), trial.unwrap().paired),
            (id, true)
        );
        let kernel = fs::read_to_string(root.path().join("ext-kernel")).unwrap();
        let (builds, trial) = mica_deploy::extensions::boot_choice(
            &state,
            &running.deployment_id,
            &kernel,
            |_| unreachable!(),
        );
        assert_eq!(
            (builds["rtl8125"].clone(), trial),
            (
                fs::read_to_string(root.path().join("ext-old-id")).unwrap(),
                None
            )
        );
        // A confirmed boot of another deployment leaves the pair to its own.
        assert_eq!(
            store
                .settle_extensions(&running.deployment_id, None, &[public])
                .unwrap(),
            mica_deploy::deployments::SetSettlement::Unchanged
        );
    }
}
