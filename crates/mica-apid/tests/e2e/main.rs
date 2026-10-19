//! End-to-end test: private `dbus-daemon --session` + real `micad` + real
//! `apid`, driven over HTTPS/HTTP with a real client.
//!
//! Everything lives in tempdirs on ephemeral ports; `MICAD_DRY_RUN=1` keeps
//! the host untouched. A missing `dbus-daemon` is a failure, never a skip.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use anyhow::Context;
use reqwest::StatusCode;
use reqwest::header::LOCATION;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

mod poured;
mod web_flow;
use poured::*;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn dbus_daemon() -> PathBuf {
    let fixed = PathBuf::from("/usr/bin/dbus-daemon");
    if fixed.exists() {
        return fixed;
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("dbus-daemon"))
                .find(|candidate| candidate.exists())
        })
        .unwrap_or_else(|| {
            panic!(
                "dbus-daemon was not found at /usr/bin/dbus-daemon or on PATH; install the \
                 dbus-daemon package because this real-bus test must not skip"
            )
        })
}

fn find_micad() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("MICAD_BIN") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().context("locate test executable")?;
    let candidate = exe
        .parent()
        .and_then(|deps| deps.parent())
        .context("test executable has no target profile directory")?
        .join("micad");
    anyhow::ensure!(
        candidate.exists(),
        "micad binary not found at {}; build it with `cargo build -p micad` or set MICAD_BIN",
        candidate.display()
    );
    Ok(candidate)
}

#[zbus::proxy(
    interface = "com.mica.micad1",
    default_service = "com.mica.micad",
    default_path = "/com/mica/micad"
)]
trait Micad {
    fn get_settings(&self, path: &str) -> zbus::Result<String>;
    fn get_state(&self, path: &str) -> zbus::Result<String>;
}

/// `mica-apid --healthcheck` against `https_addr`, bounded so that an apid that
/// ignored the flag and started serving cannot hang the test.
fn healthcheck(https_addr: &str) -> anyhow::Result<bool> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mica-apid"))
        .arg("--healthcheck")
        .env_clear()
        .env("APID_HTTPS_ADDR", https_addr)
        .env("APID_STATE_DIR", "/nonexistent/apid-healthcheck")
        .stdout(Stdio::null())
        .spawn()?;
    for _ in 0..200 {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    anyhow::bail!("mica-apid --healthcheck did not exit")
}

fn wait_for_line(stdout: ChildStdout, prefix: &'static str) -> anyhow::Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.starts_with(prefix) {
                let _ = tx.send(line);
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(30))
        .with_context(|| format!("timed out waiting for `{prefix}` on stdout"))
}

fn http_client(cookies: bool) -> anyhow::Result<reqwest::Client> {
    // The client carries no crypto provider of its own: the process's default is
    // aws-lc-rs, as it is apid's. Installing it twice is not an error here.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .cookie_store(cookies)
        .build()
        .context("build reqwest client")
}

fn location(response: &reqwest::Response) -> &str {
    response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("(no Location header)")
}

fn module_script_src(html: &str) -> Option<&str> {
    let source = html.split_once("<script type=\"module\"")?.1;
    let source = source.split_once("src=\"")?.1;
    source.split_once('"').map(|(path, _)| path)
}

async fn response_json(response: reqwest::Response) -> anyhow::Result<serde_json::Value> {
    serde_json::from_str(&response.text().await?).context("parse JSON response")
}

async fn wait_for_task(
    client: &reqwest::Client,
    https_base: &str,
    task_id: &str,
) -> anyhow::Result<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = client
                .get(format!("{https_base}/api/v1/tasks/{task_id}"))
                .send()
                .await?;
            anyhow::ensure!(response.status() == StatusCode::OK, "task lookup failed");
            let task = response_json(response).await?;
            if task["status"] == "finished" {
                break anyhow::Ok(task);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("apply task did not finish")?
}
