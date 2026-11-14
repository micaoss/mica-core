//! `mica factory-reset`: the factory reset from the device's command line.
//!
//! It asks for the administrator password, as every trigger does, and has
//! no way around it: the password comes from the terminal, or from a file
//! (`-` for standard input) for a script.

use anyhow::Result;

use crate::unit_cli::MicadProxy;

/// What the reset does, in the usage and before the password prompt.
macro_rules! what {
    () => {
        "\
  Erase everything configured on this device: the administrator and the
  claim, settings, networks, keys, containers and all data on DATA.
  The software stays at its current version. The device reboots and erases
  before anything else runs; it then starts as it did on its first boot.
  It is not a certified wipe."
    };
}

const USAGE: &str = concat!(
    "usage: mica factory-reset [--password-file FILE]\n\n",
    what!(),
    "\n\n  The administrator password is asked for on the terminal.\n",
    "  --password-file FILE  read it from FILE instead, - for standard input"
);

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Reset { password_file: Option<String> },
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => Ok(Command::Reset {
            password_file: None,
        }),
        ["--password-file", file] => Ok(Command::Reset {
            password_file: Some((*file).to_string()),
        }),
        ["help" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown arguments: {other}")),
    }
}

/// `mica factory-reset`'s entry point; `args` are the words after it.
pub fn main(args: &[String], json: bool) -> Result<()> {
    let password_file = match parse(args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(Command::Reset { password_file }) => password_file,
        Err(error) => crate::cli::misuse("factory-reset", &error, USAGE),
    };
    crate::cli::json_reads(json, false, "factory-reset", USAGE);
    if password_file.is_none() {
        println!("{}\n", what!());
    }
    let password =
        crate::cli::secret::secret(password_file.as_deref(), "Administrator password", false)?;
    crate::cli::block_on(run(&password))
}

async fn run(password: &str) -> Result<()> {
    let connection = crate::cli::system_bus().await?;
    let micad = MicadProxy::new(&connection)
        .await
        .map_err(crate::cli::bus_error)?;
    let source = format!("uid {}", rustix::process::getuid().as_raw());
    micad
        .factory_reset(password, "cli", &source)
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
    fn there_is_no_way_around_the_password() {
        assert_eq!(
            parse(&[]),
            Ok(Command::Reset {
                password_file: None
            })
        );
        assert_eq!(
            parse(&words("--password-file -")),
            Ok(Command::Reset {
                password_file: Some("-".into())
            })
        );
        assert_eq!(parse(&words("--help")), Ok(Command::Help));
        for misuse in [
            "--yes",
            "-y",
            "-h",
            "--force",
            "--password-file",
            "--password secret",
        ] {
            assert!(parse(&words(misuse)).is_err(), "{misuse}");
        }
    }
}
