//! An unconfirmed running trial.

use mica_deploy::deployments::{BootBackend, BootReceipt, Target};
use std::fs;
use tempfile::TempDir;

use super::*;

#[test]
pub(super) fn an_unconfirmed_running_trial_cannot_retire_the_other_deployment() {
    for fit in [false, true] {
        let root = TempDir::new().unwrap();
        seed(root.path(), fit, "install");
        let store = store(root.path(), fit);
        let current: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
                .unwrap();
        if let BootBackend::Fit { firmware, .. } = &store.boot {
            let mut env =
                mica_deploy::fit_env::Environment::load(firmware, boards::layout("cx3576"))
                    .unwrap();
            env.records
                .iter_mut()
                .for_each(|entry| entry.tries_left = Some(2));
            env.save(firmware).unwrap();
        } else {
            for entry in store.entries().unwrap() {
                fs::rename(
                    root.path()
                        .join("store/esp/loader/entries")
                        .join(entry.file),
                    root.path().join(format!(
                        "store/esp/loader/entries/mica-{}+2-1.conf",
                        entry.id
                    )),
                )
                .unwrap();
            }
        }
        let public: [u8; 32] = fs::read(root.path().join("public"))
            .unwrap()
            .try_into()
            .unwrap();
        let envelope = fs::read(root.path().join("candidate.json")).unwrap();
        let entries = serde_json::to_vec(&store.entries().unwrap()).unwrap();
        let error = store
            .install(
                &envelope,
                &[public],
                &Target {
                    board: if fit { "cx3576" } else { "uefi-x64" },
                    arch: if fit { "arm64" } else { "amd64" },
                    product: "uefi-x64-prod",
                },
                &root.path().join("objects"),
                &current,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deployment targets another product")
        );
        let error = store
            .install(
                &envelope,
                &[public],
                &Target {
                    board: if fit { "cx3576" } else { "uefi-x64" },
                    arch: if fit { "arm64" } else { "amd64" },
                    product: "uefi-x64-dev",
                },
                &root.path().join("objects"),
                &current,
            )
            .unwrap_err();
        assert!(error.to_string().contains("confirm the running deployment"));
        assert_eq!(
            entries,
            serde_json::to_vec(&store.entries().unwrap()).unwrap()
        );
        validate(root.path(), fit);
    }
}
