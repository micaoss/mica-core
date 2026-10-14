//! `micad`, the settings and reconciliation daemon, and, started as
//! `mica-apid`, the HTTPS API daemon.
//!
//! One executable because the two already are one version -- `mica-apid`
//! depends on `micad` at exactly its version, since both speak
//! `com.mica.micad1` from one commit -- and link the same stack. What keeps
//! the daemons apart is their units and their sandboxing, not their files:
//! `/usr/bin/mica-apid` is a symlink to this executable.

#![forbid(unsafe_code)]

fn main() -> anyhow::Result<()> {
    let argv0 = std::env::args_os().next().unwrap_or_default();
    // The exact basename selects apid; every other name is micad, which is the
    // answer that cannot open a network listener by accident.
    if std::path::Path::new(&argv0).file_name() == Some(std::ffi::OsStr::new("mica-apid")) {
        mica_apid::main()
    } else {
        micad::main()
    }
}
