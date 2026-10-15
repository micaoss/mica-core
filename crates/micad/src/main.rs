//! `micad`, the settings and reconciliation daemon; `micad api`, the HTTPS API
//! daemon; `micad update`, the operator's update command.
//!
//! One executable because the two daemons already are one version --
//! `mica-apid` depends on `micad` at exactly its version, since both speak
//! `com.mica.micad1` from one commit -- and link the same stack. What keeps
//! the daemons apart is their units and their sandboxing, not their files.

#![forbid(unsafe_code)]

const USAGE: &str = "usage: micad [--version]\n       micad api\n       micad update [status|check|install|rollback] [--reboot]";

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("api") => mica_apid::main(),
        Some("update") => micad::update_cli::main(),
        // Only the bare name or a version question starts down the daemon's
        // path: a mistyped subcommand must not start a second micad.
        None | Some("--version" | "-V") => micad::main(),
        Some(other) => {
            eprintln!("micad: unknown command: {other}\n{USAGE}");
            std::process::exit(2);
        }
    }
}
