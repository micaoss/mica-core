//! `micad mdns`, what the device announces on the LAN: the switch over the
//! system bus, and runtime drop-ins in `/run/mica/mdns.d`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::reconciler::mdns::entries::{self, Entry, RUNTIME_DIR};
use crate::unit_cli::{MicadProxy, bus_error, wait};

const USAGE: &str = "\
usage: micad mdns status|list
       micad mdns on|off
       micad mdns add NAME --type _x._tcp --port N [--instance S] [--txt KEY=VALUE]...
       micad mdns remove NAME

  status  whether the responder runs, and under which name
  list    every service, its source, and whether it is published
  on      announce the device (access.mdns.enabled)
  off     no responder, no socket; drop-ins are kept
  add     publish one service until reboot: /run/mica/mdns.d/NAME.json
  remove  remove that file";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Status,
    List,
    Switch(bool),
    Add(String, Entry),
    Remove(String),
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["status"] => Ok(Command::Status),
        ["list"] => Ok(Command::List),
        ["on"] => Ok(Command::Switch(true)),
        ["off"] => Ok(Command::Switch(false)),
        ["add", name, rest @ ..] => add(name, rest),
        ["remove", name] => {
            if entries::is_name(name) {
                Ok(Command::Remove((*name).to_string()))
            } else {
                Err(format!("{name:?} is not a drop-in name"))
            }
        }
        [] | ["help" | "-h" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown command or arguments: {other}")),
    }
}

fn add(name: &str, words: &[&str]) -> Result<Command, String> {
    if !entries::is_name(name) {
        return Err(format!(
            "{name:?} is not 1 to 32 of a-z, 0-9 and -, a letter or digit first"
        ));
    }
    let (mut kind, mut port, mut instance) = (None, None, None);
    let mut txt = BTreeMap::new();
    let mut rest = words.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match *flag {
            "--type" => kind = Some((*value).to_string()),
            "--port" => {
                port = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| format!("--port {value:?} is not a port"))?,
                );
            }
            "--instance" => instance = Some((*value).to_string()),
            "--txt" => {
                let (key, value) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--txt {value:?} is not KEY=VALUE"))?;
                txt.insert(key.to_string(), value.to_string());
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let entry = Entry {
        instance,
        kind: kind.ok_or("--type is needed")?,
        port: port.ok_or("--port is needed")?,
        txt,
        service: None,
    };
    entries::check(&entry)?;
    Ok(Command::Add(name.to_string(), entry))
}

/// `micad mdns`'s entry point: exit 0 done, 1 failed, 2 misused.
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
            eprintln!("micad mdns: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let result = match command {
        Command::Add(name, entry) => add_file(Path::new(RUNTIME_DIR), &name, &entry),
        Command::Remove(name) => remove_file(Path::new(RUNTIME_DIR), &name),
        command => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| runtime.block_on(run(command))),
    };
    if let Err(error) = result {
        eprintln!("micad mdns: {error:#}");
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
        Command::Status | Command::List => {
            let state: Value =
                serde_json::from_str(&micad.get_state("mdns").await.map_err(bus_error)?)?;
            if command == Command::Status {
                print!("{}", render_status(&state));
            } else {
                print!("{}", render_list(&state));
            }
        }
        Command::Switch(on) => {
            let task = micad
                .set_settings("access.mdns.enabled", &json!(on).to_string())
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("access.mdns.enabled is {on}");
        }
        Command::Add(..) | Command::Remove(_) | Command::Help => {}
    }
    Ok(())
}

/// Write `entry` as the drop-in `name` in `dir`, whole or not at all.
fn add_file(dir: &Path, name: &str, entry: &Entry) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let text = json!({ "version": 1, "services": [entry] }).to_string() + "\n";
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("{name}.json"));
    // Not a drop-in name, so never read half-written.
    let partial = dir.join(format!(".{name}.json.new"));
    std::fs::write(&partial, text).with_context(|| format!("write {}", partial.display()))?;
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&partial, &path).with_context(|| format!("write {}", path.display()))?;
    println!("{} is published until reboot", path.display());
    Ok(())
}

/// Remove the drop-in `name` from `dir`.
fn remove_file(dir: &Path, name: &str) -> Result<()> {
    let path = dir.join(format!("{name}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => {
            println!("{} is removed", path.display());
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("there is no {}", path.display())
        }
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

/// The switch, its source and the name; the count of what is published.
fn render_status(state: &Value) -> String {
    let services = state["services"].as_array().map_or(&[][..], Vec::as_slice);
    let published = services
        .iter()
        .filter(|s| s["published"] == json!(true))
        .count();
    let mut out = format!(
        "enabled: {} ({})\nhostname: {}\npublished: {published} of {}\n",
        state["enabled"] == json!(true),
        state["source"].as_str().unwrap_or("product"),
        state["hostname"].as_str().unwrap_or(""),
        services.len(),
    );
    if let Some(error) = state["error"].as_str().or(state["refused"].as_str()) {
        out.push_str(&format!("error: {error}\n"));
    }
    out
}

/// One line per service, then one per refused source.
fn render_list(state: &Value) -> String {
    let mut out = String::new();
    for service in state["services"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "{}.{} port {} {} {}\n",
            service["instance"].as_str().unwrap_or(""),
            service["type"].as_str().unwrap_or(""),
            service["port"],
            service["source"].as_str().unwrap_or(""),
            if service["published"] == json!(true) {
                "published"
            } else {
                "not-published"
            },
        ));
    }
    for refused in state["refused"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "refused {}: {}\n",
            refused["source"].as_str().unwrap_or(""),
            refused["error"].as_str().unwrap_or(""),
        ));
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
        assert_eq!(parse(&words("list")), Ok(Command::List));
        assert_eq!(parse(&words("on")), Ok(Command::Switch(true)));
        assert_eq!(parse(&words("off")), Ok(Command::Switch(false)));
        assert_eq!(
            parse(&words("remove grafana")),
            Ok(Command::Remove("grafana".into()))
        );
        assert_eq!(parse(&[]), Ok(Command::Help));
        assert_eq!(
            parse(&words(
                "add grafana --type _http._tcp --port 3000 --instance Grafana --txt path=/d --txt a="
            )),
            Ok(Command::Add(
                "grafana".into(),
                Entry {
                    instance: Some("Grafana".into()),
                    kind: "_http._tcp".into(),
                    port: 3000,
                    txt: BTreeMap::from([
                        ("path".into(), "/d".into()),
                        ("a".into(), String::new())
                    ]),
                    service: None,
                }
            ))
        );
        for misuse in [
            "add Grafana --type _http._tcp --port 1",
            "add g --type _http._tcp",
            "add g --port 1",
            "add g --type _mica._tcp --port 1",
            "add g --type _http._tcp --port 0",
            "add g --type _http._tcp --port x",
            "add g --type _http._tcp --port 1 --txt novalue",
            "add g --type _http._tcp --port 1 --other x",
            "add g --type",
            "remove ../x",
            "restart",
        ] {
            assert!(parse(&words(misuse)).is_err(), "{misuse}");
        }
    }

    #[test]
    fn add_writes_a_drop_in_the_responder_reads_and_remove_takes_it() {
        let (image, runtime) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let Ok(Command::Add(name, entry)) =
            parse(&words("add grafana --type _http._tcp --port 3000"))
        else {
            panic!("add parses")
        };

        add_file(runtime.path(), &name, &entry).unwrap();

        let owner = rustix::process::geteuid().as_raw();
        let found = entries::drop_ins(image.path(), runtime.path(), owner);
        assert_eq!(found["runtime:grafana"], Ok(vec![entry]));
        assert_eq!(std::fs::read_dir(runtime.path()).unwrap().count(), 1);

        remove_file(runtime.path(), "grafana").unwrap();
        assert!(entries::drop_ins(image.path(), runtime.path(), owner).is_empty());
        assert!(remove_file(runtime.path(), "grafana").is_err());
    }

    #[test]
    fn status_and_list_say_what_is_published_and_what_is_refused() {
        let state = json!({
            "enabled": true,
            "source": "setting",
            "hostname": "mica-ab12.local",
            "services": [
                { "instance": "mica-ab12", "type": "_mica._tcp", "port": 8080, "source": "built-in", "published": true },
                { "instance": "Grafana", "type": "_http._tcp", "port": 3000, "source": "image:grafana", "published": true },
            ],
            "refused": [{ "source": "runtime:broken", "error": "EOF while parsing" }],
        });
        assert_eq!(
            render_status(&state),
            "enabled: true (setting)\nhostname: mica-ab12.local\npublished: 2 of 2\n"
        );
        assert_eq!(
            render_list(&state),
            "mica-ab12._mica._tcp port 8080 built-in published\n\
             Grafana._http._tcp port 3000 image:grafana published\n\
             refused runtime:broken: EOF while parsing\n"
        );
    }
}
