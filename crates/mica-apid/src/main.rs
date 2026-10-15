//! `mica-apid` for development, the end-to-end tests and `--openapi`.
//!
//! A device does not run this executable: it runs `micad api`, which calls
//! the same entry point.

#![forbid(unsafe_code)]

fn main() -> anyhow::Result<()> {
    mica_apid::main()
}
