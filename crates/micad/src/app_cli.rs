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
usage: micad app install <file> [--reinstall] [--yes] [--detach]
       micad app upgrade <file> [--yes] [--detach]
       micad app update <name> [--yes] [--detach]
       micad app check [name]
       micad app rollback <name> [--detach]
       micad app remove <name> [--purge] [--detach]
       micad app list
       micad app status <name>
       micad app logs <name> [--tail N]

  install   install or upgrade the app in a bundle; --reinstall installs the
            installed version again
  upgrade   the same, refused when the app is not installed. A bundle that
            asks for privileges beyond the installed version's (root,
            capabilities, devices; any on a first install) prints them and
            asks first, unless --yes
  update    fetch the newer version its update URL offers, and upgrade; one
            that asks for more privileges prints them and asks first,
            unless --yes; either accepts that version and bundle only
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
    fn install_app(
        &self,
        path: &str,
        reinstall: bool,
        upgrade: bool,
        accept_privileges: bool,
    ) -> zbus::Result<String>;
    fn rollback_app(&self, name: &str) -> zbus::Result<String>;
    fn remove_app(&self, name: &str, purge: bool) -> zbus::Result<String>;
    fn check_app_update(&self, name: &str) -> zbus::Result<String>;
    fn update_app(&self, name: &str, accept: &str) -> zbus::Result<String>;
    fn get_app_log(&self, name: &str) -> zbus::Result<String>;
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Install {
        file: String,
        reinstall: bool,
        upgrade: bool,
        /// Accept the privileges the bundle asks for without asking.
        yes: bool,
    },
    Update {
        name: String,
        /// Accept the privileges the newer version asks for without asking.
        yes: bool,
    },
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

/// The words, `flag` taken out, and whether it was there.
fn take(words: &[String], flag: &str) -> (Vec<String>, bool) {
    let kept = words
        .iter()
        .filter(|word| *word != flag)
        .cloned()
        .collect::<Vec<_>>();
    let found = kept.len() != words.len();
    (kept, found)
}

fn parse(args: &[String]) -> Result<(Command, bool), String> {
    let (words, detached) = take(args, "--detach");
    let (words, yes) = take(&words, "--yes");
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    if yes && !matches!(words.first(), Some(&("install" | "upgrade" | "update"))) {
        return Err("--yes is for install, upgrade and update".into());
    }
    let command = match words.as_slice() {
        ["install", file] => Command::Install {
            file: (*file).to_string(),
            reinstall: false,
            upgrade: false,
            yes,
        },
        ["install", file, "--reinstall"] | ["install", "--reinstall", file] => Command::Install {
            file: (*file).to_string(),
            reinstall: true,
            upgrade: false,
            yes,
        },
        ["upgrade", file] => Command::Install {
            file: (*file).to_string(),
            reinstall: false,
            upgrade: true,
            yes,
        },
        ["update", name] => Command::Update {
            name: (*name).to_string(),
            yes,
        },
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

/// micad's answer when the bundle asks for privileges not yet accepted:
/// `{"message", "privileges": {"root", "capabilities", "devices"}}`.
fn privileges_asked(err: &zbus::Error) -> Option<Value> {
    match crate::bus::error_reply(err) {
        Some((name, Some(message))) if name == crate::bus::APP_PRIVILEGES_ERROR => {
            serde_json::from_str(&message).ok()
        }
        _ => None,
    }
}

/// What `asked` asks for, a line each.
fn render_privileges(asked: &Value) -> String {
    let mut out = format!("{}\n", asked["message"].as_str().unwrap_or_default());
    let privileges = &asked["privileges"];
    if privileges["root"] == json!(true) {
        out.push_str("  runs as root, with full control of the device\n");
    }
    for (key, label) in [("capabilities", "capabilities"), ("devices", "devices")] {
        let items: Vec<&str> = privileges[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !items.is_empty() {
            out.push_str(&format!("  {label}: {}\n", items.join(", ")));
        }
    }
    out
}

/// Whether the operator, at a terminal, accepts; never without one.
fn confirm() -> Result<bool> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    eprint!("install it? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
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
            yes,
        } => {
            // micad reads the file, from its own working directory.
            let path = std::fs::canonicalize(&file).with_context(|| format!("read {file}"))?;
            let path = path.to_string_lossy();
            match micad.install_app(&path, reinstall, upgrade, yes).await {
                Err(err) => match privileges_asked(&err) {
                    Some(asked) => {
                        eprint!("{}", render_privileges(&asked));
                        if !confirm()? {
                            bail!("not installed; --yes accepts the privileges");
                        }
                        micad.install_app(&path, reinstall, upgrade, true).await
                    }
                    None => Err(err),
                },
                answer => answer,
            }
        }
        Command::Update { name, yes } => {
            let task = micad.update_app(&name, "").await.map_err(bus_error)?;
            if detached {
                println!("task {task}");
                return Ok(());
            }
            let err = match wait(&micad, &task).await {
                Ok(message) => {
                    println!("{message}");
                    return Ok(());
                }
                Err(err) => err,
            };
            let app: Value = serde_json::from_str(&micad.get_app(&name).await.map_err(bus_error)?)?;
            let privileges = &app["update"]["privileges"];
            let Some(version) = app["update"]["available"]
                .as_str()
                .filter(|_| privileges.is_object())
            else {
                return Err(err);
            };
            if !yes {
                eprint!(
                    "{}",
                    render_privileges(
                        &json!({ "message": format!("{err:#}"), "privileges": privileges })
                    )
                );
                if !confirm()? {
                    bail!("not updated; --yes accepts the privileges");
                }
            }
            micad.update_app(&name, version).await
        }
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
        "RUNS AS".to_string(),
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
            runs_as(&app["privileges"]),
            update,
        ]);
    }
    let widths: Vec<usize> = (0..6)
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
/// Who an app runs as: root, or its own user.
fn runs_as(privileges: &Value) -> String {
    match privileges["user"].as_str() {
        Some("root") => "root".to_string(),
        Some(_) => privileges["account"].as_str().unwrap_or("-").to_string(),
        None => "-".to_string(),
    }
}

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
    let privileges = &app["privileges"];
    match runs_as(privileges).as_str() {
        "root" => out.push_str("runs as: root, with full control of the device\n"),
        "-" => {}
        account => out.push_str(&format!("runs as: {account} (uid {})\n", privileges["uid"])),
    }
    for key in ["capabilities", "devices"] {
        let list: Vec<&str> = privileges[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !list.is_empty() {
            out.push_str(&format!("{key}: {}\n", list.join(", ")));
        }
    }
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
                    upgrade: false,
                    yes: false
                },
                true
            ))
        );
        assert_eq!(
            parse(&words("upgrade a.tar.gz --yes")),
            Ok((
                Command::Install {
                    file: "a.tar.gz".into(),
                    reinstall: false,
                    upgrade: true,
                    yes: true
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
        assert!(parse(&words("remove demo --yes")).is_err());
        assert_eq!(
            parse(&words("update demo --yes")),
            Ok((
                Command::Update {
                    name: "demo".into(),
                    yes: true
                },
                false
            ))
        );
    }

    #[test]
    fn the_privileges_asked_are_a_line_each() {
        let asked = json!({
            "message": "demo 2.0 asks for privileges beyond 1.0; accept them to install it",
            "privileges": { "root": false, "capabilities": ["CAP_NET_RAW"], "devices": ["/dev/ttyUSB0"] },
        });
        assert_eq!(
            render_privileges(&asked),
            "demo 2.0 asks for privileges beyond 1.0; accept them to install it\n  capabilities: CAP_NET_RAW\n  devices: /dev/ttyUSB0\n"
        );
        let root = json!({ "message": "x", "privileges": { "root": true, "capabilities": [], "devices": [] } });
        assert_eq!(
            render_privileges(&root),
            "x\n  runs as root, with full control of the device\n"
        );
    }

    #[test]
    fn the_list_and_the_status_say_what_runs() {
        let app = json!({
            "name": "demo", "version": "1.1", "previous": "1.0", "source": "upload",
            "installedAt": "2026-10-21T08:00:00Z", "sha256": "ab",
            "units": [{ "name": "demo.service", "enabled": true, "active": "active" }],
            "privileges": {
                "user": "app", "account": "mica-app-demo", "uid": 30000,
                "capabilities": ["CAP_NET_ADMIN"], "devices": ["/dev/net/tun"],
            },
            "last": { "action": "upgrade", "outcome": "ok" },
            "update": { "url": "https://apps.example/demo.json", "available": "1.2" },
        });
        let root = json!({
            "name": "tool", "version": "1", "source": "cli",
            "privileges": { "user": "root", "capabilities": [], "devices": [] },
        });
        assert_eq!(
            render_list(&json!({ "available": true, "apps": [app, root] })),
            "NAME  VERSION  PREVIOUS  SOURCE  RUNS AS        UPDATE\n\
             demo  1.1      1.0       upload  mica-app-demo  1.2 available\n\
             tool  1        -         cli     root           -\n"
        );
        assert_eq!(
            render_status(&app),
            "name: demo\nversion: 1.1\nprevious: 1.0\nsource: upload\n\
             installed: 2026-10-21T08:00:00Z\nsha256: ab\n\
             runs as: mica-app-demo (uid 30000)\ncapabilities: CAP_NET_ADMIN\n\
             devices: /dev/net/tun\n\
             unit: demo.service (enabled, active)\nlast: upgrade ok\n\
             update: https://apps.example/demo.json (1.2 available)\n"
        );
        assert!(render_status(&root).contains("runs as: root, with full control of the device\n"));
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
