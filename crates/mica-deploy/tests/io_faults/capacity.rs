//! Capacity: reclaimed blocks and the declared reserves.

use mica_deploy::{board::Reserves, deployments::Target};
use std::fs;
use tempfile::TempDir;

use super::*;

#[test]
#[ignore = "requires the bounded tmpfs provided by tests/file-ab-faults/run.sh"]
pub(super) fn replacement_capacity_uses_reclaimed_blocks_without_a_third_version() {
    use mica_deploy::components::authenticate_deployment;
    let parent = std::env::var("MICA_TEST_SPACE_ROOT").unwrap();
    // The default SYSTEM reserve, and the one a board sized for a small part declares.
    for (declared, reserve) in [(None, 128 * 1048576), (Some(4 * 1048576), 4 * 1048576)] {
        for insufficient in [false, true] {
            let root = TempDir::new_in(&parent).unwrap();
            seed(root.path(), true, "install");
            let store = store(root.path(), true).with_reserves(Reserves {
                system: declared,
                ..Reserves::default()
            });
            let public: [u8; 32] = fs::read(root.path().join("public"))
                .unwrap()
                .try_into()
                .unwrap();
            let envelope = fs::read(root.path().join("candidate.json")).unwrap();
            let receipt = serde_json::from_slice(
                &fs::read(root.path().join("current-receipt.json")).unwrap(),
            )
            .unwrap();
            let d = authenticate_deployment(&envelope, &[public]).unwrap();
            let needed: u64 = store.object_paths(&d).iter().map(|(_, a)| a.bytes).sum();
            // The candidate's core component is new bytes that nothing reclaimed pays for.
            let core: u64 = d
                .core
                .iter()
                .map(|c| c.content.image.bytes + c.content.signature.bytes)
                .sum();
            let target = if insufficient {
                reserve - 32768
            } else {
                reserve + 16384 + core
            };
            let available = rustix::fs::statvfs(&store.system).unwrap();
            let filler = available.f_bavail * available.f_frsize - target;
            fs::write(root.path().join("filler"), vec![0_u8; filler as usize]).unwrap();
            let before = rustix::fs::statvfs(&store.system).unwrap();
            assert!(before.f_bavail * before.f_frsize < needed + reserve);
            let entries = serde_json::to_vec(&store.entries().unwrap()).unwrap();
            let result = store.install(
                &envelope,
                &[public],
                &Target {
                    board: "cx3576",
                    arch: "arm64",
                    product: "uefi-x64-dev",
                },
                &root.path().join("objects"),
                &receipt,
            );
            if insufficient {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("insufficient destination space")
                );
                assert_eq!(
                    entries,
                    serde_json::to_vec(&store.entries().unwrap()).unwrap()
                );
            } else {
                result.unwrap();
                let after = rustix::fs::statvfs(&store.system).unwrap();
                assert!(after.f_bavail * after.f_frsize >= reserve);
            }
            validate(root.path(), true);
        }
    }
}

#[test]
#[ignore = "requires the bounded tmpfs provided by tests/file-ab-faults/run.sh"]
pub(super) fn the_download_workspace_keeps_the_data_reserve() {
    use mica_deploy::acquisition::Acquisition;
    let parent = std::env::var("MICA_TEST_SPACE_ROOT").unwrap();
    // The default DATA reserve, and the one a board sized for a small part declares.
    for (declared, reserve) in [(None, 128 * 1048576_u64), (Some(4 * 1048576), 4 * 1048576)] {
        for insufficient in [false, true] {
            let root = TempDir::new_in(&parent).unwrap();
            let store = store(root.path(), false).with_reserves(Reserves {
                data: declared,
                ..Reserves::default()
            });
            let acquisition = Acquisition {
                root: root.path().join("updates"),
                store: &store,
                keys: &[],
                board: "uefi-x64",
                arch: "amd64",
                product: "uefi-x64-dev",
                max_bytes: Some(2 * 1024 * 1024),
            };
            // A probe asks for one page beside the reserve.
            let target = if insufficient {
                reserve + 4096 - 32768
            } else {
                reserve + 4096 + 65536
            };
            fs::create_dir_all(root.path().join("updates")).unwrap();
            let available = rustix::fs::statvfs(root.path()).unwrap();
            let filler = available.f_bavail * available.f_frsize - target;
            fs::write(root.path().join("filler"), vec![0_u8; filler as usize]).unwrap();
            let result = acquisition.probe();
            if insufficient {
                let error = format!("{:#}", result.expect_err("inside the reserve"));
                assert!(error.contains("DATA reserve unavailable"), "{error}");
            } else {
                result.unwrap();
            }
        }
    }
}
