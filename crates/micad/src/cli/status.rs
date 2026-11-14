//! `mica status`: the device at a glance, from what micad observes.

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{MicadProxy, block_on, bus_error, json_reads, misuse, system_bus};

const USAGE: &str = "\
usage: mica status [--json]

  the board, the running system and the update state, uptime, the network
  interfaces and their addresses, time synchronization and DATA's use;
  --json prints one object with micad's answers: system, update, network,
  time and storage";

/// `mica status`'s entry point.
pub fn main(args: &[String], json: bool) -> Result<()> {
    match args {
        [] => {}
        [help] if help == "help" || help == "--help" => {
            println!("{USAGE}");
            return Ok(());
        }
        [other, ..] => misuse("status", &format!("unexpected argument: {other}"), USAGE),
    }
    json_reads(json, true, "status", USAGE);
    let answers = block_on(read())?;
    if json {
        println!("{}", Value::Object(answers));
    } else {
        print!("{}", render(&answers));
    }
    Ok(())
}

/// Each member's answer. The system information must answer: without it
/// micad is not there. A later member that fails carries its error instead.
async fn read() -> Result<Map<String, Value>> {
    let connection = system_bus().await?;
    let micad = MicadProxy::new(&connection).await.map_err(bus_error)?;
    let mut answers = Map::new();
    let system = micad.get_system_info().await.map_err(bus_error)?;
    answers.insert("system".to_string(), serde_json::from_str(&system)?);
    for (name, answer) in [
        ("update", micad.get_update_state().await),
        ("network", micad.get_network_state().await),
        ("time", micad.get_time_status().await),
        ("storage", micad.get_storage_status().await),
    ] {
        let value = match answer {
            Ok(text) => serde_json::from_str(&text)?,
            Err(error) => {
                let failure = super::bus_failure(&error);
                json!({ "error": { "code": failure.code, "message": failure.message } })
            }
        };
        answers.insert(name.to_string(), value);
    }
    Ok(answers)
}

/// The answers as lines for a person.
fn render(answers: &Map<String, Value>) -> String {
    let null = Value::Null;
    let member = |name: &str| answers.get(name).unwrap_or(&null);
    let mut out = String::new();
    let system = member("system");
    if let Some(model) = system.pointer("/board/model").and_then(Value::as_str) {
        out.push_str(&format!("board      {model}\n"));
    }
    let update = member("update");
    match unavailable(update) {
        Some(why) => out.push_str(&format!("system     unavailable: {why}\n")),
        None => out.push_str(&crate::update_cli::render_status(update)),
    }
    if let Some(seconds) = system.pointer("/uptime/seconds").and_then(Value::as_u64) {
        out.push_str(&format!("uptime     {}\n", uptime(seconds)));
    }
    let network = member("network");
    match unavailable(network) {
        Some(why) => out.push_str(&format!("network    unavailable: {why}\n")),
        None => {
            for interface in network["interfaces"].as_array().into_iter().flatten() {
                let name = interface["name"].as_str().unwrap_or("?");
                if name == "lo" {
                    continue;
                }
                let state = interface
                    .pointer("/link/operationalState")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let addresses: Vec<String> = interface["addresses"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|address| {
                        let text = address["address"].as_str()?;
                        Some(match address["prefixLength"].as_u64() {
                            Some(prefix) => format!("{text}/{prefix}"),
                            None => text.to_string(),
                        })
                    })
                    .collect();
                out.push_str(&format!("network    {name} {state}"));
                if !addresses.is_empty() {
                    out.push_str(&format!(" {}", addresses.join(" ")));
                }
                out.push('\n');
            }
        }
    }
    let time = member("time");
    match unavailable(time) {
        Some(why) => out.push_str(&format!("time       unavailable: {why}\n")),
        None => out.push_str(&format!(
            "time       {}\n",
            time["status"].as_str().unwrap_or("unknown")
        )),
    }
    let storage = member("storage");
    match unavailable(storage) {
        Some(why) => out.push_str(&format!("data       unavailable: {why}\n")),
        None => {
            let data = storage["tiers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|tier| tier["name"] == json!(crate::storage_status::DATA_TIER));
            if let Some(space) = data.map(|tier| &tier["space"])
                && let (Some(used), Some(total)) =
                    (space["usedBytes"].as_u64(), space["totalBytes"].as_u64())
            {
                out.push_str(&format!(
                    "data       {} of {} used\n",
                    bytes(used),
                    bytes(total)
                ));
            }
        }
    }
    out
}

/// The message of a member that failed.
fn unavailable(member: &Value) -> Option<&str> {
    member.pointer("/error/message").and_then(Value::as_str)
}

fn uptime(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / 86400, seconds / 3600 % 24, seconds / 60 % 60);
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else {
        format!("{hours}h {minutes}m")
    }
}

fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_names_each_member_and_says_which_failed() {
        let answers: Map<String, Value> = serde_json::from_value(json!({
            "system": { "board": { "model": "cx3576" }, "uptime": { "seconds": 93784 } },
            "update": { "error": { "code": "Failed", "message": "no deployment backend" } },
            "network": { "interfaces": [
                { "name": "lo", "link": { "operationalState": "carrier" } },
                { "name": "eth0", "link": { "operationalState": "routable" },
                  "addresses": [ { "address": "192.0.2.7", "prefixLength": 24 } ] },
            ] },
            "time": { "status": "synchronized" },
            "storage": { "tiers": [
                { "name": "data", "space": { "usedBytes": 1_610_612_736_u64, "totalBytes": 8_589_934_592_u64 } },
            ] },
        }))
        .unwrap();
        assert_eq!(
            render(&answers),
            "board      cx3576\n\
             system     unavailable: no deployment backend\n\
             uptime     1d 2h 3m\n\
             network    eth0 routable 192.0.2.7/24\n\
             time       synchronized\n\
             data       1.5 GiB of 8.0 GiB used\n"
        );
    }
}
