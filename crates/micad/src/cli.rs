//! `mica`, the operator's one command on the device: every verb, its help,
//! its exit codes and its `--json`.
//!
//! A verb goes where its rules live: what micad decides is asked over the
//! system bus, as the console's API asks it. `mica internal` holds the build
//! and factory tools, and `mica help` does not list it.
//!
//! Exit codes: 0 done, 1 refused or failed, 2 misused, 3 micad (or apid) not
//! reached, 4 a wrong password or device code, or the backoff after them.

use std::future::Future;

use anyhow::Result;
use serde_json::{Value, json};

mod status;

/// Every verb `mica help` lists, with its line.
pub const VERBS: &[(&str, &str)] = &[
    (
        "status",
        "the device at a glance: system, update, network, time, storage",
    ),
    ("update", "look for, install or roll back a release"),
    ("container", "Compose projects and their containers"),
    ("unit", "this device's own services"),
    ("web", "who reaches the API, and where it listens"),
    ("mdns", "what the device announces over mDNS"),
    ("ssdp", "whether the device answers UPnP discovery"),
    ("reboot", "restart the device"),
    ("poweroff", "turn the device off"),
    (
        "factory-reset",
        "erase everything configured, after the administrator password",
    ),
    ("doctor", "collect a diagnostic snapshot"),
    ("help", "this list, or one command's usage"),
];

/// Done.
pub const EXIT_DONE: i32 = 0;
/// Refused or failed.
pub const EXIT_FAILED: i32 = 1;
/// Misused: an unknown verb, a missing or extra argument.
pub const EXIT_USAGE: i32 = 2;
/// The service was not reached.
pub const EXIT_UNREACHABLE: i32 = 3;
/// A wrong password or device code, or the backoff after them.
pub const EXIT_CREDENTIAL: i32 = 4;

/// The error codes that are about a credential.
const CREDENTIAL_CODES: &[&str] = &[
    "password_incorrect",
    "login_throttled",
    "invalid_credentials",
    "device_code_incorrect",
    "wrong_password",
];

/// The bus's own errors for a service that is not there to answer.
const UNREACHABLE_ERRORS: &[&str] = &[
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
    "org.freedesktop.DBus.Error.NoReply",
    "org.freedesktop.DBus.Error.Timeout",
    "org.freedesktop.DBus.Error.TimedOut",
    "org.freedesktop.DBus.Error.Disconnected",
    "org.freedesktop.DBus.Error.NoServer",
];

/// How a command ended other than done: its exit code, a code for scripts
/// and a message for people. An empty message has been printed already.
#[derive(Debug)]
pub struct Failure {
    pub exit: i32,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl Failure {
    /// A refusal whose reason the command has already printed.
    #[must_use]
    pub fn printed(code: &str) -> Self {
        Self {
            exit: EXIT_FAILED,
            code: code.to_string(),
            message: String::new(),
        }
    }
}

/// `mica`'s entry point: `args` are the words after `mica`. Returns the exit
/// code.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let words: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--json")
        .cloned()
        .collect();
    let Some((verb, rest)) = words.split_first() else {
        eprint!("{}", help());
        return EXIT_USAGE;
    };
    let result = match verb.as_str() {
        "--version" if rest.is_empty() && !json => {
            println!("{}", mica_apid::version_line("mica"));
            Ok(())
        }
        "help" | "--help" if json => misuse("help", "--json applies to commands that read", ""),
        "help" | "--help" => match rest {
            [] => {
                print!("{}", help());
                Ok(())
            }
            [verb] if verb != "help" && listed(verb) => {
                return main(&[verb.clone(), "help".to_string()]);
            }
            _ => misuse("help", "help names one command", &help()),
        },
        "status" => status::main(rest, json),
        "update" => crate::update_cli::main(rest, json),
        "container" => crate::ctr_cli::main(rest, json),
        "unit" => crate::unit_cli::main(rest, json),
        "web" => crate::web_cli::main(rest, json),
        "mdns" => crate::mdns_cli::main(rest, json),
        "ssdp" => crate::ssdp_cli::main(rest, json),
        "reboot" => power(rest, json, Power::Reboot),
        "poweroff" => power(rest, json, Power::Off),
        "factory-reset" => crate::factory_reset_cli::main(rest, json),
        "doctor" => doctor(rest, json),
        "internal" => internal(rest, json),
        other => misuse(
            "",
            &format!("unknown command: {other} (mica help lists them)"),
            "",
        ),
    };
    match result {
        Ok(()) => EXIT_DONE,
        Err(error) => report(verb, &error, json),
    }
}

/// Whether `verb` is one `mica help` lists.
fn listed(verb: &str) -> bool {
    VERBS.iter().any(|(name, _)| *name == verb)
}

/// The verb list, one line each.
#[must_use]
pub fn help() -> String {
    let mut out = String::from("usage: mica <command> [arguments] [--json]\n\n");
    for (verb, line) in VERBS {
        out.push_str(&format!("  {verb:<14} {line}\n"));
    }
    out.push_str(
        "\n  mica help <command> shows one command's usage. --json prints what a\n  \
         command reads as JSON. Exit codes: 0 done, 1 refused or failed,\n  \
         2 misused, 3 the service was not reached, 4 a wrong password or\n  \
         device code, or the backoff after them.\n",
    );
    out
}

/// Refuse a misused command line: the reason and the usage on stderr, exit 2.
pub fn misuse(command: &str, error: &str, usage: &str) -> ! {
    let name = if command.is_empty() {
        "mica".to_string()
    } else {
        format!("mica {command}")
    };
    if usage.is_empty() {
        eprintln!("{name}: {error}");
    } else {
        eprintln!("{name}: {error}\n{usage}");
    }
    std::process::exit(EXIT_USAGE);
}

/// Refuse `--json` on a command that reads nothing.
pub fn json_reads(json: bool, reads: bool, command: &str, usage: &str) {
    if json && !reads {
        misuse(command, "--json applies to commands that read", usage);
    }
}

/// Run `future` to its end on a runtime of its own.
pub fn block_on<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

/// The system bus, or a failure that says it was not reached.
pub async fn system_bus() -> Result<zbus::Connection> {
    zbus::Connection::system().await.map_err(|error| {
        Failure {
            exit: EXIT_UNREACHABLE,
            code: "unreachable".to_string(),
            message: format!("connect to the system bus: {error}"),
        }
        .into()
    })
}

/// A bus error as a [`Failure`]: micad's refusal in its own words, with the
/// code a script reads.
#[must_use]
pub fn bus_error(error: zbus::Error) -> anyhow::Error {
    bus_failure(&error).into()
}

fn bus_failure(error: &zbus::Error) -> Failure {
    let Some((name, message)) = crate::bus::error_reply(error) else {
        let exit = match error {
            zbus::Error::InputOutput(_) | zbus::Error::Address(_) | zbus::Error::Handshake(_) => {
                EXIT_UNREACHABLE
            }
            _ => EXIT_FAILED,
        };
        return Failure {
            exit,
            code: if exit == EXIT_UNREACHABLE {
                "unreachable"
            } else {
                "failed"
            }
            .to_string(),
            message: format!("micad: {error}"),
        };
    };
    // A refusal whose description is JSON `{code, message}`, as the factory
    // reset's and the drivers' are.
    if let Some(refusal) = message
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        && let (Some(code), Some(text)) = (refusal["code"].as_str(), refusal["message"].as_str())
    {
        return Failure {
            exit: exit_for(code),
            code: code.to_string(),
            message: text.to_string(),
        };
    }
    let exit = if UNREACHABLE_ERRORS.contains(&name.as_str()) {
        EXIT_UNREACHABLE
    } else {
        EXIT_FAILED
    };
    Failure {
        exit,
        code: name.rsplit('.').next().unwrap_or(&name).to_string(),
        message: message.unwrap_or(name),
    }
}

/// The exit code of an error code.
#[must_use]
pub fn exit_for(code: &str) -> i32 {
    if CREDENTIAL_CODES.contains(&code) {
        EXIT_CREDENTIAL
    } else {
        EXIT_FAILED
    }
}

/// What `error` says to a person or a script, and its exit code.
fn classify(error: &anyhow::Error) -> Failure {
    for cause in error.chain() {
        if let Some(failure) = cause.downcast_ref::<Failure>() {
            return Failure {
                exit: failure.exit,
                code: failure.code.clone(),
                message: if failure.message.is_empty() {
                    String::new()
                } else {
                    format!("{error:#}")
                },
            };
        }
        if let Some(bus) = cause.downcast_ref::<zbus::Error>() {
            let failure = bus_failure(bus);
            return Failure {
                message: format!("{error:#}"),
                ..failure
            };
        }
    }
    Failure {
        exit: EXIT_FAILED,
        code: "failed".to_string(),
        message: format!("{error:#}"),
    }
}

/// Print `error` for `verb` and return its exit code.
fn report(verb: &str, error: &anyhow::Error, json: bool) -> i32 {
    let failure = classify(error);
    if json {
        eprintln!(
            "{}",
            json!({ "error": { "code": failure.code, "message": failure.message } })
        );
    } else if !failure.message.is_empty() {
        eprintln!("mica {verb}: {}", failure.message);
    }
    failure.exit
}

#[zbus::proxy(
    interface = "com.mica.micad1",
    default_service = "com.mica.micad",
    default_path = "/com/mica/micad"
)]
pub(crate) trait Micad {
    fn get_system_info(&self) -> zbus::Result<String>;
    fn get_update_state(&self) -> zbus::Result<String>;
    fn get_network_state(&self) -> zbus::Result<String>;
    fn get_time_status(&self) -> zbus::Result<String>;
    fn get_storage_status(&self) -> zbus::Result<String>;
    fn reboot(&self) -> zbus::Result<()>;
    fn power_off(&self) -> zbus::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Power {
    Reboot,
    Off,
}

/// `mica reboot` and `mica poweroff`: micad asks the init, as the console's
/// buttons do.
fn power(args: &[String], json: bool, power: Power) -> Result<()> {
    let (command, usage) = match power {
        Power::Reboot => ("reboot", "usage: mica reboot\n\n  restart the device now"),
        Power::Off => (
            "poweroff",
            "usage: mica poweroff\n\n  turn the device off now",
        ),
    };
    match args {
        [] => {}
        [help] if help == "help" || help == "--help" => {
            println!("{usage}");
            return Ok(());
        }
        [other, ..] => misuse(command, &format!("unexpected argument: {other}"), usage),
    }
    json_reads(json, false, command, usage);
    block_on(async {
        let connection = system_bus().await?;
        let micad = MicadProxy::new(&connection).await.map_err(bus_error)?;
        match power {
            Power::Reboot => micad.reboot().await,
            Power::Off => micad.power_off().await,
        }
        .map_err(bus_error)?;
        println!(
            "{}",
            match power {
                Power::Reboot => "rebooting",
                Power::Off => "powering off",
            }
        );
        Ok(())
    })
}

/// `mica doctor`: the diagnostic snapshot.
fn doctor(args: &[String], json: bool) -> Result<()> {
    json_reads(json, false, "doctor", "");
    mica_apid::diag_main(args)
}

const INTERNAL_USAGE: &str = "\
usage: mica internal device-code
       mica internal derive-ap-key --secret FILE CODE
       mica internal board check FILE";

/// `mica internal`: the build's and the factory's tools.
fn internal(args: &[String], json: bool) -> Result<()> {
    json_reads(json, false, "internal", INTERNAL_USAGE);
    match args {
        [tool] if tool == "device-code" => crate::device_code::device_code_main(),
        [tool, rest @ ..] if tool == "derive-ap-key" => {
            crate::device_code::derive_ap_key_main(rest)
        }
        [tool, rest @ ..] if tool == "board" => crate::board::main(rest),
        _ => misuse("internal", "unknown tool", INTERNAL_USAGE),
    }
}

#[cfg(test)]
mod tests;
