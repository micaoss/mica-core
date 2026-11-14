//! The settings verbs: `get`, `set`, `hostname` and `time`, through apid's
//! local socket, so a write meets the API's checks.

use anyhow::Result;
use serde_json::{Value, json};

use super::{EXIT_FAILED, Failure, api, json_reads, misuse};

const GET_USAGE: &str = "\
usage: mica get <path> [--json]

  one setting by its dot-path (hostname, access.ssh, time.timezone, ...), as
  the API reads it: secrets redacted";

const SET_USAGE: &str = "\
usage: mica set <path> <value>

  write one setting the API writes: hostname, access.ssh.enabled,
  access.mdns.enabled, access.ssdp.enabled, access.cloudAdmins.enabled,
  container.enabled, mqtt.enabled, wifi.client.enabled, wifi.ap.mode,
  time.ntp.servers, time.timezone, logging.persistent, logging.maxSizeKiB,
  logging.remote. The value is typed by the setting: a string takes the
  argument as it is, a switch true or false, a number a number, a list or an
  object JSON; logging.persistent, logging.maxSizeKiB and logging.remote
  also take null";

const HOSTNAME_USAGE: &str = "\
usage: mica hostname [--json]
       mica hostname <name>

  show the hostname, or set it";

const TIME_USAGE: &str = "\
usage: mica time [--json]
       mica time zone [--json]
       mica time zone <zone>
       mica time zones [--json]
       mica time servers [--json]
       mica time servers <server>...

  (none)   whether the clock is synchronized, and to what
  zone     the time zone (time.timezone), or set it
  zones    every time zone the device knows
  servers  the NTP servers (time.ntp.servers), or set them";

/// What a setting's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Switch,
    Number,
    Structure,
}

/// The settings the API writes, with their kinds and whether `null` is one
/// of their values. A path absent here is typed by its current value.
const KNOWN: &[(&str, Kind, bool)] = &[
    ("hostname", Kind::Text, false),
    ("access.ssh.enabled", Kind::Switch, false),
    ("access.mdns.enabled", Kind::Switch, false),
    ("access.ssdp.enabled", Kind::Switch, false),
    ("access.cloudAdmins.enabled", Kind::Switch, false),
    ("container.enabled", Kind::Switch, false),
    ("mqtt.enabled", Kind::Switch, false),
    ("wifi.client.enabled", Kind::Switch, false),
    ("wifi.ap.mode", Kind::Text, false),
    ("time.ntp.servers", Kind::Structure, false),
    ("time.timezone", Kind::Text, false),
    ("logging.persistent", Kind::Switch, true),
    ("logging.maxSizeKiB", Kind::Number, true),
    ("logging.remote", Kind::Structure, true),
];

/// The kind of the value `current`.
fn kind_of(current: &Value) -> Option<Kind> {
    match current {
        Value::String(_) => Some(Kind::Text),
        Value::Bool(_) => Some(Kind::Switch),
        Value::Number(_) => Some(Kind::Number),
        Value::Array(_) | Value::Object(_) => Some(Kind::Structure),
        Value::Null => None,
    }
}

/// `argument` as a value of `kind` for `path`.
fn typed(path: &str, kind: Kind, nullable: bool, argument: &str) -> Result<Value, String> {
    if nullable && argument == "null" {
        return Ok(Value::Null);
    }
    match kind {
        Kind::Text => Ok(json!(argument)),
        Kind::Switch => match argument {
            "true" => Ok(json!(true)),
            "false" => Ok(json!(false)),
            _ => Err(format!("{path} takes true or false, not {argument:?}")),
        },
        Kind::Number => serde_json::from_str::<serde_json::Number>(argument)
            .map(Value::Number)
            .map_err(|_| format!("{path} takes a number, not {argument:?}")),
        Kind::Structure => serde_json::from_str::<Value>(argument)
            .ok()
            .filter(|value| value.is_array() || value.is_object())
            .ok_or_else(|| format!("{path} takes JSON, a list or an object, not {argument:?}")),
    }
}

fn refused(message: String) -> anyhow::Error {
    Failure {
        exit: EXIT_FAILED,
        code: "validation_failed".to_string(),
        message,
    }
    .into()
}

fn setting_path(path: &str) -> String {
    format!("/v1/settings/{}", api::segment(path))
}

/// Write `value` at `path` and wait for it to apply.
fn write(path: &str, value: &Value) -> Result<()> {
    api::wait(&api::send("PUT", &setting_path(path), value)?)
}

/// Print `value`: as it is with `--json`, a string bare, anything else as
/// JSON.
fn show(value: &Value, json: bool) {
    match value {
        Value::String(text) if !json => println!("{text}"),
        _ if json => println!("{value}"),
        _ => println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
    }
}

/// `mica get`.
pub fn get(args: &[String], json: bool) -> Result<()> {
    let path = match args {
        [] => misuse("get", "name a setting", GET_USAGE),
        [word] if word == "help" || word == "--help" => {
            println!("{GET_USAGE}");
            return Ok(());
        }
        [path] => path,
        _ => misuse("get", "one setting at a time", GET_USAGE),
    };
    show(&api::get(&setting_path(path))?, json);
    Ok(())
}

/// `mica set`.
pub fn set(args: &[String], json: bool) -> Result<()> {
    let (path, argument) = match args {
        [word] if word == "help" || word == "--help" => {
            println!("{SET_USAGE}");
            return Ok(());
        }
        [path, argument] => (path.as_str(), argument.as_str()),
        _ => misuse("set", "a setting and its value", SET_USAGE),
    };
    json_reads(json, false, "set", SET_USAGE);
    let (kind, nullable) = match KNOWN.iter().find(|(known, ..)| *known == path) {
        Some((_, kind, nullable)) => (*kind, *nullable),
        // An absent path answers 404 with its name.
        None => match kind_of(&api::get(&setting_path(path))?) {
            Some(kind) => (kind, false),
            None => return Err(refused(format!("{path} is not a setting mica set writes"))),
        },
    };
    let value = typed(path, kind, nullable, argument).map_err(refused)?;
    write(path, &value)?;
    println!("{path} is {value}");
    Ok(())
}

/// `mica hostname`.
pub fn hostname(args: &[String], json: bool) -> Result<()> {
    match args {
        [word] if word == "help" || word == "--help" => println!("{HOSTNAME_USAGE}"),
        [] => show(&api::get(&setting_path("hostname"))?, json),
        [name] => {
            json_reads(json, false, "hostname", HOSTNAME_USAGE);
            write("hostname", &json!(name))?;
            println!("hostname is {name}");
        }
        _ => misuse("hostname", "one name", HOSTNAME_USAGE),
    }
    Ok(())
}

/// `mica time`.
pub fn time(args: &[String], json: bool) -> Result<()> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["help" | "--help"] => println!("{TIME_USAGE}"),
        [] => {
            let status = api::get("/v1/time/status")?;
            if json {
                println!("{status}");
            } else {
                print!("{}", render_status(&status));
            }
        }
        ["zone"] => show(&api::get(&setting_path("time.timezone"))?, json),
        ["zone", zone] => {
            json_reads(json, false, "time", TIME_USAGE);
            write("time.timezone", &json!(zone))?;
            println!("time.timezone is {zone}");
        }
        ["zones"] => {
            let zones = api::get("/v1/time/zones")?;
            if json {
                println!("{zones}");
            } else {
                for zone in zones["zones"].as_array().into_iter().flatten() {
                    println!("{}", zone["id"].as_str().unwrap_or_default());
                }
            }
        }
        ["servers"] => {
            let servers = api::get(&setting_path("time.ntp.servers"))?;
            if json {
                println!("{servers}");
            } else {
                for server in servers.as_array().into_iter().flatten() {
                    println!("{}", server.as_str().unwrap_or_default());
                }
            }
        }
        ["servers", servers @ ..] => {
            json_reads(json, false, "time", TIME_USAGE);
            write("time.ntp.servers", &json!(servers))?;
            println!("time.ntp.servers is {}", servers.join(" "));
        }
        _ => misuse("time", "unknown command or arguments", TIME_USAGE),
    }
    Ok(())
}

/// The synchronization state, its server and its offset.
fn render_status(status: &Value) -> String {
    let mut out = format!(
        "status: {}\n",
        status["status"].as_str().unwrap_or("unknown")
    );
    if let Some(synchronized) = status["synchronized"].as_bool() {
        out.push_str(&format!("synchronized: {synchronized}\n"));
    }
    if let Some(server) = status["server"]["name"]
        .as_str()
        .or(status["server"]["address"].as_str())
    {
        out.push_str(&format!("server: {server}\n"));
    }
    if let Some(offset) = status
        .pointer("/sample/offsetSeconds")
        .and_then(Value::as_f64)
    {
        out.push_str(&format!("offset: {offset} s\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value is typed by its setting: a string takes the argument as it is,
    /// a switch only true or false, a structure only JSON.
    #[test]
    fn a_value_is_typed_by_its_setting() {
        assert_eq!(
            typed("hostname", Kind::Text, false, "1234"),
            Ok(json!("1234"))
        );
        assert_eq!(
            typed("hostname", Kind::Text, false, "true"),
            Ok(json!("true"))
        );
        assert_eq!(
            typed("hostname", Kind::Text, false, "null"),
            Ok(json!("null"))
        );
        assert_eq!(
            typed("container.enabled", Kind::Switch, false, "true"),
            Ok(json!(true))
        );
        for wrong in ["yes", "1", "on", "True", "null"] {
            assert!(
                typed("container.enabled", Kind::Switch, false, wrong).is_err(),
                "{wrong}"
            );
        }
        assert_eq!(
            typed("logging.persistent", Kind::Switch, true, "null"),
            Ok(Value::Null)
        );
        assert_eq!(
            typed("logging.maxSizeKiB", Kind::Number, true, "512"),
            Ok(json!(512))
        );
        assert!(typed("logging.maxSizeKiB", Kind::Number, true, "lots").is_err());
        assert_eq!(
            typed(
                "time.ntp.servers",
                Kind::Structure,
                false,
                r#"["pool.ntp.org"]"#
            ),
            Ok(json!(["pool.ntp.org"]))
        );
        assert!(typed("time.ntp.servers", Kind::Structure, false, "pool.ntp.org").is_err());
        assert!(typed("time.ntp.servers", Kind::Structure, false, "\"x\"").is_err());
    }

    /// Every setting the API writes is known here; another path is typed by
    /// what it holds.
    #[test]
    fn the_known_settings_are_the_apis() {
        assert_eq!(KNOWN.len(), 14);
        assert_eq!(kind_of(&json!("eth0")), Some(Kind::Text));
        assert_eq!(kind_of(&json!(false)), Some(Kind::Switch));
        assert_eq!(kind_of(&json!(3)), Some(Kind::Number));
        assert_eq!(kind_of(&json!({})), Some(Kind::Structure));
        assert_eq!(kind_of(&Value::Null), None);
    }
}
