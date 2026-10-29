//! `mica-deploy --version` answers before the boot receipt or any policy is
//! read, as image checks ask every core program.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

#[test]
fn version_prints_the_package_version() {
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_mica-deploy"))
            .arg(flag)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("mica-deploy {}\n", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
}
