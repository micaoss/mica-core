//! `micad`, the settings and reconciliation daemon; `micad apid`, the HTTPS
//! API daemon; `micad update`, the operator's update command.
//!
//! One executable because the two daemons already are one version --
//! `mica-apid` depends on `micad` at exactly its version, since both speak
//! `com.mica.micad1` from one commit -- and link the same stack. What keeps
//! the daemons apart is their units and their sandboxing, not their files:
//! `/usr/bin/mica-apid` and `/usr/bin/mica-update` are symlinks to this
//! executable, the same as `micad apid` and `micad update`.

#![forbid(unsafe_code)]

const USAGE: &str = "usage: micad [--version]\n       micad apid\n       micad update [status|check|install|rollback] [--reboot]";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let name = args
        .first()
        .and_then(|argv0| std::path::Path::new(argv0).file_name())
        .and_then(|name| name.to_str());
    // The exact link names first; every other name is micad.
    match name {
        Some("mica-apid") => return mica_apid::main(),
        Some("mica-update") => return micad::update_cli::main(&args[1..]),
        _ => {}
    }
    match args.get(1).map(String::as_str) {
        Some("apid" | "api") => mica_apid::main(),
        Some("update") => micad::update_cli::main(&args[2..]),
        // Only the bare name or a version question starts down the daemon's
        // path: a mistyped subcommand must not start a second micad.
        None | Some("--version" | "-V") => micad::main(),
        Some(other) => {
            eprintln!("micad: unknown command: {other}\n{USAGE}");
            std::process::exit(2);
        }
    }
}
