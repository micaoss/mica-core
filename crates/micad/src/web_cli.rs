//! `mica web`, who reaches the API: `access.web.mode` over the system bus.
//!
//! The way back once the API has left the network: it needs no apid, and
//! the bus policy makes it root's.

use anyhow::Result;
use serde_json::{Value, json};

use crate::unit_cli::{MicadProxy, bus_error, wait};

const USAGE: &str = "\
usage: mica web status [--json]
       mica web network|local|off

  status   who reaches the API, and where it listens; --json prints access.web
  network  the console and the API on the network, and the local socket
  local    the API on apid's local socket alone, for root on this device
  off      no API: apid stops";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Status,
    Mode(&'static str),
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["status"] => Ok(Command::Status),
        ["network"] => Ok(Command::Mode("network")),
        ["local"] => Ok(Command::Mode("local")),
        ["off"] => Ok(Command::Mode("off")),
        [] | ["help" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown command or arguments: {other}")),
    }
}

/// `mica web`'s entry point; `args` are the words after it.
pub fn main(args: &[String], json: bool) -> Result<()> {
    let command = match parse(args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(command) => command,
        Err(error) => crate::cli::misuse("web", &error, USAGE),
    };
    crate::cli::json_reads(json, command == Command::Status, "web", USAGE);
    crate::cli::block_on(run(command, json))
}

async fn run(command: Command, json: bool) -> Result<()> {
    let connection = crate::cli::system_bus().await?;
    let micad = MicadProxy::new(&connection).await.map_err(bus_error)?;
    match command {
        Command::Status => {
            let text = micad.get_settings("access.web").await.map_err(bus_error)?;
            if json {
                println!("{text}");
            } else {
                print!("{}", render_status(&serde_json::from_str::<Value>(&text)?));
            }
        }
        Command::Mode(mode) => {
            let task = micad
                .set_settings("access.web.mode", &json!(mode).to_string())
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("access.web.mode is {mode}");
        }
        Command::Help => {}
    }
    Ok(())
}

/// The mode, and the listeners it serves when it is `network`.
fn render_status(web: &Value) -> String {
    let mode = web["mode"].as_str().unwrap_or("network");
    let mut out = format!("mode: {mode}\n");
    if mode == "network" {
        out.push_str(&format!("http: {}\n", web["httpPort"]));
        if web["httpsEnabled"] == json!(true) {
            out.push_str(&format!("https: {}\n", web["httpsPort"]));
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
        for mode in ["network", "local", "off"] {
            assert_eq!(parse(&words(mode)), Ok(Command::Mode(mode)));
        }
        assert_eq!(parse(&[]), Ok(Command::Help));
        assert!(parse(&words("on")).is_err());
        assert!(parse(&words("off now")).is_err());
    }

    #[test]
    fn the_status_names_the_listeners_only_on_the_network() {
        assert_eq!(
            render_status(
                &json!({ "mode": "network", "httpPort": 8080, "httpsEnabled": true, "httpsPort": 8443 })
            ),
            "mode: network\nhttp: 8080\nhttps: 8443\n"
        );
        assert_eq!(
            render_status(
                &json!({ "mode": "local", "httpPort": 8080, "httpsEnabled": false, "httpsPort": 8443 })
            ),
            "mode: local\n"
        );
    }
}
