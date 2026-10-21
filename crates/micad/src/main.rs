//! `micad`, the settings and reconciliation daemon; `micad apid`, the HTTPS
//! API daemon; `micad update`, the operator's update command; `micad ctr`, the
//! operator's containers command; `micad unit`, the operator's command for
//! their own services; `micad web`, who reaches the API; `micad diag`, a
//! diagnostic snapshot.
//!
//! One executable because the two daemons already are one version --
//! `mica-apid` depends on `micad` at exactly its version, since both speak
//! `com.mica.micad1` from one commit -- and link the same stack. What keeps
//! the daemons apart is their units and their sandboxing, not their files:
//! `/usr/bin/mica-apid`, `/usr/bin/mica-update` and `/usr/bin/mica-ctr` are
//! symlinks to this executable, the same as `micad apid`, `micad update` and
//! `micad ctr`.

#![forbid(unsafe_code)]

const USAGE: &str = "usage: micad [--version]\n       micad apid\n       micad update [status|check|install|rollback] [--reboot]\n       micad ctr <command> (micad ctr help)\n       micad unit <command> (micad unit help)\n       micad web status|network|local|off\n       micad diag\n       micad device-code\n       micad derive-ap-key --secret FILE CODE";

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
        Some("mica-ctr") => return micad::ctr_cli::main(&args[1..]),
        _ => {}
    }
    match args.get(1).map(String::as_str) {
        Some("apid" | "api") => mica_apid::main(),
        Some("update") => micad::update_cli::main(&args[2..]),
        Some("ctr") => micad::ctr_cli::main(&args[2..]),
        Some("unit") => micad::unit_cli::main(&args[2..]),
        Some("web") => micad::web_cli::main(&args[2..]),
        Some("diag") => mica_apid::diag_main(&args[2..]),
        Some("device-code") => micad::device_code::device_code_main(),
        Some("derive-ap-key") => micad::device_code::derive_ap_key_main(&args[2..]),
        // Only the bare name or a version question starts down the daemon's
        // path: a mistyped subcommand must not start a second micad.
        None | Some("--version" | "-V") => micad::main(),
        Some(other) => {
            eprintln!("micad: unknown command: {other}\n{USAGE}");
            std::process::exit(2);
        }
    }
}
