use super::*;
use serde_json::json;
use std::sync::Mutex as StdMutex;

mod acquisition;
mod client;
mod outputs;
mod policy;

fn output(code: i32, stdout: &str, stderr: &str) -> ClientOutput {
    ClientOutput {
        code: Some(code),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

fn selection_output() -> ClientOutput {
    output(
        0,
        &json!({"revision":1,"selected":{
            "deploymentId":"a".repeat(64),"deployment":{"version":"1.1.0"}}
        })
        .to_string(),
        "",
    )
}

fn fetch_output(path: &str) -> ClientOutput {
    output(
        0,
        &json!({"id":"a".repeat(64),"path":path,
            "objects":"/mica/updates/verified/objects","version":"1.1.0","generation":3
        })
        .to_string(),
        "",
    )
}

fn staged_path() -> String {
    format!("/mica/updates/verified/{}.json", "a".repeat(64))
}

/// What a client from before core sets answers a core verb.
fn old_client(verb: &str) -> ClientOutput {
    output(
        2,
        "",
        &format!(
            "error: unrecognized subcommand '{verb}'\n\nUsage: mica-deploy [OPTIONS] <COMMAND>"
        ),
    )
}

/// `core-check` naming a newer set of the channel.
fn core_selection_output() -> ClientOutput {
    output(
        0,
        &json!({"revision":1,"selected":{
            "id":"core.general.20261010-0000","coreSetId":"c".repeat(64),
            "set":{"channel":"general","generation":4,"version":"0.0.6"}}
        })
        .to_string(),
        "",
    )
}

fn core_staged_path() -> String {
    format!("/mica/updates/verified/core-{}.json", "c".repeat(64))
}

/// `core-fetch` leaving the set ready.
fn core_fetch_output() -> ClientOutput {
    output(
        0,
        &json!({"id":"c".repeat(64),"path":core_staged_path(),
            "objects":"/mica/updates/verified/objects","channel":"general",
            "version":"0.0.6","generation":4
        })
        .to_string(),
        "",
    )
}

fn no_selection() -> ClientOutput {
    output(0, r#"{"revision":1,"selected":null}"#, "")
}

/// A client killed by a signal: no exit code at all.
fn output_signalled(stderr: &str) -> ClientOutput {
    ClientOutput {
        code: None,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

/// Scripted client: each expected invocation is `(leading subcommand,
/// result)`, consumed in order; the full argv of every call is logged.
struct MockClient {
    script: StdMutex<Vec<(String, Result<ClientOutput, String>)>>,
    calls: Arc<StdMutex<Vec<Vec<String>>>>,
    unavailable: Option<String>,
}

impl MockClient {
    fn new(script: Vec<(&str, Result<ClientOutput, String>)>) -> Self {
        Self {
            script: StdMutex::new(
                script
                    .into_iter()
                    .map(|(verb, result)| (verb.to_string(), result))
                    .collect(),
            ),
            calls: Arc::new(StdMutex::new(Vec::new())),
            unavailable: None,
        }
    }

    fn absent(reason: &str) -> Self {
        Self {
            script: StdMutex::new(Vec::new()),
            calls: Arc::new(StdMutex::new(Vec::new())),
            unavailable: Some(reason.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl UpdateClient for MockClient {
    fn unavailable(&self) -> Option<String> {
        self.unavailable.clone()
    }

    async fn run(&self, args: &[String], _timeout: Duration) -> Result<ClientOutput> {
        let verb_at = usize::from(args.first().is_some_and(|arg| arg == "--max-bytes")) * 2;
        let mut script = self.script.lock().expect("script");
        // A script that does not name a core verb is a client from before core
        // sets, which answers one as clap answers any unknown subcommand. The
        // tests of the product line run against that client, and their call
        // lists stay the product line's.
        if let Some(verb) = args.get(verb_at).filter(|verb| verb.starts_with("core-"))
            && script.first().is_none_or(|(next, _)| next != verb)
        {
            return Ok(old_client(verb));
        }
        self.calls.lock().expect("calls").push(args.to_vec());
        anyhow::ensure!(!script.is_empty(), "unexpected client call: {args:?}");
        let (verb, result) = script.remove(0);
        anyhow::ensure!(
            args.get(usize::from(args.first().is_some_and(|arg| arg == "--max-bytes")) * 2)
                == Some(&verb),
            "expected `{verb}`, got {args:?}"
        );
        result.map_err(|reason| anyhow::anyhow!("{reason}"))
    }
}

/// Host recording every `update.lifecycle` write and serving a health
/// tree the test controls.
struct TestHost {
    recorded: StdMutex<Vec<Value>>,
    health: StdMutex<Value>,
}

impl TestHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            recorded: StdMutex::new(Vec::new()),
            health: StdMutex::new(json!({})),
        })
    }

    fn last(&self) -> Value {
        self.recorded
            .lock()
            .expect("recorded")
            .last()
            .cloned()
            .expect("at least one record")
    }

    fn set_health(&self, health: Value) {
        *self.health.lock().expect("health") = health;
    }
}

#[async_trait::async_trait]
impl LifecycleHost for TestHost {
    async fn record(&self, lifecycle: Value) {
        self.recorded.lock().expect("recorded").push(lifecycle);
    }

    async fn health(&self) -> Value {
        self.health.lock().expect("health").clone()
    }
}

fn policy_file(dir: &tempfile::TempDir, body: &str) -> PolicyStore {
    let path = dir.path().join("updates.json");
    std::fs::write(&path, body).expect("seed policy");
    PolicyStore::at(path)
}

fn lifecycle(
    client: MockClient,
    policy: PolicyStore,
    host: Arc<TestHost>,
) -> (Arc<UpdateLifecycle>, Arc<AtomicBool>) {
    let installing = Arc::new(AtomicBool::new(false));
    (
        Arc::new(UpdateLifecycle::new(
            Arc::new(client),
            policy,
            host,
            Arc::clone(&installing),
            PathBuf::from(DEFAULT_WORKSPACE_ROOT),
        )),
        installing,
    )
}

/// The probe answer of a healthy workspace, first in every script.
fn ready_probe() -> (&'static str, Result<ClientOutput, String>) {
    (
        "probe",
        Ok(output(
            0,
            r#"{"status":"ready","root":"/mica/updates","freeBytes":1000000000,"maxBytes":500000000,"freeInodes":10000}"#,
            "",
        )),
    )
}

/// Poll the host until the recorded state leaves `busy` states.
async fn settled(host: &TestHost) -> Value {
    for _ in 0..500 {
        let last = host.last();
        let state = last["state"].as_str().unwrap_or_default();
        if state != "checking" && state != "downloading" {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("lifecycle never settled: {}", host.last());
}
