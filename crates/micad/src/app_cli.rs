//! `micad app`, the operator's command for their apps: micad over the system
//! bus. The bus policy makes it root's.
//!
//! A write waits for its task unless `--detach`: an app upgrading itself
//! stops its own units, so it detaches once micad has the bundle.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::unit_cli::bus_error;

const USAGE: &str = "\
usage: micad app install <file> [--reinstall] [--detach]
       micad app upgrade <file> [--detach]
       micad app update <name> [--detach]
       micad app check [name]
       micad app rollback <name> [--detach]
       micad app remove <name> [--purge] [--detach]
       micad app list
       micad app status <name>
       micad app logs <name> [--tail N]

  install   install or upgrade the app in a bundle; --reinstall installs the
            installed version again
  upgrade   the same, refused when the app is not installed
  update    fetch the newer version its update URL offers, and upgrade
  check     read its update URL now (every app with one, without a name)
  rollback  switch to the previous version
  remove    stop it and delete it; --purge deletes its data too
  list      every app
  status    one app
  logs      its units' newest lines and its first-run output (200 unless
            --tail)

  --detach returns once micad has the request, without waiting for it.";

/// How often a task is read while micad runs it.
const POLL: Duration = Duration::from_millis(500);
/// How long an app flow may take before the command stops waiting: an
/// unpack and a first-run hook of 300 s.
const TASK_TIMEOUT: Duration = Duration::from_secs(600);
/// The lines `logs` shows unless asked.
const DEFAULT_TAIL: usize = 200;

#[zbus::proxy(
    interface = "com.mica.micad1",
    default_service = "com.mica.micad",
    default_path = "/com/mica/micad"
)]
trait MicadApps {
    fn get_task(&self, id: &str) -> zbus::Result<String>;
    fn get_apps(&self) -> zbus::Result<String>;
    fn get_app(&self, name: &str) -> zbus::Result<String>;
    fn install_app(&self, path: &str, reinstall: bool, upgrade: bool) -> zbus::Result<String>;
    fn rollback_app(&self, name: &str) -> zbus::Result<String>;
    fn remove_app(&self, name: &str, purge: bool) -> zbus::Result<String>;
    fn check_app_update(&self, name: &str) -> zbus::Result<String>;
    fn update_app(&self, name: &str) -> zbus::Result<String>;
    fn get_app_log(&self, name: &str) -> zbus::Result<String>;
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Install {
        file: String,
        reinstall: bool,
        upgrade: bool,
    },
    Update(String),
    Check(Option<String>),
    Rollback(String),
    Remove {
        name: String,
        purge: bool,
    },
    List,
    Status(String),
    Logs {
        name: String,
        tail: usize,
    },
    Help,
}

/// The words, `--detach` taken out, and whether it was there.
fn detach(words: &[&str]) -> (Vec<String>, bool) {
    let kept = words
        .iter()
        .filter(|word| **word != "--detach")
        .map(|word| (*word).to_string())
        .collect::<Vec<_>>();
    let detached = kept.len() != words.len();
    (kept, detached)
}

fn parse(args: &[String]) -> Result<(Command, bool), String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (words, detached) = detach(&words);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let command = match words.as_slice() {
        ["install", file] => Command::Install {
            file: (*file).to_string(),
            reinstall: false,
            upgrade: false,
        },
        ["install", file, "--reinstall"] | ["install", "--reinstall", file] => Command::Install {
            file: (*file).to_string(),
            reinstall: true,
            upgrade: false,
        },
        ["upgrade", file] => Command::Install {
            file: (*file).to_string(),
            reinstall: false,
            upgrade: true,
        },
        ["update", name] => Command::Update((*name).to_string()),
        ["check"] => Command::Check(None),
        ["check", name] => Command::Check(Some((*name).to_string())),
        ["rollback", name] => Command::Rollback((*name).to_string()),
        ["remove", name] => Command::Remove {
            name: (*name).to_string(),
            purge: false,
        },
        ["remove", name, "--purge"] | ["remove", "--purge", name] => Command::Remove {
            name: (*name).to_string(),
            purge: true,
        },
        ["list"] => Command::List,
        ["status", name] => Command::Status((*name).to_string()),
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
            Command::Logs {
                name: (*name).to_string(),
                tail,
            }
        }
        [] | ["help" | "-h" | "--help"] => Command::Help,
        [other, ..] => return Err(format!("unknown command or arguments: {other}")),
    };
    Ok((command, detached))
}

/// `micad app`'s entry point: exit 0 done, 1 failed, 2 misused.
pub fn main(args: &[String]) -> Result<()> {
    if mica_apid::wants_version(args.iter().cloned()) {
        println!("{}", mica_apid::version_line("micad"));
        return Ok(());
    }
    let (command, detached) = match parse(args) {
        Ok((Command::Help, _)) => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("micad app: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if let Err(error) = runtime.block_on(run(command, detached)) {
        eprintln!("micad app: {error:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run(command: Command, detached: bool) -> Result<()> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to the system bus")?;
    let micad = MicadAppsProxy::new(&connection).await?;
    let task = match command {
        Command::Install {
            file,
            reinstall,
            upgrade,
        } => {
            // micad reads the file, from its own working directory.
            let path = std::fs::canonicalize(&file).with_context(|| format!("read {file}"))?;
            micad
                .install_app(&path.to_string_lossy(), reinstall, upgrade)
                .await
        }
        Command::Update(name) => micad.update_app(&name).await,
        Command::Rollback(name) => micad.rollback_app(&name).await,
        Command::Remove { name, purge } => micad.remove_app(&name, purge).await,
        Command::Check(Some(name)) => micad.check_app_update(&name).await,
        Command::Check(None) => {
            let apps: Value = serde_json::from_str(&micad.get_apps().await.map_err(bus_error)?)?;
            for app in apps["apps"].as_array().into_iter().flatten() {
                let Some(name) = app["name"].as_str().filter(|_| app["update"].is_object()) else {
                    continue;
                };
                let task = micad.check_app_update(name).await.map_err(bus_error)?;
                wait(&micad, &task).await?;
            }
            print!(
                "{}",
                render_list(&serde_json::from_str(
                    &micad.get_apps().await.map_err(bus_error)?
                )?)
            );
            return Ok(());
        }
        Command::List => {
            let apps: Value = serde_json::from_str(&micad.get_apps().await.map_err(bus_error)?)?;
            print!("{}", render_list(&apps));
            return Ok(());
        }
        Command::Status(name) => {
            let app: Value = serde_json::from_str(&micad.get_app(&name).await.map_err(bus_error)?)?;
            print!("{}", render_status(&app));
            return Ok(());
        }
        Command::Logs { name, tail } => {
            let log: Value =
                serde_json::from_str(&micad.get_app_log(&name).await.map_err(bus_error)?)?;
            print!("{}", render_log(&log, tail));
            return Ok(());
        }
        Command::Help => return Ok(()),
    }
    .map_err(bus_error)?;
    if detached {
        println!("task {task}");
        return Ok(());
    }
    let message = wait(&micad, &task).await?;
    println!("{message}");
    Ok(())
}

/// Wait until micad has run `task`; what it did, or its failure.
async fn wait(micad: &MicadAppsProxy<'_>, task: &str) -> Result<String> {
    let deadline = tokio::time::Instant::now() + TASK_TIMEOUT;
    loop {
        let record: Value = serde_json::from_str(&micad.get_task(task).await.map_err(bus_error)?)?;
        if record["status"] == json!("finished") {
            if record["outcome"] == json!("succeeded") {
                return Ok(format!(
                    "{} done",
                    record["operation"].as_str().unwrap_or("task")
                ));
            }
            bail!(
                "{}",
                record["message"]
                    .as_str()
                    .unwrap_or("failed, no reason given")
            );
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("micad is still running it (task {task})");
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every app: its version, the one kept, where it came from and its update.
fn render_list(apps: &Value) -> String {
    let mut out = String::new();
    if apps["available"] != json!(true) {
        out.push_str("this daemon keeps no apps\n");
    }
    let mut rows = vec![[
        "NAME".to_string(),
        "VERSION".to_string(),
        "PREVIOUS".to_string(),
        "SOURCE".to_string(),
        "UPDATE".to_string(),
    ]];
    let text = |value: &Value| value.as_str().unwrap_or("-").to_string();
    for app in apps["apps"].as_array().into_iter().flatten() {
        let update = match &app["update"] {
            Value::Null => "-".to_string(),
            update => match update["available"].as_str() {
                Some(version) => format!("{version} available"),
                None if update["error"].is_string() => "check failed".to_string(),
                None => "current".to_string(),
            },
        };
        rows.push([
            text(&app["name"]),
            text(&app["version"]),
            text(&app["previous"]),
            text(&app["source"]),
            update,
        ]);
    }
    let widths: Vec<usize> = (0..5)
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

/// One app, a line a fact.
fn render_status(app: &Value) -> String {
    let text = |value: &Value| value.as_str().unwrap_or("-").to_string();
    let mut out = format!(
        "name: {}\nversion: {}\nprevious: {}\nsource: {}\ninstalled: {}\nsha256: {}\n",
        text(&app["name"]),
        text(&app["version"]),
        text(&app["previous"]),
        text(&app["source"]),
        text(&app["installedAt"]),
        text(&app["sha256"]),
    );
    for unit in app["units"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "unit: {} ({}, {})\n",
            text(&unit["name"]),
            if unit["enabled"] == json!(true) {
                "enabled"
            } else {
                "disabled"
            },
            text(&unit["active"]),
        ));
    }
    let last = &app["last"];
    if last.is_object() {
        out.push_str(&format!(
            "last: {} {}{}\n",
            text(&last["action"]),
            text(&last["outcome"]),
            last["message"]
                .as_str()
                .map(|message| format!(": {message}"))
                .unwrap_or_default(),
        ));
    }
    let update = &app["update"];
    if update.is_object() {
        out.push_str(&format!(
            "update: {}{}\n",
            text(&update["url"]),
            update["available"]
                .as_str()
                .map(|version| format!(" ({version} available)"))
                .unwrap_or_default(),
        ));
    }
    out
}

/// The newest `tail` lines of each unit, then the first-run output.
fn render_log(log: &Value, tail: usize) -> String {
    let mut out = String::new();
    let lines = |value: &Value| -> Vec<String> {
        let lines: Vec<&str> = value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        lines[lines.len().saturating_sub(tail)..]
            .iter()
            .map(|line| format!("{line}\n"))
            .collect()
    };
    for unit in log["units"].as_array().into_iter().flatten() {
        out.push_str(&format!("== {}\n", unit["unit"].as_str().unwrap_or("-")));
        if unit["available"] == json!(true) {
            out.extend(lines(&unit["lines"]));
        } else {
            out.push_str(&format!(
                "{}\n",
                unit["detail"].as_str().unwrap_or("the log is unavailable")
            ));
        }
    }
    if !log["firstRun"].is_null() {
        out.push_str("== first run\n");
        out.extend(lines(&log["firstRun"]));
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
        assert_eq!(
            parse(&words("install a.tar.gz --reinstall --detach")),
            Ok((
                Command::Install {
                    file: "a.tar.gz".into(),
                    reinstall: true,
                    upgrade: false
                },
                true
            ))
        );
        assert_eq!(
            parse(&words("upgrade a.tar.gz")),
            Ok((
                Command::Install {
                    file: "a.tar.gz".into(),
                    reinstall: false,
                    upgrade: true
                },
                false
            ))
        );
        assert_eq!(
            parse(&words("remove demo --purge")),
            Ok((
                Command::Remove {
                    name: "demo".into(),
                    purge: true
                },
                false
            ))
        );
        assert_eq!(parse(&words("check")), Ok((Command::Check(None), false)));
        assert_eq!(
            parse(&words("logs demo --tail 5")),
            Ok((
                Command::Logs {
                    name: "demo".into(),
                    tail: 5
                },
                false
            ))
        );
        assert!(parse(&words("logs demo --tail 0")).is_err());
        assert!(parse(&words("install")).is_err());
        assert!(parse(&words("remove demo --keep")).is_err());
    }

    #[test]
    fn the_list_and_the_status_say_what_runs() {
        let app = json!({
            "name": "demo", "version": "1.1", "previous": "1.0", "source": "upload",
            "installedAt": "2026-10-21T08:00:00Z", "sha256": "ab",
            "units": [{ "name": "demo.service", "enabled": true, "active": "active" }],
            "last": { "action": "upgrade", "outcome": "ok" },
            "update": { "url": "https://apps.example/demo.json", "available": "1.2" },
        });
        assert_eq!(
            render_list(&json!({ "available": true, "apps": [app] })),
            "NAME  VERSION  PREVIOUS  SOURCE  UPDATE\n\
             demo  1.1      1.0       upload  1.2 available\n"
        );
        assert_eq!(
            render_status(&app),
            "name: demo\nversion: 1.1\nprevious: 1.0\nsource: upload\n\
             installed: 2026-10-21T08:00:00Z\nsha256: ab\n\
             unit: demo.service (enabled, active)\nlast: upgrade ok\n\
             update: https://apps.example/demo.json (1.2 available)\n"
        );
    }

    #[test]
    fn the_log_is_each_unit_then_the_first_run() {
        let log = json!({
            "units": [
                { "unit": "a.service", "available": true, "lines": ["1", "2", "3"] },
                { "unit": "b.service", "available": false, "detail": "no journal" },
            ],
            "firstRun": ["ran"],
        });
        assert_eq!(
            render_log(&log, 2),
            "== a.service\n2\n3\n== b.service\nno journal\n== first run\nran\n"
        );
    }
}
