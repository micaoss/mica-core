//! apid — appliance API daemon; the web dashboard is what it serves.
//!
//! Serves the management web interface over HTTPS with a self-signed
//! certificate and talks to `micad` exclusively over D-Bus
//! (`com.mica.micad` / `/com/mica/micad` / `com.mica.micad1`).
//!
//! Configuration is taken from the environment.

#![forbid(unsafe_code)]

mod access_cache;
mod assets;
mod audit;
mod auth;
mod bundle;
mod bus_client;
mod config;
mod diagnostics;
mod healthcheck;
mod openapi;
mod persist;
mod provisioning_api;
mod redact;
mod routes;
mod session;
mod settings_api;
mod startup;
mod task_registry;
#[cfg(test)]
mod tests;
mod tls;
mod token;
mod update_api;

use std::sync::Arc;

use anyhow::Context;
use axum_server::tls_rustls::RustlsConfig;
use tokio::signal::unix::{SignalKind, signal};

use crate::settings_api::SettingsApi;

/// The daemon entry point of the `mica-apid` executable.
pub fn main() -> anyhow::Result<()> {
    // `--version` is answered and returned from here, above every line that
    // makes this process a daemon. The position is the requirement: a handler
    // below initialisation would generate a self-signed certificate and a
    // session signing key into the state dir, bind 0.0.0.0:443 and 0.0.0.0:80,
    // print APID_LISTENING and never exit, so a smoke run that asked for the
    // version would hang rather than go red. Hence a synchronous `main` with
    // the async body in `serve`: the answer is given before the tokio runtime
    // is built, before the ring crypto provider is installed, before
    // `Config::from_env`, before the state dir exists and before any key is
    // generated. There is nothing above it. `micad/micad/src/main.rs` carries
    // the same handler and the long form of the shared reasoning.
    if wants_version(std::env::args().skip(1)) {
        println!("{}", version_line());
        return Ok(());
    }
    // `--openapi`, answered from the same place and for the same reason: it
    // is the committed `apid/openapi.json` regenerated, and a handler below
    // initialisation would mint key material and bind ports to print a
    // document that describes neither.
    if wants_openapi(std::env::args().skip(1)) {
        print!("{}", openapi::document_json());
        return Ok(());
    }
    // `--healthcheck`, from the same place: the probe asks the running apid,
    // and a handler below initialisation would bind the ports it is probing.
    if std::env::args().skip(1).any(|arg| arg == "--healthcheck") {
        return healthcheck::probe();
    }
    serve()
}

/// What the package version is reported as when the build supplied none: an
/// unpackaged build (`cargo run`, the Rust gate).
const UNKNOWN_VERSION: &str = "unknown";

/// Whether an argv (argv[1..]) is asking for the version.
///
/// `-V` as well as `--version`, so the four binaries this repository ships
/// answer one question one way: `mica-mqttd` and `mica-mqtt-broker` get the pair
/// from clap's `#[command(version)]`. It is the same flag, not a second one.
fn wants_version(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter()
        .any(|arg| arg == "--version" || arg == "-V")
}

/// Whether an argv (argv[1..]) is asking for the OpenAPI document.
///
/// The long spelling only: there is no short flag to collide with, and the
/// output is a file's worth of JSON that nobody types by accident.
fn wants_openapi(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|arg| arg == "--openapi")
}

/// The package version, or [`UNKNOWN_VERSION`], from whatever the build embedded.
///
/// Takes the embedded value as an argument so the absent case is reachable from
/// a test in a binary that was built with a version.
fn version_or_unknown(embedded: Option<&'static str>) -> &'static str {
    match embedded {
        Some(version) if !version.trim().is_empty() => version,
        _ => UNKNOWN_VERSION,
    }
}

/// The one line `--version` prints: `mica-apid <package version>`.
///
/// The version is the producer's declared `VERSION` (`pkgs/<producer>/producer.env`),
/// compiled in as `MICA_PACKAGE_VERSION` by `scripts/build/build-deb.sh`: the
/// version of the package this binary ships in, and nothing about the commit.
fn version_line() -> String {
    format!(
        "{} {}",
        "mica-apid",
        version_or_unknown(option_env!("MICA_PACKAGE_VERSION")),
    )
}

#[tokio::main]
async fn serve() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("install ring crypto provider"))?;

    let config = config::Config::from_env()?;
    tls::ensure_state_dir(&config.state_dir)
        .with_context(|| format!("create state dir {}", config.state_dir.display()))?;
    let certificate = tls::load_or_generate_certificate(&config.state_dir)?;
    let signing_key = tls::load_or_generate_session_key(&config.state_dir)?;

    let api: Arc<dyn SettingsApi> = Arc::new(bus_client::BusSettings::new(config.bus));
    // The state dir already exists (ensure_state_dir above) and already holds
    // the TLS material, so the backoff counter and the audit ring go there
    // too: one STATE-backed directory, one set of permissions to reason about.
    let state = routes::AppState::new(api, signing_key).with_persistence(&config.state_dir);
    // The SettingsChanged watcher that keeps the gate's access cache honest;
    // until it reports a live subscription the gate reads the bus directly,
    // so a micad that is not up yet costs latency, never staleness.
    tokio::spawn(bus_client::watch_settings_changed(
        config.bus,
        state.access_cache().clone(),
    ));
    tokio::spawn(bus_client::watch_tasks(
        config.bus,
        state.task_registry().clone(),
        state.audit().clone(),
    ));

    let https_listener = std::net::TcpListener::bind(&config.https_addr)
        .with_context(|| format!("bind https listener on {}", config.https_addr))?;
    https_listener.set_nonblocking(true)?;
    let https_addr = https_listener.local_addr()?;
    let http_listener = tokio::net::TcpListener::bind(&config.http_addr)
        .await
        .with_context(|| format!("bind http listener on {}", config.http_addr))?;
    let http_addr = http_listener.local_addr()?;

    // The one machine-readable startup marker; everything else goes to stderr.
    println!("APID_LISTENING https={https_addr} http={http_addr}");
    tracing::info!(%https_addr, %http_addr, "apid serving");

    // The ordering is the requirement rather than a detail: bundle
    // discovery and the compatibility re-check happen **after** the listeners
    // bind and after `APID_LISTENING` is printed, and the outcome is a state
    // this function holds rather than an error it returns. Note the absence of
    // `?`: every step above this line propagates, and under
    // `Restart=on-failure` (`dist/apid.service`) a propagated error is
    // a crash loop with no listener bound. A bundle must not be able to stop
    // apid from listening, so `discover` has no error variant to propagate.
    let bundle_state = startup::discover(state.bundles().clone(), state.audit().clone()).await;
    tracing::info!(bundle = %bundle_state, "custom UI state at start-up");

    let rustls_config = RustlsConfig::from_pem(
        certificate.cert_pem.into_bytes(),
        certificate.key_pem.into_bytes(),
    )
    .await
    .context("build rustls server config")?;
    // `with_connect_info` installs the peer address the audit trail reads
    // (`audit::Source`); without it every audit line would say `unknown`.
    let https = axum_server::from_tcp_rustls(https_listener, rustls_config)
        .serve(routes::app(state).into_make_service_with_connect_info::<std::net::SocketAddr>());
    let http = axum::serve(
        http_listener,
        routes::redirect_app(https_addr.port()).into_make_service(),
    );
    tokio::spawn(async move {
        if let Err(err) = https.await {
            tracing::error!(error = %err, "https server failed");
        }
    });
    tokio::spawn(async move {
        if let Err(err) = http.await {
            tracing::error!(error = %err, "http redirect server failed");
        }
    });

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM received, exiting"),
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT received, exiting"),
    }
    Ok(())
}

/// The `--version` handler, driven from the failing side.
///
/// A module of its own because `mod tests` is already `src/tests.rs`, the
/// HTTP suite.
#[cfg(test)]
mod version_tests {
    use super::{UNKNOWN_VERSION, version_line, version_or_unknown, wants_openapi, wants_version};

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    /// The two spellings, and the positive control for the negatives below.
    #[test]
    fn both_spellings_of_the_one_flag_are_recognised() {
        assert!(wants_version(argv(&["--version"])));
        assert!(wants_version(argv(&["-V"])));
    }

    /// Every one of these must fall through into the daemon. `-v` matters
    /// most: it is not this flag, and a loose match on it would stop apid from
    /// ever binding a listener on a unit that passed it.
    #[test]
    fn nothing_else_is_this_flag() {
        for args in [
            vec![],
            argv(&["--help"]),
            argv(&["-h"]),
            argv(&["-v"]),
            argv(&["-VV"]),
            argv(&["--versions"]),
            argv(&["--version=1"]),
            argv(&["version"]),
            argv(&["--Version"]),
            argv(&[""]),
        ] {
            assert!(
                !wants_version(args.clone()),
                "argv {args:?} is not --version and must reach the daemon unchanged"
            );
        }
    }

    /// It is asked of the whole argv, not only of the first element.
    #[test]
    fn the_flag_is_found_wherever_it_appears() {
        assert!(wants_version(argv(&["--state-dir", "/var/lib/mica", "-V"])));
    }

    /// The second flag, held to the same rule, and the two do not answer for
    /// each other: `--openapi` reaching the version handler would print a
    /// version line instead of the document a regeneration is asking for.
    #[test]
    fn the_openapi_flag_is_itself_and_nothing_else() {
        assert!(wants_openapi(argv(&["--openapi"])));
        assert!(wants_openapi(argv(&[
            "--state-dir",
            "/var/lib/mica",
            "--openapi"
        ])));
        assert!(!wants_version(argv(&["--openapi"])));
        assert!(!wants_openapi(argv(&["--version"])));
        for args in [
            vec![],
            argv(&["--open-api"]),
            argv(&["--openapi=1"]),
            argv(&["openapi"]),
            argv(&["-o"]),
        ] {
            assert!(
                !wants_openapi(args.clone()),
                "argv {args:?} is not --openapi and must reach the daemon unchanged"
            );
        }
    }

    /// Absent is `unknown`, never an error, and empty counts as absent.
    #[test]
    fn a_version_the_build_did_not_supply_reports_unknown() {
        assert_eq!(version_or_unknown(None), UNKNOWN_VERSION);
        assert_eq!(version_or_unknown(Some("")), UNKNOWN_VERSION);
        assert_eq!(version_or_unknown(Some("   ")), UNKNOWN_VERSION);
    }

    /// The positive control: a supplied value is passed through untouched.
    #[test]
    fn a_version_the_build_did_supply_is_reported_verbatim() {
        assert_eq!(version_or_unknown(Some("0.1.1-1")), "0.1.1-1");
    }

    /// The shape: the binary, one space, one version token, one line.
    #[test]
    fn the_version_line_names_the_binary_and_the_package_version() {
        let line = version_line();
        let version = line
            .strip_prefix("mica-apid ")
            .unwrap_or_else(|| panic!("got {line:?}"));
        assert!(!version.is_empty(), "an empty version in {line:?}");
        assert!(
            !version.contains(char::is_whitespace),
            "the version must be one token, got {version:?}"
        );
    }
}
