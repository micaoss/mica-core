//! `mica`'s credential and settings verbs end to end: the command against a
//! real apid's local socket, with a real micad on a private bus behind it.

use anyhow::Context;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use super::*;

/// One run of `mica` against the apid at `socket`, `stdin` on its standard
/// input; no system bus to reach.
fn mica(dir: &Path, socket: &Path, args: &[&str], stdin: &str) -> anyhow::Result<Output> {
    let link = dir.join("mica");
    if !link.exists() {
        std::os::unix::fs::symlink(find_micad()?, &link)?;
    }
    let mut child = Command::new(&link)
        .args(args)
        .env_clear()
        .env("APID_LOCAL_SOCKET", socket)
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            format!("unix:path={}", dir.join("no-bus").display()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("piped stdin")?
        .write_all(stdin.as_bytes())?;
    Ok(child.wait_with_output()?)
}

fn code(output: &Output) -> Option<i32> {
    output.status.code()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A file holding `content`, for a `--…-file` flag.
fn file(dir: &Path, name: &str, content: &str) -> anyhow::Result<PathBuf> {
    let path = dir.join(name);
    std::fs::write(&path, content)?;
    Ok(path)
}

#[tokio::test(flavor = "multi_thread")]
pub(super) async fn mica_end_to_end() -> anyhow::Result<()> {
    let mut bus_child = Command::new(dbus_daemon())
        .args(["--session", "--print-address=1", "--nofork"])
        .stdout(Stdio::piped())
        .spawn()?;
    let bus_stdout = bus_child.stdout.take().expect("piped stdout");
    let _bus_guard = ChildGuard(bus_child);
    let mut address = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(bus_stdout), &mut address)?;
    let address = address.trim().to_string();
    anyhow::ensure!(!address.is_empty(), "dbus-daemon printed no address");

    let dir = tempfile::tempdir()?;
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir)?;
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, "root:*:20000:0:99999:7:::\n")?;
    let _micad_guard = ChildGuard(
        Command::new(find_micad()?)
            .arg("daemon")
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env("MICAD_BUS", "session")
            .env("MICAD_DRY_RUN", "1")
            .env("MICAD_SETTINGS_PATH", dir.path().join("settings.toml"))
            .env("MICAD_CONFIG_DIR", &config_dir)
            .env("MICAD_SHADOW_PATH", &shadow_path)
            .spawn()?,
    );
    let state_dir = dir.path().join("apid");
    let socket = dir.path().join("apid.sock");
    let mut apid_child = Command::new(env!("CARGO_BIN_EXE_mica-apid"))
        .env("DBUS_SESSION_BUS_ADDRESS", &address)
        .env("APID_BUS", "session")
        .env("APID_STATE_DIR", &state_dir)
        .env("APID_UI_DIR", poured::console(dir.path())?)
        .env("APID_HTTPS_ADDR", "127.0.0.1:0")
        .env("APID_HTTP_ADDR", "127.0.0.1:0")
        .env("APID_LOCAL_SOCKET", &socket)
        .env("APID_LISTENERS_FILE", dir.path().join("apid.json"))
        .stdout(Stdio::piped())
        .spawn()?;
    let apid_stdout = apid_child.stdout.take().expect("piped stdout");
    let _apid_guard = ChildGuard(apid_child);
    wait_for_line(apid_stdout, "APID_LISTENING ")?;
    let listeners = dir.path().join("apid.json");
    let mut answered = false;
    for _ in 0..100 {
        answered = healthcheck(&socket, &listeners)?;
        if answered {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(answered, "apid answers on its local socket");
    let connection = zbus::connection::Builder::address(address.as_str())
        .build()
        .await?;
    let proxy = MicadProxy::new(&connection).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while proxy.get_settings("").await.is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("micad did not come up on the private bus")?;
    let d = dir.path();

    // Setup, with the password from a file: the device leaves setup mode.
    let password = file(d, "password", "first-password\n")?;
    let out = mica(
        d,
        &socket,
        &[
            "setup",
            "--hostname",
            "cli-host",
            "--password-file",
            password.to_str().unwrap(),
        ],
        "",
    )?;
    assert_eq!(code(&out), Some(0), "{}", text(&out.stderr));
    let again = mica(
        d,
        &socket,
        &["setup", "--password-file", password.to_str().unwrap()],
        "",
    )?;
    assert_eq!(code(&again), Some(1), "{}", text(&again.stderr));

    // A wrong current password is exit 4, audited as the local caller's, and
    // backs off the next change even with the right one.
    let wrong = file(d, "wrong", "not-the-password\n")?;
    let new = file(d, "new", "second-password\n")?;
    let out = mica(
        d,
        &socket,
        &[
            "password",
            "change",
            "--current-password-file",
            wrong.to_str().unwrap(),
            "--password-file",
            new.to_str().unwrap(),
        ],
        "",
    )?;
    assert_eq!(code(&out), Some(4), "{}", text(&out.stderr));
    let audit = std::fs::read_to_string(state_dir.join("audit.log"))?;
    assert!(
        audit.lines().any(|line| line.contains("\"password\"")
            && line.contains("wrong-password")
            && line.contains("\"local uid 0\"")),
        "{audit}"
    );
    let out = mica(
        d,
        &socket,
        &[
            "password",
            "change",
            "--current-password-file",
            password.to_str().unwrap(),
            "--password-file",
            new.to_str().unwrap(),
            "--json",
        ],
        "",
    )?;
    assert_eq!(code(&out), Some(2), "--json on a write is a misuse");
    let out = mica(
        d,
        &socket,
        &[
            "password",
            "change",
            "--current-password-file",
            password.to_str().unwrap(),
            "--password-file",
            new.to_str().unwrap(),
        ],
        "",
    )?;
    assert_eq!(code(&out), Some(4), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("retry shortly"),
        "{}",
        text(&out.stderr)
    );

    // Tokens: created, printed once, listed, revoked.
    let out = mica(d, &socket, &["tokens", "create", "ci", "--json"], "")?;
    assert_eq!(code(&out), Some(0), "{}", text(&out.stderr));
    let minted: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let id = minted["id"].as_str().context("token id")?.to_string();
    assert!(
        minted["token"]
            .as_str()
            .is_some_and(|token| token.starts_with("mica_"))
    );
    let out = mica(d, &socket, &["tokens", "list", "--json"], "")?;
    assert!(text(&out.stdout).contains(&id), "{}", text(&out.stdout));
    let out = mica(d, &socket, &["tokens", "revoke", &id], "")?;
    assert_eq!(code(&out), Some(0), "{}", text(&out.stderr));

    // SSH keys: added from stdin, listed by fingerprint, removed by it.
    let key =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIL99V7xPTOP3jZjnbVPM7xC+ckwzkOQPalUpsvtPzYo8 e2e@cli";
    let out = mica(d, &socket, &["ssh", "keys", "add", "-"], key)?;
    assert_eq!(code(&out), Some(0), "{}", text(&out.stderr));
    let out = mica(d, &socket, &["ssh", "keys", "list", "--json"], "")?;
    let list: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let fingerprint = list["keys"][0]["fingerprint"]
        .as_str()
        .context("a fingerprint")?
        .to_string();
    let out = mica(d, &socket, &["ssh", "keys", "remove", &fingerprint], "")?;
    assert_eq!(code(&out), Some(0), "{}", text(&out.stderr));

    // Settings, typed by the setting: a hostname of digits is a string.
    let out = mica(d, &socket, &["set", "hostname", "1234"], "")?;
    assert_eq!(code(&out), Some(0), "{}", text(&out.stderr));
    let out = mica(d, &socket, &["get", "hostname"], "")?;
    assert_eq!(text(&out.stdout), "1234\n");
    let out = mica(d, &socket, &["set", "container.enabled", "yes"], "")?;
    assert_eq!(code(&out), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("container.enabled takes true or false"));
    let out = mica(d, &socket, &["set", "no.such.setting", "x", "--json"], "")?;
    assert_eq!(code(&out), Some(2), "--json on a write is a misuse");
    let out = mica(d, &socket, &["get", "no.such.setting", "--json"], "")?;
    assert_eq!(code(&out), Some(1));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&out.stderr)?;
    assert_eq!(error["error"]["code"], "settings_not_found", "{error}");
    let out = mica(d, &socket, &["set", "no.such.setting", "x"], "")?;
    assert_eq!(code(&out), Some(1));
    assert!(
        text(&out.stderr).contains("no.such.setting"),
        "{}",
        text(&out.stderr)
    );

    // No apid answering: exit 3.
    let out = mica(d, &d.join("absent.sock"), &["hostname"], "")?;
    assert_eq!(code(&out), Some(3), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("apid is not answering"),
        "{}",
        text(&out.stderr)
    );
    Ok(())
}
