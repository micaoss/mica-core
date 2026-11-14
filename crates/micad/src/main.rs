//! `mica`, the operator's command (`micad::cli`), and the two daemons as
//! their units start them: `micad daemon`, the settings and reconciliation
//! daemon, and `mica-apid` (or `micad apid`), the HTTPS API daemon.
//!
//! One executable because the two daemons already are one version --
//! `mica-apid` depends on `micad` at exactly its version, since both speak
//! `com.mica.micad1` from one commit -- and link the same stack. What keeps
//! the daemons apart is their units and their sandboxing, not their files:
//! `/usr/bin/mica` and `/usr/bin/mica-apid` are symlinks to this executable.

#![forbid(unsafe_code)]

const USAGE: &str = "usage: micad daemon\n       micad apid\n       micad --version\n\nThe operator's command is mica (mica help).";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let name = args
        .first()
        .and_then(|argv0| std::path::Path::new(argv0).file_name())
        .and_then(|name| name.to_str());
    // The exact link names first; every other name is micad.
    match name {
        Some("mica-apid") => return mica_apid::main(),
        Some("mica") => std::process::exit(micad::cli::main(&args[1..])),
        _ => {}
    }
    match args.get(1).map(String::as_str) {
        Some("apid" | "api") => mica_apid::main(),
        // The daemon starts only when asked by name, as its units do: a bare
        // `micad` or a mistyped subcommand must not start a second one.
        Some("daemon") if args.len() == 2 => micad::main(),
        Some("--version" | "-V") => micad::main(),
        Some("daemon") => {
            eprintln!("micad: daemon takes no arguments\n{USAGE}");
            std::process::exit(2);
        }
        None => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
        // The operator's verbs are mica's.
        Some(other) => {
            eprintln!("micad: use mica {other} (mica help)\n{USAGE}");
            std::process::exit(2);
        }
    }
}
