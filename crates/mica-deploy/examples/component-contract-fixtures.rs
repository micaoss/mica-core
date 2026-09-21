//! Regenerate `tests/component-contracts/` in place:
//!
//! ```text
//! cargo run -p mica-deploy --example component-contract-fixtures -- \
//!     crates/mica-deploy/tests/component-contracts
//! ```
//!
//! It reads the unsigned inputs from the directory's `cases.json` and
//! `firmware.json` and writes every file, signed with the TEST-ONLY keys
//! `tests/support/contract_fixtures.rs` derives from its labels. Running it
//! twice changes nothing; `tests/contract_fixtures.rs` holds the committed
//! files to that.

#![forbid(unsafe_code)]

#[path = "../tests/support/contract_fixtures.rs"]
mod contract_fixtures;

use std::{fs, path::PathBuf};

fn main() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: component-contract-fixtures <directory>"))?,
    );
    let read = |name: &str| -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::from_str(&fs::read_to_string(
            directory.join(name),
        )?)?)
    };
    let generated = contract_fixtures::generate(&read("cases.json")?, &read("firmware.json")?);
    fs::write(directory.join("deployment.json"), generated.deployment)?;
    fs::write(directory.join("envelope.json"), generated.envelope)?;
    fs::write(directory.join("firmware.json"), generated.firmware)?;
    fs::write(directory.join("cases.json"), generated.cases)?;
    fs::write(directory.join("catalog.json"), generated.catalog)?;
    fs::write(directory.join("chunker.json"), generated.chunker)?;
    Ok(())
}
