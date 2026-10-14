//! A poured configuration document, adopted, and its secret.

use anyhow::Context;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::*;

/// The site key an integrator pours into `wifi.json`.
///
/// A string with no other reason to exist anywhere on the device, so a probe
/// that searches a response body for it is asserting about this key and not
/// about a word that happens to be common in JSON.
pub(super) const POURED_PSK: &str = "poured-site-key-9d4ec7b0";

/// The AP's SSID from the same document: not a secret, and the control that
/// keeps the search below from passing vacuously.
pub(super) const POURED_SSID: &str = "poured-ap-e2e";

/// A second secret, in a document that **fails**, and in a field whose type it
/// does not fit — so serde's own sentence has to print it back.
pub(super) const REFUSED_SECRET: &str = "refused-broker-secret-3f2a9c41";

/// `wifi.json` exactly as an integrator hand-writes it with the DATA partition
/// mounted on a laptop — no `schema_version` ceremony beyond the one key, no
/// field they do not care about, and two secrets in the two places the schema
/// puts them.
pub(super) fn poured_wifi_document() -> String {
    format!(
        r#"{{
  "schema_version": 1,
  "wifi": {{
    "ap": {{ "mode": "always", "ssid": "{POURED_SSID}", "psk": "{POURED_PSK}" }},
    "client": {{
      "enabled": true,
      "networks": [{{ "ssid": "site-uplink", "psk": "{POURED_PSK}" }}]
    }}
  }}
}}
"#
    )
}

/// Every GET path this build publishes, from the document that publishes them.
pub(super) fn published_get_paths() -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.json"),
    )
    .context("read the published openapi.json")?;
    let document: serde_json::Value = serde_json::from_str(&text)?;
    let paths = document["paths"]
        .as_object()
        .context("openapi.json has no paths object")?;
    let mut gets: Vec<String> = paths
        .iter()
        .filter(|(_, item)| item.get("get").is_some())
        .map(|(path, _)| path.clone())
        .collect();
    gets.sort();
    anyhow::ensure!(!gets.is_empty(), "openapi.json declares no GET route");
    Ok(gets)
}

/// Concrete URLs for one published path.
pub(super) fn probe_urls(path: &str, task_id: &str, snapshot_id: &str) -> Vec<String> {
    let owned = |values: &[&str]| values.iter().map(|value| (*value).to_string()).collect();
    match path {
        // Every shape the settings root can be read in: the whole tree, the
        // subtree the poured document carries, the array inside it, and the
        // secret named directly — which is the one case the structural walk
        // cannot see, because a bare JSON string has no field name left in it.
        "/api/v1/settings/{path}" => owned(&[
            "/api/v1/settings/",
            "/api/v1/settings/wifi",
            "/api/v1/settings/wifi.ap",
            "/api/v1/settings/wifi.ap.psk",
            "/api/v1/settings/wifi.client",
            "/api/v1/settings/wifi.client.networks",
            "/api/v1/settings/access",
        ]),
        // The live-state route is per dot-path — there is no whole-tree form —
        // so the refusal list is asked for by name, beside a reconciler entry
        // and the root-ish `meta` that a poured document never reaches.
        "/api/v1/state/{path}" => owned(&[
            "/api/v1/state/configuration",
            "/api/v1/state/meta",
            "/api/v1/state/wifiAp",
        ]),
        "/api/v1/network/{iface}/peers" => owned(&["/api/v1/network/eth0/peers"]),
        // A service log: the device's own two, which every product carries.
        "/api/v1/system/logs/{source}" => {
            owned(&["/api/v1/system/logs/micad", "/api/v1/system/logs/apid"])
        }
        "/api/v1/tasks/{id}" => vec![format!("/api/v1/tasks/{task_id}")],
        "/api/v1/diagnostics/snapshots/{id}" => {
            vec![format!("/api/v1/diagnostics/snapshots/{snapshot_id}")]
        }
        other if other.contains('{') => Vec::new(),
        other => vec![other.to_string()],
    }
}

/// **Clauses 1 and 3 of the pour gate, on a real device.**
#[tokio::test(flavor = "multi_thread")]
pub(super) async fn a_poured_document_is_adopted_and_its_secret_reaches_no_served_record()
-> anyhow::Result<()> {
    let mut bus_child = Command::new(dbus_daemon())
        .args(["--session", "--print-address=1", "--nofork"])
        .stdout(Stdio::piped())
        .spawn()?;
    let bus_stdout = bus_child.stdout.take().expect("piped stdout");
    let _bus_guard = ChildGuard(bus_child);
    let mut address = String::new();
    BufReader::new(bus_stdout).read_line(&mut address)?;
    let address = address.trim().to_string();
    anyhow::ensure!(!address.is_empty(), "dbus-daemon printed no address");

    let dir = tempfile::tempdir()?;
    let settings_path = dir.path().join("settings.toml");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir)?;
    // THE POUR. Written before the daemon exists, which is the point: an integrator writes these files onto a device that is not
    // running, and a pour onto a running device is not supported.
    std::fs::write(config_dir.join("wifi.json"), poured_wifi_document())?;
    // And one the integrator got wrong, carrying a secret into the parser's
    // hands: `enabled` is a boolean, so the refusal's underlying sentence is
    // `invalid type: string "…", expected a boolean`.
    std::fs::write(
        config_dir.join("mqtt.json"),
        format!(r#"{{"schema_version": 1, "mqtt": {{"enabled": "{REFUSED_SECRET}"}}}}"#),
    )?;
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, "root:!:20000:0:99999:7:::\n")?;
    let _micad_guard = ChildGuard(
        Command::new(find_micad()?)
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env("MICAD_BUS", "session")
            .env("MICAD_DRY_RUN", "1")
            .env("MICAD_SETTINGS_PATH", &settings_path)
            .env("MICAD_CONFIG_DIR", &config_dir)
            .env("MICAD_SHADOW_PATH", &shadow_path)
            .spawn()?,
    );

    let mut apid_child = Command::new(env!("CARGO_BIN_EXE_mica-apid"))
        .env("DBUS_SESSION_BUS_ADDRESS", &address)
        .env("APID_BUS", "session")
        .env("APID_STATE_DIR", dir.path().join("apid"))
        .env("APID_HTTPS_ADDR", "127.0.0.1:0")
        .env("APID_HTTP_ADDR", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .spawn()?;
    let apid_stdout = apid_child.stdout.take().expect("piped stdout");
    let _apid_guard = ChildGuard(apid_child);
    let marker = wait_for_line(apid_stdout, "APID_LISTENING ")?;
    let https_addr = marker
        .split_whitespace()
        .find_map(|part| part.strip_prefix("https="))
        .with_context(|| format!("`https=` missing from marker `{marker}`"))?
        .to_string();
    let https_base = format!("https://{https_addr}");

    let connection = zbus::connection::Builder::address(address.as_str())?
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

    // **Clause 1.** The daemon adopted the hand-written document, secrets and
    // all: the bus surface is micad's own tree with no redactor in front of it,
    // so this is the device genuinely holding the key. Everything below is
    // about it not leaving.
    let tree: serde_json::Value = serde_json::from_str(&proxy.get_settings("").await?)?;
    assert_eq!(tree["wifi"]["ap"]["ssid"], POURED_SSID);
    assert_eq!(tree["wifi"]["ap"]["psk"], POURED_PSK);
    assert_eq!(tree["wifi"]["client"]["networks"][0]["psk"], POURED_PSK);
    assert_eq!(tree["wifi"]["client"]["enabled"], true);
    // **Clause 2 on the same device**: the broken document is refused, by name,
    // and the valid one beside it is not. One namespace, two documents, two
    // different answers — which is the whole difference between "refuses its
    // subsystem" and "refuses to start".
    let state: serde_json::Value = serde_json::from_str(&proxy.get_state("").await?)?;
    let refused = state["configuration"]["refused"]
        .as_array()
        .context("configuration.refused")?;
    assert_eq!(
        refused.len(),
        1,
        "only the broken document is refused: {refused:?}"
    );
    assert_eq!(refused[0]["document"], "mqtt.json");
    assert!(
        refused[0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("mqtt.json")),
        "the refusal must name the file: {:?}",
        refused[0]
    );
    // The device came up, is answering, and its Wi-Fi configuration was
    // adopted from the same namespace — the neighbour direction.
    assert_eq!(tree["wifi"]["ap"]["ssid"], POURED_SSID);

    let admin = http_client(true)?;
    let response = admin
        .post(format!("{https_base}/api/v1/setup"))
        .header(CONTENT_TYPE, "application/json")
        .body(r#"{"password":"pour-password","hostname":"pour-host"}"#)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let setup = response_json(response).await?;
    let csrf = setup["csrfToken"].as_str().context("csrf")?.to_string();

    // A task id and a snapshot id, so the two parameterised GET paths are
    // probed against something that exists rather than against a 404.
    let response = admin
        .put(format!("{https_base}/api/v1/settings/hostname"))
        .header("x-csrf-token", &csrf)
        .header(CONTENT_TYPE, "application/json")
        .body(r#""pour-host2""#)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id = response_json(response).await?["taskId"]
        .as_str()
        .context("taskId")?
        .to_string();
    wait_for_task(&admin, &https_base, &task_id).await?;

    let response = admin
        .post(format!("{https_base}/api/v1/diagnostics/snapshots"))
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "the diagnostic snapshot has to be collected, or clause 3 never reaches it"
    );
    let collected = response_json(response).await?;
    let snapshot_id = collected["snapshot"]["id"]
        .as_u64()
        .context("snapshot id")?
        .to_string();

    // **Clause 3.** Every published GET path, probed and searched.
    let mut answered = std::collections::BTreeMap::new();
    for path in published_get_paths()? {
        let urls = probe_urls(&path, &task_id, &snapshot_id);
        anyhow::ensure!(
            !urls.is_empty(),
            "`{path}` is a published GET route with a parameter this test cannot expand; \
             add it to `probe_urls` rather than leaving it unprobed"
        );
        for url in urls {
            let response = admin.get(format!("{https_base}{url}")).send().await?;
            let status = response.status();
            let body = response.text().await?;
            for secret in [POURED_PSK, REFUSED_SECRET] {
                assert!(
                    !body.contains(secret),
                    "GET {url} ({status}) served a poured secret:\n{body}"
                );
            }
            answered.insert(url, (status, body));
        }
    }

    // The three surfaces the gate names by name, each reached and each
    // answering — so the search above ran over a populated space rather than
    // over a wall of 404s.
    let (status, body) = &answered["/api/v1/settings/wifi"];
    assert_eq!(*status, StatusCode::OK);
    assert!(
        body.contains(POURED_SSID) && body.contains("<redacted>"),
        "the settings route must render the poured subtree with the key removed: {body}"
    );
    let (status, body) = &answered["/api/v1/settings/wifi.ap.psk"];
    assert_eq!(*status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body)?,
        serde_json::json!("<redacted>"),
        "a dot-path naming the secret directly answers the sentinel, not the value"
    );
    let (status, body) = &answered["/api/v1/wifi/client/networks"];
    assert_eq!(*status, StatusCode::OK, "{body}");
    assert!(
        body.contains("site-uplink") && body.contains("<redacted>"),
        "the wifi listing must render the poured network with the key removed: {body}"
    );
    let (status, body) = &answered[&format!("/api/v1/diagnostics/snapshots/{snapshot_id}")];
    assert_eq!(*status, StatusCode::OK, "{body}");
    assert!(
        body.contains("collectedAt"),
        "the snapshot has to be a snapshot: {body}"
    );
    // And the refusal really did travel to a served route, so the second
    // sentinel was searched for somewhere it could have been.
    let (status, body) = &answered["/api/v1/state/configuration"];
    assert_eq!(*status, StatusCode::OK, "{body}");
    assert!(
        body.contains("mqtt.json") && body.contains("refused"),
        "the served live state must carry the refusal: {body}"
    );

    Ok(())
}

/// A console tree shaped like the `mica-apid-ui` package's, under `dir`: the
/// entry document with the console's title and a hashed module script.
pub(super) fn console(dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let root = dir.join("ui");
    std::fs::create_dir_all(root.join("assets"))?;
    std::fs::write(
        root.join("index.html"),
        "<!doctype html><title>mica console</title>\n<script type=\"module\" crossorigin src=\"/_ui/assets/index-a1b2c3.js\"></script>",
    )?;
    std::fs::write(root.join("assets/index-a1b2c3.js"), "export {}")?;
    Ok(root)
}
