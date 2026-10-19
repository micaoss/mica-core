//! An unconfirmed running trial.

use mica_deploy::{
    components::RootFacts,
    core_set::{self, BootRoot, CoreBoot},
    deployments::{BootReceipt, SetSettlement, Target},
    set_state,
    update_set::SetKind,
};
use std::fs;
use tempfile::TempDir;

use super::*;

#[test]
pub(super) fn an_unconfirmed_running_trial_cannot_retire_the_other_deployment() {
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "install");
        let store = store(root.path(), kind);
        let current: BootReceipt =
            serde_json::from_slice(&fs::read(root.path().join("current-receipt.json")).unwrap())
                .unwrap();
        if store.boot.is_fit() {
            let mut env = store.boot.records().unwrap();
            env.records
                .iter_mut()
                .for_each(|entry| entry.tries_left = Some(2));
            store.boot.save_records(&env).unwrap();
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
                    board: if kind.fit() { "cx3576" } else { "uefi-x64" },
                    arch: if kind.fit() { "arm64" } else { "amd64" },
                    product: "uefi-x64-prod",
                    features: &[],
                },
                &root.path().join("objects"),
                &current,
                None,
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
                    board: if kind.fit() { "cx3576" } else { "uefi-x64" },
                    arch: if kind.fit() { "arm64" } else { "amd64" },
                    product: "uefi-x64-dev",
                    features: &[],
                },
                &root.path().join("objects"),
                &current,
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("confirm the running deployment"));
        assert_eq!(
            entries,
            serde_json::to_vec(&store.entries().unwrap()).unwrap()
        );
        validate(root.path(), kind);
    }
}

/// A system update whose image carries a newer general set than the device's
/// installs that set with the deployment as one pair: composed when the new
/// deployment boots, committed by its confirmation, and the deployment that
/// ran the old set bound to it.
#[test]
pub(super) fn a_system_update_installs_its_core_set_as_one_pair() {
    for kind in KINDS {
        let root = TempDir::new().unwrap();
        seed(root.path(), kind, "pair");
        let store = store(root.path(), kind);
        let public: [u8; 32] = fs::read(root.path().join("public"))
            .unwrap()
            .try_into()
            .unwrap();
        let read = |name: &str| -> BootReceipt {
            serde_json::from_slice(&fs::read(root.path().join(name)).unwrap()).unwrap()
        };
        let (candidate, current) = (read("candidate-receipt.json"), read("current-receipt.json"));
        let paired = fs::read_to_string(root.path().join("paired-set")).unwrap();
        let state = set_state::read_state(&store.system, SetKind::Core).unwrap();
        let seed_set = state.current.clone().unwrap();
        assert_eq!(state.pending.as_deref(), Some(paired.as_str()), "{kind:?}");
        assert_eq!(state.pair.as_ref(), Some(&candidate.deployment_id));
        assert!(
            set_state::read_trial(&store.meta).is_none(),
            "no trial of its own"
        );

        let device =
            (kind == Kind::Ubi).then(|| ubi::DirUbi::open_at(&root.path().join("store/ubi")));
        // The seed set's objects are not on SYSTEM here, so it is composed
        // selecting nothing; the pair's were published with the deployment.
        let micad = vec!["micad".to_owned()];
        let compose = |deployment: &str, features: &[String]| {
            core_set::composition(
                &store.system,
                &store.meta,
                device
                    .as_ref()
                    .map(|device| device as &dyn mica_deploy::ubi::UbiDevice),
                &[public],
                &BootRoot {
                    arch: if kind.fit() { "arm64" } else { "amd64" },
                    features,
                    root: RootFacts {
                        level: 1,
                        provides: None,
                    },
                },
                deployment,
                |_| {},
            )
            .unwrap()
        };
        assert_eq!(compose(&current.deployment_id, &[]).id, seed_set);
        let composed = compose(&candidate.deployment_id, &micad);
        assert_eq!(
            (composed.id.as_str(), composed.pending, composed.paired),
            (paired.as_str(), true, true)
        );

        store.confirm(&candidate).unwrap();
        let boot = CoreBoot {
            core_set_id: paired.clone(),
            pending: true,
            paired: true,
        };
        assert_eq!(
            store
                .settle_sets(&candidate.deployment_id, &boot, &[public])
                .unwrap(),
            SetSettlement::Committed
        );
        let state = set_state::read_state(&store.system, SetKind::Core).unwrap();
        assert_eq!(
            (state.current, state.pending, state.pair),
            (Some(paired.clone()), None, None)
        );
        assert_eq!(
            state.bound,
            [(current.deployment_id.clone(), seed_set.clone())].into()
        );
        assert_eq!(compose(&current.deployment_id, &[]).id, seed_set);
        assert_eq!(compose(&candidate.deployment_id, &micad).id, paired);
        validate(root.path(), kind);
    }
}
