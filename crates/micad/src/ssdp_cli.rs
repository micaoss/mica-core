//! `micad ssdp`, whether the device answers UPnP discovery: the switch over
//! the system bus.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::unit_cli::{MicadProxy, bus_error, wait};

const USAGE: &str = "\
usage: micad ssdp status
       micad ssdp on|off

  status  whether the responder runs, and what it announces
  on      answer UPnP discovery (access.ssdp.enabled)
  off     no responder, no socket";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Status,
    Switch(bool),
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["status"] => Ok(Command::Status),
        ["on"] => Ok(Command::Switch(true)),
        ["off"] => Ok(Command::Switch(false)),
        [] | ["help" | "-h" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown command or arguments: {other}")),
    }
}

/// `micad ssdp`'s entry point: exit 0 done, 1 failed, 2 misused.
pub fn main(args: &[String]) -> Result<()> {
    if mica_apid::wants_version(args.iter().cloned()) {
        println!("{}", mica_apid::version_line("micad"));
        return Ok(());
    }
    let command = match parse(args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(command) => command,
        Err(error) => {
            eprintln!("micad ssdp: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if let Err(error) = runtime.block_on(run(command)) {
        eprintln!("micad ssdp: {error:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run(command: Command) -> Result<()> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to the system bus")?;
    let micad = MicadProxy::new(&connection).await?;
    match command {
        Command::Status => {
            let state: Value =
                serde_json::from_str(&micad.get_state("ssdp").await.map_err(bus_error)?)?;
            print!("{}", render_status(&state));
        }
        Command::Switch(on) => {
            let task = micad
                .set_settings("access.ssdp.enabled", &json!(on).to_string())
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("access.ssdp.enabled is {on}");
        }
        Command::Help => {}
    }
    Ok(())
}

/// The switch and its source, whether it runs and why not, and the names
/// it announces while it does.
fn render_status(state: &Value) -> String {
    let mut out = format!(
        "enabled: {} ({})\nactive: {}\n",
        state["enabled"] == json!(true),
        state["source"].as_str().unwrap_or("product"),
        state["active"] == json!(true),
    );
    for (label, key) in [
        ("reason", "reason"),
        ("error", "error"),
        ("udn", "udn"),
        ("friendly name", "friendlyName"),
        ("model", "modelName"),
    ] {
        if let Some(value) = state[key].as_str() {
            out.push_str(&format!("{label}: {value}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn every_command_parses_and_a_misuse_is_named() {
        assert_eq!(parse(&words("status")), Ok(Command::Status));
        assert_eq!(parse(&words("on")), Ok(Command::Switch(true)));
        assert_eq!(parse(&words("off")), Ok(Command::Switch(false)));
        assert_eq!(parse(&[]), Ok(Command::Help));
        for misuse in ["list", "on now", "restart"] {
            assert!(parse(&words(misuse)).is_err(), "{misuse}");
        }
    }

    #[test]
    fn the_status_says_why_it_does_not_run() {
        let running = json!({
            "enabled": true, "source": "product", "active": true,
            "udn": "uuid:a8d31972-5f0f-8c1e-ae28-68c78f6b4568",
            "friendlyName": "mica-ab12", "modelName": "cx3576.full",
        });
        assert_eq!(
            render_status(&running),
            "enabled: true (product)\nactive: true\nudn: uuid:a8d31972-5f0f-8c1e-ae28-68c78f6b4568\nfriendly name: mica-ab12\nmodel: cx3576.full\n"
        );
        let held = json!({
            "enabled": true, "source": "setting", "active": false,
            "reason": "the console is not on the network", "friendlyName": "mica-ab12",
        });
        assert_eq!(
            render_status(&held),
            "enabled: true (setting)\nactive: false\nreason: the console is not on the network\nfriendly name: mica-ab12\n"
        );
    }
}
