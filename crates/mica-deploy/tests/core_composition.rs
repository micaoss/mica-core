//! Composing core components over the root: names, overlay arguments, shadowing.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mica_deploy::boot::{check_composition, mica_mapping, startup::native::overlay_data};
use std::{fs, os::unix::fs::symlink, path::Path};

fn file(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "x").unwrap();
}

#[test]
fn the_mappings_startup_owns_are_the_root_the_support_and_the_core_components() {
    for name in [
        "mica-root",
        "mica-support",
        "mica-core-micad",
        "mica-core-mica-apid-ui",
    ] {
        assert!(mica_mapping(name), "{name}");
    }
    for name in [
        "mica-core-",
        "mica-core-../x",
        "mica-core-a b",
        "crypt-root",
        "mica-rootfs",
        "mica-core--x",
    ] {
        assert!(!mica_mapping(name), "{name}");
    }
}

#[test]
fn an_overlay_composes_only_core_trees_over_the_roots_own() {
    let lower = |layers: &[&str]| layers.iter().map(|l| (*l).to_string()).collect::<Vec<_>>();
    assert_eq!(
        overlay_data(
            "/newroot/usr",
            &lower(&[
                "/run/mica-core/mica-apid-ui/usr",
                "/run/mica-core/micad/usr",
                "/newroot/usr"
            ])
        )
        .unwrap()
        .to_str()
        .unwrap(),
        "lowerdir=/run/mica-core/mica-apid-ui/usr:/run/mica-core/micad/usr:/newroot/usr"
    );
    for (target, layers) in [
        (
            "/newroot/var",
            vec!["/run/mica-core/micad/var", "/newroot/var"],
        ),
        ("/newroot/usr", vec!["/newroot/usr"]),
        (
            "/newroot/usr",
            vec!["/newroot/usr", "/run/mica-core/micad/usr"],
        ),
        (
            "/newroot/usr",
            vec!["/run/mica-core/micad/etc", "/newroot/usr"],
        ),
        ("/newroot/usr", vec!["/tmp/micad/usr", "/newroot/usr"]),
        (
            "/newroot/usr",
            vec!["/run/mica-core/../x/usr", "/newroot/usr"],
        ),
        (
            "/newroot/usr",
            vec![
                "/run/mica-core/micad/usr",
                "/run/mica-core/micad/usr",
                "/newroot/usr",
            ],
        ),
        (
            "/newroot/usr",
            vec!["/run/mica-core/a:b/usr", "/newroot/usr"],
        ),
    ] {
        assert!(
            overlay_data(target, &lower(&layers)).is_err(),
            "{target} {layers:?}"
        );
    }
}

#[test]
fn a_composition_is_a_union_and_never_shadows_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let micad = dir.path().join("micad");
    let ui = dir.path().join("ui");
    file(&root.join("usr/bin/sh"));
    fs::create_dir_all(root.join("etc/init.d")).unwrap();
    symlink("lib", root.join("usr/lib64")).unwrap();
    file(&micad.join("usr/bin/micad"));
    symlink("micad", micad.join("usr/bin/mica-apid")).unwrap();
    file(&micad.join("etc/init.d/micad"));
    file(&ui.join("usr/share/mica-apid/ui/index.html"));
    file(&micad.join("usr/share/mica-apid/openapi.json"));
    check_composition(&root, &[ui.clone(), micad.clone()]).unwrap();

    let refused = |component: &Path, why: &str| {
        let err = check_composition(&root, &[ui.clone(), micad.clone(), component.to_path_buf()])
            .expect_err(why)
            .to_string();
        assert!(err.contains(why), "{why}: {err}");
    };
    let shadow = dir.path().join("shadow");
    file(&shadow.join("usr/bin/sh"));
    refused(&shadow, "would shadow /usr/bin/sh of the root");
    let twice = dir.path().join("twice");
    file(&twice.join("usr/bin/micad"));
    refused(&twice, "two core components ship /usr/bin/micad");
    let over_link = dir.path().join("over-link");
    file(&over_link.join("usr/lib64/x"));
    refused(&over_link, "would shadow /usr/lib64 of the root");
    let dir_over_file = dir.path().join("dir-over-file");
    file(&dir_over_file.join("usr/bin/micad/x"));
    refused(&dir_over_file, "two core components ship /usr/bin/micad");
    let outside = dir.path().join("outside");
    file(&outside.join("opt/x"));
    refused(&outside, "outside /usr and /etc");
}
