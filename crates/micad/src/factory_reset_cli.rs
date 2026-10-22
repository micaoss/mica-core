//! `micad factory-reset`: the factory reset from the device's command line.
//!
//! It asks for the administrator password, as every trigger does, and has
//! no way around it: the password comes from the terminal, or from a file
//! (`-` for standard input) for a script.

use std::io::{BufRead, Read, Write};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::unit_cli::MicadProxy;

/// What the reset does, in the usage and before the password prompt.
macro_rules! what {
    () => {
        "\
  Erase everything configured on this device: the administrator and the
  claim, settings, networks, keys, apps, containers and all data on DATA.
  The software stays at its current version. The device reboots and erases
  before anything else runs; it then starts as it did on its first boot.
  It is not a certified wipe."
    };
}

const USAGE: &str = concat!(
    "usage: micad factory-reset [--password-file FILE]\n\n",
    what!(),
    "\n\n  The administrator password is asked for on the terminal.\n",
    "  --password-file FILE  read it from FILE instead, - for standard input"
);

/// The largest password file read.
const MAX_PASSWORD: u64 = 4096;

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
        ["help" | "-h" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown arguments: {other}")),
    }
}

/// `micad factory-reset`'s entry point: exit 0 staged, 1 refused or
/// failed, 2 misused.
pub fn main(args: &[String]) -> Result<()> {
    if mica_apid::wants_version(args.iter().cloned()) {
        println!("{}", mica_apid::version_line("micad"));
        return Ok(());
    }
    let password_file = match parse(args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(Command::Reset { password_file }) => password_file,
        Err(error) => {
            eprintln!("micad factory-reset: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let outcome = password(password_file.as_deref()).and_then(|password| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run(&password))
    });
    if let Err(error) = outcome {
        eprintln!("micad factory-reset: {error:#}");
        std::process::exit(1);
    }
    Ok(())
}

/// The password from `file`, or from the terminal without echo.
fn password(file: Option<&str>) -> Result<String> {
    let text = match file {
        Some("-") => read_limited(std::io::stdin().lock())?,
        Some(file) => {
            read_limited(std::fs::File::open(file).with_context(|| format!("open {file}"))?)
                .with_context(|| format!("read {file}"))?
        }
        None => prompt()?,
    };
    let password = text.strip_suffix('\n').map_or(text.as_str(), |line| {
        line.strip_suffix('\r').unwrap_or(line)
    });
    if password.is_empty() {
        bail!("no password given");
    }
    Ok(password.to_string())
}

fn read_limited(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader.take(MAX_PASSWORD + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PASSWORD {
        bail!("the password is longer than {MAX_PASSWORD} bytes");
    }
    String::from_utf8(bytes).context("the password is not UTF-8")
}

/// Print what the reset does, and read the password from the terminal with
/// its echo off.
fn prompt() -> Result<String> {
    let stdin = std::io::stdin();
    if !rustix::termios::isatty(&stdin) {
        bail!("standard input is not a terminal; give the password with --password-file");
    }
    println!("{}", what!());
    print!("\nAdministrator password: ");
    std::io::stdout().flush()?;
    let saved = rustix::termios::tcgetattr(&stdin)?;
    let mut quiet = saved.clone();
    quiet.local_modes.remove(rustix::termios::LocalModes::ECHO);
    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &quiet)?;
    let mut line = String::new();
    let read = stdin.lock().take(MAX_PASSWORD + 1).read_line(&mut line);
    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &saved)?;
    println!();
    read?;
    Ok(line)
}

async fn run(password: &str) -> Result<()> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to the system bus")?;
    let micad = MicadProxy::new(&connection).await?;
    let source = format!("uid {}", rustix::process::getuid().as_raw());
    micad
        .factory_reset(password, "cli", &source)
        .await
        .map_err(refusal)?;
    println!(
        "The factory reset is staged. The device reboots in a few seconds, erases everything \
         configured and starts as on its first boot."
    );
    Ok(())
}

/// micad's refusal in its own words: the message of a `FactoryResetRefused`
/// reply, or the reply as the other commands show it.
fn refusal(error: zbus::Error) -> anyhow::Error {
    if let Some((name, Some(description))) = crate::bus::error_reply(&error)
        && name == crate::bus::FACTORY_RESET_REFUSED_ERROR
        && let Ok(value) = serde_json::from_str::<Value>(&description)
        && let Some(message) = value["message"].as_str()
    {
        return anyhow::anyhow!("{message}");
    }
    crate::unit_cli::bus_error(error)
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
            "--force",
            "--password-file",
            "--password secret",
        ] {
            assert!(parse(&words(misuse)).is_err(), "{misuse}");
        }
    }

    #[test]
    fn a_password_file_is_one_line_of_bounded_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("password");
        for (text, want) in [
            ("secret\n", Some("secret")),
            ("secret\r\n", Some("secret")),
            ("with spaces ", Some("with spaces ")),
            ("\n", None),
            ("", None),
        ] {
            std::fs::write(&file, text).unwrap();
            let read = password(Some(file.to_str().unwrap()));
            assert_eq!(read.ok().as_deref(), want, "{text:?}");
        }
        std::fs::write(&file, "x".repeat(4097)).unwrap();
        assert!(password(Some(file.to_str().unwrap())).is_err());
    }
}
