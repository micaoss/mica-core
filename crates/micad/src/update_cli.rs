//! `mica-update` (or `micad update`), the operator's update command: micad
//! over the system bus.
//!
//! It asks micad what `mica-deploy` would otherwise be asked by hand, so the
//! source, channel and workspace budget are the device's own and the gates
//! are the ones the console meets. Nothing installs without `install`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

const USAGE: &str = "\
usage: mica-update [status]
       mica-update check
       mica-update install [--reboot]
       mica-update rollback [--reboot]
       (micad update ... is the same command)

  status    show the running versions and the update state
  check     look for a newer release
  install   check, download and install the newest release
  rollback  return to the retained previous system

  --reboot  reboot once the change needs it, instead of saying so";

/// How often the state is read while micad works.
const POLL: Duration = Duration::from_secs(1);
/// Polls in a row with nothing running before a request counts as stalled.
const STALLED_POLLS: u32 = 10;

#[zbus::proxy(
    interface = "com.mica.micad1",
    default_service = "com.mica.micad",
    default_path = "/com/mica/micad"
)]
trait Micad {
    fn get_update_state(&self) -> zbus::Result<String>;
    fn check_update(&self) -> zbus::Result<()>;
    fn install_latest_update(&self) -> zbus::Result<()>;
    fn rollback_deployment(&self, deployment_id: &str) -> zbus::Result<()>;
    fn reboot(&self) -> zbus::Result<()>;
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Status,
    Check,
    Install { reboot: bool },
    Rollback { reboot: bool },
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let mut words = args.iter().map(String::as_str);
    let verb = words.next();
    let rest: Vec<&str> = words.collect();
    let reboot = match rest.as_slice() {
        [] => false,
        ["--reboot"] if matches!(verb, Some("install" | "rollback")) => true,
        _ => return Err(format!("unexpected arguments: {}", rest.join(" "))),
    };
    match verb {
        None | Some("status") => Ok(Command::Status),
        Some("check") => Ok(Command::Check),
        Some("install") => Ok(Command::Install { reboot }),
        Some("rollback") => Ok(Command::Rollback { reboot }),
        Some("help" | "-h" | "--help") => Ok(Command::Help),
        Some(other) => Err(format!("unknown command: {other}")),
    }
}

/// `mica-update`'s entry point: exit 0 done, 1 failed, 2 misused.
///
/// `args` are the words after the command: after `mica-update`, or after
/// `micad update`.
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
            eprintln!("mica-update: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if let Err(error) = runtime.block_on(run(command)) {
        eprintln!("mica-update: {error:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run(command: Command) -> Result<()> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to the system bus")?;
    let micad = MicadProxy::new(&connection).await?;
    let me = connection
        .unique_name()
        .map(|name| name.to_string())
        .unwrap_or_default();
    match command {
        Command::Status => {
            print!("{}", render_status(&state(&micad).await?));
            Ok(())
        }
        Command::Check => {
            let before = state(&micad).await?;
            micad.check_update().await.map_err(bus_error)?;
            match follow(&micad, &me, &before, Watch::Check).await? {
                Done::Available(what) => println!("available: {what}"),
                _ => println!("up to date"),
            }
            Ok(())
        }
        Command::Install { reboot } => {
            let before = state(&micad).await?;
            micad.install_latest_update().await.map_err(bus_error)?;
            match follow(&micad, &me, &before, Watch::Install).await? {
                Done::Installed {
                    reboot_pending: true,
                } => finish(&micad, reboot, "installed").await,
                Done::Installed { .. } => {
                    println!("installed");
                    Ok(())
                }
                _ => {
                    println!("up to date");
                    Ok(())
                }
            }
        }
        Command::Rollback { reboot } => {
            let doc = state(&micad).await?;
            let (running, target) = rollback_target(&doc)?;
            micad
                .rollback_deployment(&running)
                .await
                .map_err(bus_error)?;
            finish(
                &micad,
                reboot,
                &format!("rolled back to {}", short(&target)),
            )
            .await
        }
        Command::Help => Ok(()),
    }
}

/// Say what a change that needs a reboot did, and reboot when asked to.
async fn finish(micad: &MicadProxy<'_>, reboot: bool, what: &str) -> Result<()> {
    if !reboot {
        println!("{what}; reboot to run it (or run again with --reboot)");
        return Ok(());
    }
    println!("{what}; rebooting");
    micad.reboot().await.map_err(bus_error)
}

async fn state(micad: &MicadProxy<'_>) -> Result<Value> {
    let raw = micad.get_update_state().await.map_err(bus_error)?;
    Ok(serde_json::from_str(&raw)?)
}

/// micad's refusal, in its own words rather than the bus error's name.
fn bus_error(error: zbus::Error) -> anyhow::Error {
    match crate::bus::error_reply(&error) {
        Some((name, message)) => anyhow::anyhow!(message.unwrap_or(name)),
        None => anyhow::Error::new(error).context("micad"),
    }
}

/// Read the state until the request ends, printing each step once.
async fn follow(micad: &MicadProxy<'_>, me: &str, before: &Value, watch: Watch) -> Result<Done> {
    let refusal = text(before, "/lifecycle/last_refusal");
    let mut said = String::new();
    let mut stalled = 0;
    loop {
        let doc = state(micad).await?;
        match progress(&doc, me, refusal.as_deref(), watch) {
            Progress::Working(step) => {
                stalled = 0;
                if step != said {
                    println!("{step}...");
                    said = step;
                }
            }
            Progress::Between => {
                stalled += 1;
                if stalled >= STALLED_POLLS {
                    let at = text(&doc, "/lifecycle/state").unwrap_or_default();
                    bail!("stopped at {at}; see `mica-update status`");
                }
            }
            Progress::Finished(outcome) => return outcome.map_err(anyhow::Error::msg),
        }
        tokio::time::sleep(POLL).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watch {
    Check,
    Install,
}

#[derive(Debug, PartialEq, Eq)]
enum Done {
    UpToDate,
    Available(String),
    Installed { reboot_pending: bool },
}

#[derive(Debug, PartialEq, Eq)]
enum Progress {
    /// A step is running.
    Working(String),
    /// Nothing runs, and the request has not ended: one step settled and the
    /// next has not started yet.
    Between,
    Finished(Result<Done, String>),
}

/// Where a request stands in one read of the state document.
///
/// `me` is this connection's bus name, which micad records on the install it
/// starts for it; `refusal` the refusal recorded before the request, so only
/// a new one ends it.
fn progress(doc: &Value, me: &str, refusal: Option<&str>, watch: Watch) -> Progress {
    if watch == Watch::Install && text(doc, "/install/requested_by").as_deref() == Some(me) {
        return match text(doc, "/install/status").as_deref() {
            Some("done") => Progress::Finished(Ok(Done::Installed {
                reboot_pending: reboot_pending(doc),
            })),
            Some("failed") => Progress::Finished(Err(
                text(doc, "/install/error").unwrap_or_else(|| "the install failed".to_string())
            )),
            _ => Progress::Working("installing".to_string()),
        };
    }
    let state = text(doc, "/lifecycle/state").unwrap_or_default();
    let reason = || text(doc, "/lifecycle/reason").unwrap_or_else(|| state.clone());
    match state.as_str() {
        "failed" | "update-unavailable" => return Progress::Finished(Err(reason())),
        "checking" | "downloading" | "installing" | "discarding" => {
            return Progress::Working(state);
        }
        _ => {}
    }
    if let Some(new) =
        text(doc, "/lifecycle/last_refusal").filter(|new| Some(new.as_str()) != refusal)
    {
        return Progress::Finished(Err(new));
    }
    // A check that finds nothing clears `available`; a fetch keeps it until
    // the install, so between the steps it is still there.
    let available = doc
        .pointer("/lifecycle/available")
        .filter(|value| !value.is_null());
    match (watch, available) {
        (_, None) => Progress::Finished(Ok(Done::UpToDate)),
        (Watch::Check, Some(available)) => {
            Progress::Finished(Ok(Done::Available(describe(available))))
        }
        (Watch::Install, Some(_)) => Progress::Between,
    }
}

/// Whether what is installed runs only after a reboot.
fn reboot_pending(doc: &Value) -> bool {
    text(doc, "/lifecycle/state").as_deref() == Some("reboot-required")
        || doc
            .pointer("/state/candidate")
            .is_some_and(Value::is_string)
        || doc
            .pointer("/sets/core/pending")
            .is_some_and(|pending| !pending.is_null())
}

/// The running deployment and the retained one a rollback selects, or the
/// reason the device refuses it.
fn rollback_target(doc: &Value) -> Result<(String, String)> {
    let permitted = doc.pointer("/rollback/permitted").and_then(Value::as_bool) == Some(true);
    match (
        text(doc, "/boot/deploymentId"),
        text(doc, "/rollback/target"),
    ) {
        (Some(running), Some(target)) if permitted => Ok((running, target)),
        _ => bail!(
            "rollback is not possible: {}",
            text(doc, "/rollback/reason").unwrap_or_else(|| "no retained system".to_string())
        ),
    }
}

/// `status`: what runs, what is on offer, where the lifecycle stands.
fn render_status(doc: &Value) -> String {
    let mut lines = Vec::new();
    let running = text(doc, "/boot/deploymentId");
    let version = running.as_deref().and_then(|id| {
        doc.pointer("/deployments")?
            .as_array()?
            .iter()
            .find(|deployment| deployment["id"].as_str() == Some(id))
            .and_then(|deployment| deployment["version"].as_str())
            .map(str::to_string)
    });
    match (version, &running) {
        (Some(version), Some(id)) => lines.push(format!("system     {version} ({})", short(id))),
        (None, Some(id)) => lines.push(format!("system     {}", short(id))),
        _ => lines.push("system     unknown".to_string()),
    }
    if let Some(core) = doc
        .pointer("/sets/core/current")
        .filter(|core| !core.is_null())
    {
        lines.push(format!(
            "core       {} (channel {})",
            core["version"].as_str().unwrap_or("?"),
            core["channel"].as_str().unwrap_or("?")
        ));
    }
    let state = text(doc, "/lifecycle/state").unwrap_or_else(|| "unknown".to_string());
    match text(doc, "/lifecycle/reason") {
        Some(reason) => lines.push(format!("state      {state}: {reason}")),
        None => lines.push(format!("state      {state}")),
    }
    if let Some(available) = doc
        .pointer("/lifecycle/available")
        .filter(|value| !value.is_null())
    {
        lines.push(format!("available  {}", describe(available)));
    }
    if reboot_pending(doc) {
        lines.push("reboot     needed to run what is installed".to_string());
    }
    if let Some(checked) = text(doc, "/lifecycle/last_check") {
        lines.push(format!("checked    {checked}"));
    }
    if let Some(refusal) = text(doc, "/lifecycle/last_refusal") {
        lines.push(format!("refused    {refusal}"));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// An offered release: `core 0.0.6` or `system 1.4.0`.
fn describe(available: &Value) -> String {
    let kind = match available["kind"].as_str() {
        Some("set") => available["setKind"].as_str().unwrap_or("set"),
        _ => "system",
    };
    format!("{kind} {}", available["version"].as_str().unwrap_or("?"))
}

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

fn text(doc: &Value, pointer: &str) -> Option<String> {
    doc.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests;
