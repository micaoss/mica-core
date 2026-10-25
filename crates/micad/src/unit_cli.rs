//! `micad unit`, the operator's command for their own services: micad over the
//! system bus.
//!
//! A service put here is stored in micad's settings (`services.units`), the
//! same set the console and the API change, and micad keeps it on DATA state
//! with Base's `mica-unit`. `mica-unit` itself stays for a hand that wants a
//! service micad does not manage. The bus policy makes it root's.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const USAGE: &str = "\
usage: micad unit install <file|-> [--name <name>] [--conf <file>] [--enable]
       micad unit enable|disable <name>
       micad unit remove <name>
       micad unit start|stop|restart <name>
       micad unit list
       micad unit logs <name> [--tail N]

  install  store a unit file (systemd) or an openrc-run script (OpenRC) as one
           of this device's services; its name is the file's unless --name
           gives one (- reads stdin and needs --name); --conf gives an OpenRC
           script its conf.d settings; --enable enables it, and a service
           already enabled stays so
  enable   start it now and at every boot
  disable  stop it now and no longer at boot
  remove   stop it and delete it
  start, stop, restart
           the service, now; managed here, put there with mica-unit or part
           of an app
  list     every service, declared here, put there with mica-unit or part of
           an app
  logs     its newest lines (200 unless --tail)

  Under systemd a name without a unit type is a .service.";

/// How often a task is read while micad applies it.
const POLL: Duration = Duration::from_millis(500);
/// How long a reconcile may take before the command stops waiting for it.
const TASK_TIMEOUT: Duration = Duration::from_secs(300);
/// The lines `logs` shows unless asked: what micad answers.
const DEFAULT_TAIL: usize = 200;
/// The largest unit file `install` reads: a unit's bound, and one byte.
const MAX_FILE: u64 = micad_settings::MAX_USER_UNIT_BYTES as u64 + 1;
/// The largest conf it reads.
const MAX_CONF: u64 = micad_settings::MAX_USER_UNIT_CONF_BYTES as u64 + 1;

#[zbus::proxy(
    interface = "com.mica.micad1",
    default_service = "com.mica.micad",
    default_path = "/com/mica/micad"
)]
pub(crate) trait Micad {
    fn get_settings(&self, path: &str) -> zbus::Result<String>;
    fn set_settings(&self, path: &str, value_json: &str) -> zbus::Result<String>;
    fn get_task(&self, id: &str) -> zbus::Result<String>;
    fn get_state(&self, path: &str) -> zbus::Result<String>;
    fn get_units(&self) -> zbus::Result<String>;
    fn get_unit_log(&self, name: &str) -> zbus::Result<String>;
    fn start_unit(&self, name: &str) -> zbus::Result<()>;
    fn stop_unit(&self, name: &str) -> zbus::Result<()>;
    fn restart_unit(&self, name: &str) -> zbus::Result<()>;
    fn factory_reset(&self, password: &str, by: &str, source: &str) -> zbus::Result<()>;
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Install {
    file: String,
    name: Option<String>,
    conf: Option<String>,
    enable: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Install(Install),
    Enable { name: String, enabled: bool },
    Remove(String),
    Verb { verb: Verb, name: String },
    List,
    Logs { name: String, tail: usize },
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Start,
    Stop,
    Restart,
}

fn install(words: &[&str]) -> Result<Install, String> {
    let mut install = Install::default();
    let mut file = None;
    let mut rest = words.iter();
    while let Some(word) = rest.next() {
        let slot = match *word {
            "--enable" => {
                install.enable = true;
                continue;
            }
            "--name" => &mut install.name,
            "--conf" => &mut install.conf,
            other if file.is_none() && (other == "-" || !other.starts_with('-')) => {
                file = Some(other.to_string());
                continue;
            }
            other => return Err(format!("unexpected argument: {other}")),
        };
        let value = rest.next().ok_or_else(|| format!("{word} needs a value"))?;
        if slot.replace((*value).to_string()).is_some() {
            return Err(format!("{word} is given twice"));
        }
    }
    install.file = file.ok_or("install needs a file, or - for stdin")?;
    if install.file == "-" && install.name.is_none() {
        return Err("a unit read from stdin needs --name".to_string());
    }
    Ok(install)
}

fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["install", rest @ ..] => Ok(Command::Install(install(rest)?)),
        [verb @ ("enable" | "disable"), name] => Ok(Command::Enable {
            name: (*name).to_string(),
            enabled: *verb == "enable",
        }),
        ["remove", name] => Ok(Command::Remove((*name).to_string())),
        [verb @ ("start" | "stop" | "restart"), name] => Ok(Command::Verb {
            verb: match *verb {
                "start" => Verb::Start,
                "stop" => Verb::Stop,
                _ => Verb::Restart,
            },
            name: (*name).to_string(),
        }),
        ["list"] => Ok(Command::List),
        ["logs", name, rest @ ..] => {
            let tail = match rest {
                [] => DEFAULT_TAIL,
                ["--tail", n] => n
                    .parse()
                    .ok()
                    .filter(|n| (1..=DEFAULT_TAIL).contains(n))
                    .ok_or_else(|| format!("--tail is 1 to {DEFAULT_TAIL} lines, not {n:?}"))?,
                _ => return Err(format!("unexpected arguments: {}", rest.join(" "))),
            };
            Ok(Command::Logs {
                name: (*name).to_string(),
                tail,
            })
        }
        [] | ["help" | "-h" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown command or arguments: {other}")),
    }
}

/// `micad unit`'s entry point: exit 0 done, 1 failed, 2 misused.
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
            eprintln!("micad unit: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if let Err(error) = runtime.block_on(run(command)) {
        eprintln!("micad unit: {error:#}");
        std::process::exit(1);
    }
    Ok(())
}

/// `name` as the init knows it: under systemd a name without a unit type is
/// a `.service`, as `mica-unit` reads it.
fn unit_name(init: &str, name: &str) -> String {
    if init == "systemd" && !name.contains('.') {
        format!("{name}.service")
    } else {
        name.to_string()
    }
}

/// The settings path of one service.
fn unit_path(name: &str) -> String {
    format!(
        "services.units.{}",
        micad_settings::quote_path_segment(name)
    )
}

async fn run(command: Command) -> Result<()> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to the system bus")?;
    let micad = MicadProxy::new(&connection).await?;
    let units: Value = serde_json::from_str(&micad.get_units().await.map_err(bus_error)?)?;
    let init = units["init"].as_str().unwrap_or("systemd").to_string();
    let declared = || async {
        let declared: serde_json::Map<String, Value> = match serde_json::from_str(
            &micad
                .get_settings("services.units")
                .await
                .map_err(bus_error)?,
        )? {
            Value::Object(units) => units,
            _ => serde_json::Map::new(),
        };
        anyhow::Ok(declared)
    };
    // An app's units are its own: `micad app` installs and removes them.
    let app_of = |name: &str| {
        units["units"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|unit| unit["name"] == json!(name))
            .and_then(|unit| unit["app"].as_str())
            .map(str::to_string)
    };
    let refuse_app = |name: &str| match app_of(name) {
        Some(app) => Err(anyhow::anyhow!(
            "{name} is part of app {app}; `micad app` installs and removes it"
        )),
        None => Ok(()),
    };
    match command {
        Command::Install(install) => {
            let text = read_input(&install.file, MAX_FILE)?;
            let name = unit_name(
                &init,
                match &install.name {
                    Some(name) => name,
                    None => Path::new(&install.file)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .context("the file names no unit; give one with --name")?,
                },
            );
            refuse_app(&name)?;
            let conf = install
                .conf
                .as_deref()
                .map(|file| read_input(file, MAX_CONF))
                .transpose()?;
            let enabled = install.enable
                || declared()
                    .await?
                    .get(&name)
                    .is_some_and(|unit| unit["enabled"] == json!(true));
            let mut unit = json!({ "kind": init, "text": text, "enabled": enabled });
            if let Some(conf) = conf {
                unit["conf"] = json!(conf);
            }
            let task = micad
                .set_settings(&unit_path(&name), &unit.to_string())
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("{name} installed{}", if enabled { ", enabled" } else { "" });
        }
        Command::Enable { name, enabled } => {
            let name = unit_name(&init, &name);
            refuse_app(&name)?;
            if !declared().await?.contains_key(&name) {
                bail!(
                    "no service named {name} is declared here; `micad unit install` it, or use mica-unit for one put there by hand"
                );
            }
            let task = micad
                .set_settings(
                    &format!("{}.enabled", unit_path(&name)),
                    &json!(enabled).to_string(),
                )
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("{name} {}", if enabled { "enabled" } else { "disabled" });
        }
        Command::Remove(name) => {
            let name = unit_name(&init, &name);
            refuse_app(&name)?;
            let mut units = declared().await?;
            if units.remove(&name).is_none() {
                bail!("no service named {name} is declared here");
            }
            let task = micad
                .set_settings("services.units", &Value::Object(units).to_string())
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("{name} removed");
        }
        Command::Verb { verb, name } => {
            let name = unit_name(&init, &name);
            match verb {
                Verb::Start => micad.start_unit(&name).await,
                Verb::Stop => micad.stop_unit(&name).await,
                Verb::Restart => micad.restart_unit(&name).await,
            }
            .map_err(bus_error)?;
        }
        Command::List => print!("{}", render_list(&units)),
        Command::Logs { name, tail } => {
            let name = unit_name(&init, &name);
            let log: Value =
                serde_json::from_str(&micad.get_unit_log(&name).await.map_err(bus_error)?)?;
            print!("{}", render_log(&log, tail)?);
        }
        Command::Help => {}
    }
    Ok(())
}

/// A file's text (`-` is stdin), refused at `limit` bytes.
fn read_input(file: &str, limit: u64) -> Result<String> {
    let mut bytes = Vec::new();
    if file == "-" {
        std::io::stdin()
            .take(limit)
            .read_to_end(&mut bytes)
            .context("read stdin")?;
    } else {
        std::fs::File::open(file)
            .with_context(|| format!("open {file}"))?
            .take(limit)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read {file}"))?;
    }
    if bytes.len() as u64 >= limit {
        bail!("{file} is larger than {} KiB", (limit - 1) / 1024);
    }
    String::from_utf8(bytes).with_context(|| format!("{file} is not UTF-8 text"))
}

/// Every service: its name, links, the init's state and who manages it.
fn render_list(units: &Value) -> String {
    let mut out = String::new();
    if units["available"] != json!(true) {
        out.push_str("this root has no mica-unit, which keeps the user's services\n");
    }
    let mut rows = vec![[
        "NAME".to_string(),
        "ENABLED".to_string(),
        "ACTIVE".to_string(),
        "MANAGED".to_string(),
    ]];
    for unit in units["units"].as_array().into_iter().flatten() {
        let managed = if let Some(app) = unit["app"].as_str() {
            format!("app {app}")
        } else if unit["managed"] == json!(true) {
            "micad".to_string()
        } else if let Some(refused) = unit["refused"].as_str() {
            format!("refused: {refused}")
        } else {
            "by hand".to_string()
        };
        rows.push([
            unit["name"].as_str().unwrap_or("-").to_string(),
            if unit["enabled"] == json!(true) {
                "yes"
            } else {
                "no"
            }
            .to_string(),
            unit["active"].as_str().unwrap_or("-").to_string(),
            managed,
        ]);
    }
    let widths: Vec<usize> = (0..4)
        .map(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// The newest `tail` lines of a `GetUnitLog` answer.
fn render_log(log: &Value, tail: usize) -> Result<String> {
    if log["available"] != json!(true) {
        bail!(
            "{}",
            log["detail"].as_str().unwrap_or("the log is unavailable")
        );
    }
    let lines: Vec<&str> = log["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    Ok(lines[lines.len().saturating_sub(tail)..]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect())
}

/// Wait until micad has applied `task`, and fail with its message if the
/// reconcile failed.
pub(crate) async fn wait(micad: &MicadProxy<'_>, task: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + TASK_TIMEOUT;
    loop {
        let record: Value = serde_json::from_str(&micad.get_task(task).await.map_err(bus_error)?)?;
        if record["status"] == json!("finished") {
            if record["outcome"] == json!("succeeded") {
                return Ok(());
            }
            bail!(
                "stored, but the reconcile failed: {}",
                record["message"].as_str().unwrap_or("no reason given")
            );
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("stored; micad is still applying it (task {task})");
        }
        tokio::time::sleep(POLL).await;
    }
}

/// micad's refusal, in its own words rather than the bus error's name.
pub(crate) fn bus_error(error: zbus::Error) -> anyhow::Error {
    match crate::bus::error_reply(&error) {
        Some((name, message)) => anyhow::anyhow!(message.unwrap_or(name)),
        None => anyhow::Error::new(error).context("micad"),
    }
}

#[cfg(test)]
mod tests;
