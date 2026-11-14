//! `mica factory-reset`: the factory reset from the device's command line.
//!
//! Root on the device needs no administrator password: micad takes the
//! caller's uid from the bus. One confirmation guards against a slip: the
//! hostname or `factory-reset` typed on the terminal, or `--yes`.

use anyhow::Result;

use crate::cli::{EXIT_FAILED, Failure};
use crate::unit_cli::MicadProxy;

/// What the reset erases and what it keeps, in the usage and before the
/// confirmation.
macro_rules! what {
    () => {
        "\
  Erases everything configured on this device: the administrator and the
  claim, settings, networks, keys, containers and all data on DATA.
  Keeps the software at its current version. The device reboots and erases
  before anything else runs; it then starts as it did on its first boot.
  It is not a certified wipe."
    };
}

const USAGE: &str = concat!(
    "usage: mica factory-reset [--yes]\n\n",
    what!(),
    "\n\n  Confirm by typing the hostname or factory-reset on the terminal.\n",
    "  --yes  confirm without asking, for a script"
);

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Reset { yes: bool },
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => Ok(Command::Reset { yes: false }),
        ["--yes"] => Ok(Command::Reset { yes: true }),
        ["help" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown arguments: {other}")),
    }
}

/// Whether `typed` confirms the reset of the device named `hostname`.
fn confirms(typed: &str, hostname: &str) -> bool {
    let typed = typed.trim();
    typed == "factory-reset" || (!typed.is_empty() && typed == hostname)
}

/// `mica factory-reset`'s entry point; `args` are the words after it.
pub fn main(args: &[String], json: bool) -> Result<()> {
    let yes = match parse(args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(Command::Reset { yes }) => yes,
        Err(error) => crate::cli::misuse("factory-reset", &error, USAGE),
    };
    crate::cli::json_reads(json, false, "factory-reset", USAGE);
    if !yes && !rustix::termios::isatty(std::io::stdin()) {
        crate::cli::misuse(
            "factory-reset",
            "standard input is not a terminal; confirm with --yes",
            USAGE,
        );
    }
    println!("{}\n", what!());
    if !yes {
        let hostname = rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned();
        let typed = crate::cli::secret::plain(&format!(
            "Type the hostname ({hostname}) or factory-reset to confirm"
        ))?;
        if !confirms(&typed, &hostname) {
            return Err(Failure {
                exit: EXIT_FAILED,
                code: "not_confirmed".to_string(),
                message: "not confirmed; nothing was reset".to_string(),
            }
            .into());
        }
    }
    crate::cli::block_on(run())
}

/// Ask micad for the reset with no password: it holds root to its uid.
async fn run() -> Result<()> {
    let connection = crate::cli::system_bus().await?;
    let micad = MicadProxy::new(&connection)
        .await
        .map_err(crate::cli::bus_error)?;
    let source = format!("uid {}", rustix::process::getuid().as_raw());
    micad
        .factory_reset("", "cli", &source)
        .await
        .map_err(crate::cli::bus_error)?;
    println!(
        "The factory reset is staged. The device reboots in a few seconds, erases everything \
         configured and starts as on its first boot."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_reset_asks_no_password_and_takes_one_confirmation() {
        assert_eq!(parse(&[]), Ok(Command::Reset { yes: false }));
        assert_eq!(parse(&words("--yes")), Ok(Command::Reset { yes: true }));
        assert_eq!(parse(&words("--help")), Ok(Command::Help));
        for misuse in [
            "-y",
            "-h",
            "--force",
            "--password-file -",
            "--yes --yes",
            "--password secret",
        ] {
            assert!(parse(&words(misuse)).is_err(), "{misuse}");
        }
    }

    #[test]
    fn the_hostname_or_the_command_confirms() {
        assert!(confirms("factory-reset\n", "boat"));
        assert!(confirms(" boat ", "boat"));
        for typed in ["", "y", "yes", "Boat", "factory reset"] {
            assert!(!confirms(typed, "boat"), "{typed:?}");
        }
        assert!(!confirms("", ""));
    }
}
