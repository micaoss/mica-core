//! `mica-ctr` (or `micad ctr`), the operator's containers command: micad over
//! the system bus.
//!
//! A project put here is stored in micad's settings, the same set the console
//! and the API change, and micad declares it to mica-containerd. The bus
//! policy makes it root's.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const USAGE: &str = "\
usage: mica-ctr up [-p <project>] [-f <file|->] [--project-dir <dir>]
       mica-ctr down [-p <project>]
       mica-ctr list (or ps)
       mica-ctr check [-p <project>] [-f <file|->] [--project-dir <dir>]
       mica-ctr start|stop|restart <project>[/<service>]
       mica-ctr logs <project>/<service> [--tail N]
       (micad ctr ... is the same command)

  up       check a Compose file, store it as the project with its directory and
           turn containers on
  down     remove the project: its containers and network go, its files stay
  list     every project's containers and their state
  check    check a Compose file without storing it
  start, stop, restart
           the project's every container, or one service's
  logs     the last lines of one service's log (100 unless --tail)

  As docker compose: the file is the first of compose.yaml, compose.yml,
  docker-compose.yaml and docker-compose.yml in the working directory unless -f
  names one (- is stdin); the project directory, where relative host paths
  resolve, is the file's directory (the working directory for stdin) unless
  --project-dir names one; the project is -p, else the file's name key, else the
  project directory's name in lower case without other characters than a-z,
  0-9, _ and -.";

/// The names a Compose file is found by in the working directory, in order,
/// as docker compose finds one.
const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// How often a task is read while micad applies it.
const POLL: Duration = Duration::from_millis(500);
/// How long a reconcile may take before the command stops waiting for it: the
/// container reconciler waits up to 30 s for the daemon and 60 s for
/// containers to stop.
const TASK_TIMEOUT: Duration = Duration::from_secs(300);
/// The lines `logs` shows unless asked.
const DEFAULT_TAIL: u32 = 100;
/// The largest file `put` and `check` read: a project's bound, and one byte.
const MAX_FILE: u64 = micad_settings::MAX_CONTAINER_PROJECT_BYTES as u64 + 1;

#[zbus::proxy(
    interface = "com.mica.micad1",
    default_service = "com.mica.micad",
    default_path = "/com/mica/micad"
)]
trait Micad {
    fn get_settings(&self, path: &str) -> zbus::Result<String>;
    fn set_settings(&self, path: &str, value_json: &str) -> zbus::Result<String>;
    fn get_task(&self, id: &str) -> zbus::Result<String>;
    fn get_containers(&self) -> zbus::Result<String>;
    fn check_container_project_in(
        &self,
        project: &str,
        text: &str,
        project_dir: &str,
    ) -> zbus::Result<String>;
    fn get_container_log(&self, name: &str, tail: u32) -> zbus::Result<String>;
    fn start_container(&self, name: &str) -> zbus::Result<()>;
    fn stop_container(&self, name: &str) -> zbus::Result<()>;
    fn restart_container(&self, name: &str) -> zbus::Result<()>;
}

/// What names a project: `-p`, `-f` and `--project-dir`, as docker compose
/// takes them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Target {
    project: Option<String>,
    file: Option<String>,
    dir: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Up(Target),
    Down(Target),
    List,
    Check(Target),
    Verb { verb: Verb, name: String },
    Logs { name: String, tail: u32 },
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Start,
    Stop,
    Restart,
}

/// The flags of `up`, `down` and `check`; `down` takes no file.
fn target(words: &[&str], files: bool) -> Result<Target, String> {
    let mut target = Target::default();
    let mut rest = words.iter();
    while let Some(flag) = rest.next() {
        let slot = match *flag {
            "-p" | "--project" => &mut target.project,
            "-f" | "--file" if files => &mut target.file,
            "--project-dir" if files => &mut target.dir,
            other => return Err(format!("unexpected argument: {other}")),
        };
        let value = rest.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if slot.replace((*value).to_string()).is_some() {
            return Err(format!("{flag} is given twice"));
        }
    }
    if let Some(name) = &target.project {
        project_name(name)?;
    }
    Ok(target)
}

/// `name` when it is a project name mica-containerd takes.
fn project_name(name: &str) -> Result<String, String> {
    if micad_settings::is_project_name(name) {
        Ok(name.to_string())
    } else {
        Err(format!(
            "{name:?} is not a project name: 1 to 40 of a-z, 0-9, `_` and `-`, starting with a letter or a digit"
        ))
    }
}

fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["up", rest @ ..] => Ok(Command::Up(target(rest, true)?)),
        ["down", rest @ ..] => Ok(Command::Down(target(rest, false)?)),
        ["list" | "ps"] => Ok(Command::List),
        ["check", rest @ ..] => Ok(Command::Check(target(rest, true)?)),
        [verb @ ("start" | "stop" | "restart"), name] => Ok(Command::Verb {
            verb: match *verb {
                "start" => Verb::Start,
                "stop" => Verb::Stop,
                _ => Verb::Restart,
            },
            name: (*name).to_string(),
        }),
        ["logs", name, rest @ ..] => {
            let tail = match rest {
                [] => DEFAULT_TAIL,
                ["--tail", n] => n
                    .parse()
                    .ok()
                    .filter(|n| (1..=1000).contains(n))
                    .ok_or_else(|| format!("--tail is 1 to 1000 lines, not {n:?}"))?,
                _ => return Err(format!("unexpected arguments: {}", rest.join(" "))),
            };
            if !name.contains('/') {
                return Err(format!(
                    "logs names one service: <project>/<service>, not {name:?}"
                ));
            }
            Ok(Command::Logs {
                name: (*name).to_string(),
                tail,
            })
        }
        [] | ["help" | "-h" | "--help"] => Ok(Command::Help),
        [other, ..] => Err(format!("unknown command or arguments: {other}")),
    }
}

/// `mica-ctr`'s entry point: exit 0 done, 1 failed, 2 misused.
///
/// `args` are the words after the command: after `mica-ctr`, or after
/// `micad ctr`.
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
            eprintln!("mica-ctr: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(run(command)) {
        Ok(true) => Ok(()),
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("mica-ctr: {error:#}");
            std::process::exit(1);
        }
    }
}

/// Run one command; `false` when it printed its own refusal.
async fn run(command: Command) -> Result<bool> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to the system bus")?;
    let micad = MicadProxy::new(&connection).await?;
    if matches!(
        command,
        Command::Up(_) | Command::Down(_) | Command::Check(_)
    ) {
        let doc: Value = serde_json::from_str(&micad.get_containers().await.map_err(bus_error)?)?;
        if doc["containerEngine"] == json!("docker") {
            bail!("{}; use docker compose", crate::docker::USE_DOCKER_COMPOSE);
        }
    }
    match command {
        Command::Up(target) => {
            let found = resolve(&target, &working_dir()?, &mut std::io::stdin())?;
            let dir = found.dir()?;
            if !checked(&micad, &found.name, &found.text, dir, false).await? {
                return Ok(false);
            }
            let mut container: Value =
                serde_json::from_str(&micad.get_settings("container").await.map_err(bus_error)?)?;
            container["enabled"] = json!(true);
            container["projects"][&found.name] = json!(found.text);
            container["projectDirs"][&found.name] = json!(dir);
            let task = micad
                .set_settings("container", &container.to_string())
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("{} declared, in {dir}", found.name);
            Ok(true)
        }
        Command::Down(target) => {
            let project = down_project(&target, &working_dir()?)?;
            let mut projects: serde_json::Map<String, Value> = match serde_json::from_str(
                &micad
                    .get_settings("container.projects")
                    .await
                    .map_err(bus_error)?,
            )? {
                Value::Object(projects) => projects,
                _ => serde_json::Map::new(),
            };
            if projects.remove(&project).is_none() {
                bail!("no project named {project} is declared");
            }
            let task = micad
                .set_settings("container.projects", &Value::Object(projects).to_string())
                .await
                .map_err(bus_error)?;
            wait(&micad, &task).await?;
            println!("{project} removed; its files and volumes stay");
            Ok(true)
        }
        Command::List => {
            let doc: Value =
                serde_json::from_str(&micad.get_containers().await.map_err(bus_error)?)?;
            print!("{}", render_list(&doc));
            Ok(true)
        }
        Command::Check(target) => {
            let found = resolve(&target, &working_dir()?, &mut std::io::stdin())?;
            checked(&micad, &found.name, &found.text, found.dir()?, true).await
        }
        Command::Verb { verb, name } => {
            match verb {
                Verb::Start => micad.start_container(&name).await,
                Verb::Stop => micad.stop_container(&name).await,
                Verb::Restart => micad.restart_container(&name).await,
            }
            .map_err(bus_error)?;
            Ok(true)
        }
        Command::Logs { name, tail } => {
            print!(
                "{}",
                micad
                    .get_container_log(&name, tail)
                    .await
                    .map_err(bus_error)?
            );
            Ok(true)
        }
        Command::Help => Ok(true),
    }
}

/// A project as docker compose finds it: its name, its directory and its
/// Compose text.
#[derive(Debug, PartialEq, Eq)]
struct Found {
    name: String,
    dir: PathBuf,
    text: String,
}

impl Found {
    fn dir(&self) -> Result<&str> {
        self.dir
            .to_str()
            .with_context(|| format!("{} is not a UTF-8 path", self.dir.display()))
    }
}

fn working_dir() -> Result<PathBuf> {
    std::env::current_dir().context("read the working directory")
}

/// The first Compose file of `dir` by docker compose's names.
fn compose_file_in(dir: &Path) -> Option<PathBuf> {
    COMPOSE_FILES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// `path` made absolute against `cwd`, with `.` and `..` taken out.
fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in cwd.join(path).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The project `target` names from `cwd`: its file (`-` read from `stdin`),
/// the file's directory or `--project-dir`, and `-p`, the file's name key or
/// the directory's name.
fn resolve(target: &Target, cwd: &Path, stdin: &mut dyn Read) -> Result<Found> {
    let (text, mut dir) = match target.file.as_deref() {
        Some("-") => (read_bounded(stdin, "stdin")?, cwd.to_path_buf()),
        Some(file) => {
            let path = absolute(cwd, Path::new(file));
            let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
            (read_file(&path)?, dir)
        }
        None => {
            let path = compose_file_in(cwd).with_context(|| {
                format!(
                    "no Compose file: none of {} is in {}; name one with -f",
                    COMPOSE_FILES.join(", "),
                    cwd.display()
                )
            })?;
            (read_file(&path)?, cwd.to_path_buf())
        }
    };
    if let Some(named) = &target.dir {
        dir = absolute(cwd, Path::new(named));
        if !dir.is_dir() {
            bail!("{} is not a directory", dir.display());
        }
    }
    let name = match &target.project {
        Some(name) => name.clone(),
        None => default_project(&text, &dir)?,
    };
    Ok(Found { name, dir, text })
}

/// The project `down` removes: `-p`, else the name key of the working
/// directory's Compose file, else the directory's name.
fn down_project(target: &Target, cwd: &Path) -> Result<String> {
    if let Some(name) = &target.project {
        return Ok(name.clone());
    }
    let text = match compose_file_in(cwd) {
        Some(path) => read_file(&path)?,
        None => String::new(),
    };
    default_project(&text, cwd)
}

/// The file's name key, else `dir`'s name made a project name.
fn default_project(text: &str, dir: &Path) -> Result<String> {
    let name = match name_key(text) {
        Some(name) => name,
        None => normalized(
            &dir.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default(),
        ),
    };
    if name.is_empty() {
        bail!("{} names no project; name one with -p", dir.display());
    }
    project_name(&name).map_err(anyhow::Error::msg)
}

/// The value of a top-level `name:` line, unquoted.
fn name_key(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix("name:")?;
        let value = value.split(" #").next().unwrap_or_default().trim();
        let value = ['"', '\'']
            .iter()
            .find_map(|quote| value.strip_prefix(*quote)?.strip_suffix(*quote))
            .unwrap_or(value);
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// A directory's name as docker compose makes it a project name: lower case,
/// only `a-z`, `0-9`, `_` and `-`, none of `_` and `-` leading.
fn normalized(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        .collect::<String>()
        .trim_start_matches(['_', '-'])
        .to_string()
}

/// A file's text, bounded as a project is.
fn read_file(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    read_bounded(&mut &file, &path.display().to_string())
}

fn read_bounded(reader: &mut dyn Read, name: &str) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_FILE)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {name}"))?;
    if bytes.len() as u64 >= MAX_FILE {
        bail!("{name} is larger than a project may be, 64 KiB");
    }
    String::from_utf8(bytes).with_context(|| format!("{name} is not UTF-8 text"))
}

/// Check `text` as `project` in `dir`, printing the problems on stderr; with
/// `show`, the containers it becomes on stdout.
async fn checked(
    micad: &MicadProxy<'_>,
    project: &str,
    text: &str,
    dir: &str,
    show: bool,
) -> Result<bool> {
    let outcome: Value = serde_json::from_str(
        &micad
            .check_container_project_in(project, text, dir)
            .await
            .map_err(bus_error)?,
    )?;
    if outcome["ok"] == json!(true) {
        if show {
            println!("{}", serde_json::to_string_pretty(&outcome["containers"])?);
        }
        return Ok(true);
    }
    eprint!("{}", render_problems(project, &outcome));
    Ok(false)
}

/// A refused check, one problem a line.
fn render_problems(project: &str, outcome: &Value) -> String {
    let mut out = format!("mica-ctr: project {project} is refused\n");
    let problems = outcome["problems"].as_array().cloned().unwrap_or_default();
    if problems.is_empty() {
        out.push_str(&format!(
            "  {}\n",
            outcome["detail"]
                .as_str()
                .unwrap_or("the Compose file is refused")
        ));
    }
    for problem in problems {
        let at = [problem["service"].as_str(), problem["field"].as_str()]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(".");
        let detail = problem["detail"].as_str().unwrap_or("refused");
        if at.is_empty() {
            out.push_str(&format!("  {detail}\n"));
        } else {
            out.push_str(&format!("  {at}: {detail}\n"));
        }
    }
    out
}

/// Every project's containers.
fn render_list(doc: &Value) -> String {
    let mut rows = vec![[
        "PROJECT".to_string(),
        "SERVICE".to_string(),
        "CONTAINER".to_string(),
        "PHASE".to_string(),
        "HEALTH".to_string(),
    ]];
    let engine = &doc["engine"];
    let field = |entry: &Value, key: &str| {
        entry[key]
            .as_str()
            .filter(|value| !value.is_empty())
            .unwrap_or("-")
            .to_string()
    };
    let listed: Vec<Value> = engine["projects"].as_array().cloned().unwrap_or_default();
    // On docker nothing is declared, and every project dockerd holds is listed.
    let docker = doc["containerEngine"] == json!("docker");
    let names: Vec<String> = if docker {
        listed
            .iter()
            .filter_map(|project| project["name"].as_str().map(str::to_string))
            .collect()
    } else {
        doc["projects"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, _)| name.clone())
            .collect()
    };
    for name in &names {
        let services: Vec<Value> = listed
            .iter()
            .filter(|project| project["name"].as_str() == Some(name))
            .flat_map(|project| project["services"].as_array().cloned().unwrap_or_default())
            .collect();
        if services.is_empty() {
            rows.push([
                name.clone(),
                "-".into(),
                "-".into(),
                "not running".into(),
                "-".into(),
            ]);
        }
        for entry in services {
            rows.push([
                name.clone(),
                field(&entry, "service"),
                entry["spec"]["name"].as_str().unwrap_or("-").to_string(),
                field(&entry, "phase"),
                field(&entry, "health"),
            ]);
        }
    }
    let widths: Vec<usize> = (0..5)
        .map(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    if doc["enabled"] != json!(true) && docker {
        out.push_str("containers are off: `container.enabled` turns dockerd on\n");
    } else if doc["enabled"] != json!(true) {
        out.push_str("containers are off: `mica-ctr up` turns them on\n");
    } else if engine["available"] != json!(true) {
        out.push_str(&format!(
            "{} is not answering: {}\n",
            if docker { "dockerd" } else { "mica-containerd" },
            engine["detail"].as_str().unwrap_or("no reason given")
        ));
    }
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

/// Wait until micad has applied `task`, and fail with its message if the
/// reconcile failed.
async fn wait(micad: &MicadProxy<'_>, task: &str) -> Result<()> {
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
fn bus_error(error: zbus::Error) -> anyhow::Error {
    match crate::bus::error_reply(&error) {
        Some((name, message)) => anyhow::anyhow!(message.unwrap_or(name)),
        None => anyhow::Error::new(error).context("micad"),
    }
}

#[cfg(test)]
mod tests;
