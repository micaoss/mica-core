//! micad — management-plane daemon.
//!
//! Owns the settings tree (persisted via `micad-settings`) and a live-state
//! tree, exposed on D-Bus as `com.mica.micad` / `/com/mica/micad` /
//! `com.mica.micad1`. Configuration comes from the environment:
//!
//! `--version` (or `-V`) is answered before any of the above is read: it prints
//! `micad <package version>` and exits 0 without provisioning,
//! connecting to a bus or writing a file (see [`main`]). Anything else on the
//! command line is ignored.

#![forbid(unsafe_code)]

mod apply_queue;
mod bluetooth;
mod bus;
mod containers;
mod deployment;
mod diagnostics;
mod fswrite;
mod identity;
mod network_state;
mod power;
mod provisioning;
mod provisioning_doc;
mod reconciler;
mod recovery;
mod reset;
mod scan;
mod storage_status;
mod system_info;
mod telemetry;
mod time_status;
mod transient;
mod update_auto;
mod update_codes;
mod update_lifecycle;
mod update_policy;
mod wgkeys;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use micad_settings::Store;
use serde_json::Value;
use tokio::signal::unix::{SignalKind, signal};

/// The daemon entry point: the `micad` binary runs it when invoked as `micad`.
pub fn main() -> anyhow::Result<()> {
    // `--version` is answered and returned from here, above every line that
    // makes this process a daemon -- before the tokio runtime, the subscriber,
    // any environment read and the settings store. The position is the
    // requirement: first-boot provisioning writes /var/lib/mica/settings.toml
    // and /var/lib/mica/secrets/ before the daemon reaches the bus, so a handler
    // below it would answer a question and MUTATE the machine that asked.
    if wants_version(std::env::args().skip(1)) {
        println!("{}", version_line());
        return Ok(());
    }
    serve()
}

/// What the package version is reported as when the build supplied none: an
/// unpackaged build (`cargo run`, the Rust gate).
const UNKNOWN_VERSION: &str = "unknown";

/// Whether an argv (argv[1..]) is asking for the version.
///
/// `-V` as well as `--version`, because clap gives `mica-mqttd` and
/// `mica-mqtt-broker` the pair and the four binaries spell the one question the
/// same way. A pure function over an iterator rather than a read of
/// `std::env::args`, so the tests below can drive it without spawning.
fn wants_version(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter()
        .any(|arg| arg == "--version" || arg == "-V")
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

/// The one line `--version` prints: `micad <package version>`.
///
/// The version is the producer's declared `VERSION` (`pkgs/<producer>/producer.env`),
/// compiled in as `MICA_PACKAGE_VERSION` by `scripts/build/build-deb.sh`: the
/// version of the package this binary ships in, and nothing about the commit.
fn version_line() -> String {
    format!(
        "{} {}",
        "micad",
        version_or_unknown(option_env!("MICA_PACKAGE_VERSION")),
    )
}

#[tokio::main]
async fn serve() -> anyhow::Result<()> {
    // INFO by default, not ERROR.
    //
    // `tracing_subscriber::fmt::init` falls back to ERROR when RUST_LOG is
    // unset, which on a device means micad records what FAILED and never what
    // it did. RUST_LOG still wins, so a debug session is one variable away.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings_path = std::env::var("MICAD_SETTINGS_PATH")
        .unwrap_or_else(|_| micad_settings::DEFAULT_PATH.to_string());
    // The `/mica/config/` namespace. Relocatable for the same reason the
    // settings path is — the bus tests run a real daemon against a temporary
    // tree — and by a variable of its own rather than derived from
    // `MICA_DATA_ROOT`, because what micad reads is the `/mica` BIND and what
    // `mica-data-layout` and `reset.rs` write is the pool underneath it.
    let config_dir = std::env::var("MICAD_CONFIG_DIR")
        .unwrap_or_else(|_| micad_settings::DEFAULT_CONFIG_DIR.to_string());
    let bus_kind = std::env::var("MICAD_BUS").unwrap_or_else(|_| "system".to_string());
    let dry_run = std::env::var("MICAD_DRY_RUN").is_ok_and(|value| value == "1");

    let store = Store::new(&settings_path, &config_dir);
    // **Fail closed on the medium**. System configuration
    // lives on DATA now, so a DATA pool that does not mount is a device with
    // no configuration — and a device that cannot read its configuration must
    // not render a different one. Without this it would come up on schema
    // defaults, DHCP on every interface and sshd off, and look fine to
    // everything except the operator who configured a static address.
    // `RequiresMountsFor=/mica` on the unit is the first half; this is the half
    // that names the mount in the journal, and the error carries that name.
    // The recovery route is the serial console and the reset tiers, not a
    // silently degraded network.
    let micad_settings::LoadedStore {
        mut settings,
        refusals,
    } = store
        .load_with_refusals()
        .with_context(|| format!("load settings from {settings_path} and {config_dir}"))?;
    // Loud, and at ERROR: a poured document that did not load is a device
    // running with a capability deliberately switched off, and the operator
    // who poured it has no other way to find out which file it was. The
    // message names the file, which is what the pour gate requires of it.
    for refusal in &refusals {
        // `detail` is the parser's own sentence and it is logged HERE and
        // nowhere else: it quotes what it choked on, so a poured document's
        // site key can be in it, and the journal is on the device while
        // `configuration.refused` is served over the API.
        tracing::error!(
            document = refusal.document,
            subtrees = ?refusal.subtrees,
            detail = refusal.detail,
            "{}",
            refusal.message
        );
    }
    // **Narrowed here and once**, before anything on the boot path can save.
    // `provisioning::ensure_provisioned` writes the store on the very boot that
    // finds a pour — it mints the device identity — and `save` writes every
    // document out of an addressed tree in which a refused subtree sits at its
    // schema default. Without this the first boot after a bad pour would
    // overwrite the integrator's file with the default nobody chose, and the
    // second boot would come up clean on it (`Store::preserving`).
    let store = store.preserving(
        &refusals
            .iter()
            .map(|refusal| refusal.document.as_str())
            .collect::<Vec<_>>(),
    );
    // Before the reconcilers exist, so the very first reconcile already sees a
    // seeded tree rather than the built-in defaults. Skipped under dry-run,
    // which must not write to STATE at all.
    if dry_run {
        tracing::info!("dry run: first-boot provisioning skipped");
    } else {
        let state_dir = state_dir_for(&settings_path);
        // BEFORE both of the steps below, and before every reconciler: a
        // staged reset is what the operator asked this boot to do, so the rest
        // of the boot has to see the device the reset produced rather than
        // reconcile settings that are about to be taken away. Tiers 1 and 3
        // hand the seeded values back to `ensure_provisioned` below by putting
        // `provisioning.state` to `Pending`, which is why this call comes
        // first and not merely early.
        match recovery::apply_boot_intent(&store, &mut settings, &recovery::Paths::from_env()) {
            Ok(outcome) => tracing::info!(?outcome, "boot recovery intent checked"),
            Err(err) => tracing::error!(
                error = %err,
                "a board-declared recovery action mapped and could not be carried out; \
                 the device boots normally and nothing is asserted"
            ),
        }
        match reset::apply_pending(&store, &mut settings, &reset::Roots::from_env()) {
            Ok(outcome) => tracing::info!(?outcome, "staged reset checked"),
            Err(error) => {
                // Keep application writers stopped until the staged scope is
                // complete; shared DATA failure needs recovery, not a new root.
                std::fs::write(
                    "/run/mica/shared-data-failure",
                    format!("staged reset failed: {error:#}"),
                )?;
                return Err(error).context("staged reset failed; DATA recovery required");
            }
        }
        // BEFORE seeding, and that order is the point: a factory-injected
        // `provisioning.deviceId` has to be in the tree when `ensure_identity`
        // decides whether to mint one, and when the hostname is derived from
        // it. Applied the other way round the device would mint an identity,
        // name itself after it, and only then be handed the identity the
        // factory recorded.
        match provisioning_doc::import(
            &store,
            &mut settings,
            &provisioning_doc::staging_root_from_env(),
        ) {
            Ok(outcome) => tracing::info!(?outcome, "provisioning document checked"),
            Err(err) => tracing::error!(
                error = %err,
                "the provisioning document import could not be recorded; the device is \
                 unchanged and the next boot retries"
            ),
        }
        // Hard failure on purpose: an unwritable STATE means no device identity
        // and no device credential, so there is no usable device to serve. A
        // loud exit is better than a daemon that quietly serves an
        // unprovisioned tree the operator cannot log in to.
        let outcome = provisioning::ensure_provisioned(&store, &state_dir, &mut settings)
            .context("first-boot provisioning")?;
        tracing::info!(?outcome, state_dir = %state_dir.display(), "provisioning checked");
    }

    // What the product carries decides what runs. A dry-run daemon and a host
    // with no product file serve everything.
    let features = if dry_run {
        micad_settings::Features::all()
    } else {
        product_features(std::path::Path::new(micad_settings::PRODUCT_FILE))
    };
    tracing::info!(features = ?features.words(), "product features");
    let reconcilers = if dry_run {
        Vec::new()
    } else {
        reconciler::for_features(&features)
    };
    // Under dry-run the production control is never constructed, so a daemon
    // started by a test cannot reach systemd's manager at all.
    let power: Box<dyn power::PowerControl> = if dry_run {
        Box::new(power::DryRunPower)
    } else {
        Box::new(power::Systemd::new())
    };
    // The service registry exists only when a scan does, so that a daemon
    // running no scan answers `ForgetService` with "there is no registry"
    // rather than with an empty one it would never fill.
    let scan_enabled = service_scan_enabled(dry_run, std::env::var("MICAD_SCAN").ok().as_deref());
    let registry = scan_enabled.then(|| Arc::new(scan::Registry::new()));
    tracing::info!(
        settings_path,
        config_dir,
        dry_run,
        reconcilers = reconcilers.len(),
        service_scan = scan_enabled,
        "micad starting"
    );

    let mut state = serde_json::Map::new();
    if dry_run {
        state.insert("dry_run".to_string(), Value::Bool(true));
    }
    // Layer 1, read here and only here: the manifest is inside
    // the read-only dm-verity root, so its value cannot change while this
    // process runs and a re-read per decision would answer the same thing.
    // Read under dry-run too -- it is a read of one file in /usr/share and
    // touches nothing -- so a test daemon reports the same shape a device
    // does, with the error saying the host has no baked manifest.
    let meta_path = std::env::var("MICAD_META_MANIFEST_PATH").map_or_else(
        |_| PathBuf::from(micad_settings::configuration::DEFAULT_MANIFEST_PATH),
        PathBuf::from,
    );
    let meta = micad_settings::configuration::load_manifest(&meta_path);
    if let Some(error) = &meta.error {
        // Not fatal: the reader always answers with a document, and every
        // action the missing values gate refuses on its own terms. A build
        // refuses a manifest this reader would reject, so reaching this on a
        // device means the image is not the one the build produced.
        tracing::warn!(error, "baked update configuration unavailable");
    }
    state.insert("meta".to_string(), meta.to_json());

    // Read before the tree is handed to the service: the agent answers a
    // legacy peer with the declared code, or with the one derived from this
    // device's own identity.
    let settings_pin = settings.bluetooth.pin.clone();
    let device_id_for_pin = settings.provisioning.device_id.clone().unwrap_or_default();
    let mut pairing_agent: Option<Arc<bluetooth::Agent>> = None;

    let mut service = bus::MicadService::new(
        store,
        settings,
        reconcilers,
        power,
        transient::production_shadow_path(),
        Value::Object(state),
    )
    .with_features(features.clone());
    if !dry_run {
        service =
            service.with_network_state(Arc::new(network_state::SystemdNetworkState::production()));
        service = service.with_time_status(Arc::new(time_status::SystemdTimesync));
        service = service.with_storage_status(Arc::new(storage_status::HostStorage::production()));
        // The diagnostic observers, on the same rule: each reads the host
        // (sysfs, /etc, the journal), so a dry-run daemon is never given one.
        service = service.with_system_info(Arc::new(system_info::HostSystemInfo::production()));
        service = service.with_telemetry(Arc::new(telemetry::SysfsTelemetry::production()));
        service =
            service.with_failure_evidence(Arc::new(diagnostics::HostFailureEvidence::production()));
        // The engine is read-only and the unit control is systemd: a declared
        // container's lifecycle is a unit's, and a dry-run daemon gets
        // neither, so it neither runs podman nor starts anything.
        // The adapter and the pairing agent. The agent's state is shared with
        // the `org.bluez.Agent1` object registered below: it blocks on a
        // decision the console makes through the bus.
        if features.has(micad_settings::Feature::Bluetooth) {
            let agent = Arc::new(bluetooth::Agent::default());
            pairing_agent = Some(Arc::clone(&agent));
            service = service.with_bluetooth(Arc::new(bluetooth::BlueZ), Arc::clone(&agent));
        }
        service = service.with_containers(
            Arc::new(containers::Podman::default()),
            Arc::new(reconciler::systemd::Systemd::new()),
        );
        // Same reasoning again: the rotation writes a private key onto STATE
        // and deletes a kernel device, so a dry-run daemon is never given one.
        service = service.with_wireguard(Arc::new(reconciler::network::KeyRotation::production()));
        let policy_path = std::env::var("MICAD_UPDATE_POLICY_PATH").map_or_else(
            |_| PathBuf::from(update_policy::DEFAULT_POLICY_PATH),
            PathBuf::from,
        );
        service = service.with_update(
            Arc::new(update_lifecycle::SubprocessClient::new(PathBuf::from(
                update_lifecycle::DEFAULT_CLIENT_PATH,
            ))),
            update_policy::PolicyStore::at(policy_path).with_baked(meta.manifest.update.clone()),
        );
    }
    if let Some(registry) = &registry {
        service = service.with_service_registry(Arc::clone(registry));
    }
    // Taken before the service moves onto the bus: the automatic driver
    // shares the lifecycle with the manual check and fetch routes, so both
    // read one policy and record into one state entry.
    let update_handle = service.update_handle();
    // BEFORE the first reconcile, which is the point: a refused document must
    // not be applied from its schema default even once, so the refusals have
    // to be in place before `apply_all` visits the reconcilers they gate.
    service.set_config_refusals(refusals).await;
    service.apply_all().await;
    let builder = match bus_kind.as_str() {
        "system" => zbus::connection::Builder::system()?,
        "session" => zbus::connection::Builder::session()?,
        other => anyhow::bail!("MICAD_BUS must be `system` or `session`, got `{other}`"),
    };
    let connection = builder
        .serve_at(bus::OBJECT_PATH, service)?
        .build()
        .await
        .with_context(|| format!("connect to {bus_kind} bus"))?;
    // The pairing agent, on the same connection. Served before it is
    // registered, so BlueZ never calls an object that is not there yet, and
    // registered on a best-effort basis: a board with no radio has no
    // `org.bluez` to register with, and that is not a reason for micad not to
    // start.
    if !dry_run && let Some(agent) = pairing_agent.take() {
        let state = Arc::clone(&agent);
        if let Err(err) = connection
            .object_server()
            .at(bluetooth::AGENT_PATH, bluetooth::PairingAgent { state })
            .await
        {
            tracing::warn!(error = %err, "serving the Bluetooth pairing agent failed");
        } else if let Err(err) = bluetooth::register_agent(&connection).await {
            tracing::info!(error = %err, "no Bluetooth agent registered: bluetoothd did not answer");
        } else {
            tracing::info!(
                path = bluetooth::AGENT_PATH,
                "Bluetooth pairing agent registered"
            );
        }
        // The agent answers with whatever the settings say; the reconciler
        // keeps it in step from here on.
        agent
            .set_pin(
                settings_pin
                    .clone()
                    .unwrap_or_else(|| micad_settings::derived_pairing_pin(&device_id_for_pin)),
            )
            .await;
    }
    // The service scan, started before the well-known name is claimed so that
    // its NameOwnerChanged subscription is in place before anything can react
    // to micad appearing — a service that claims its name in that window is
    // seen by the signal rather than missed between the sweep and the
    // subscription.
    if let Some(registry) = registry {
        let service_ref = connection
            .object_server()
            .interface::<_, bus::MicadService>(bus::OBJECT_PATH)
            .await
            .context("look up served MicadService")?;
        tokio::spawn(scan::run(connection.clone(), registry, service_ref));
    }
    // The automatic update driver: the check cadence under `policy = check`,
    // and under `auto` the whole check/fetch/re-check/install pass with its
    // reboot. Started here rather than beside the lifecycle because the
    // install and reboot routes it calls live on the service the object
    // server now owns — the same reason the scan is started here. Never
    // under dry-run, whose lifecycle has no client to call anyway.
    if !dry_run {
        let service_ref = connection
            .object_server()
            .interface::<_, bus::MicadService>(bus::OBJECT_PATH)
            .await
            .context("look up served MicadService")?;
        tokio::spawn(update_auto::run(Arc::new(bus::BusRoutes::new(
            update_handle,
            service_ref,
        ))));
    }
    connection
        .request_name(bus::BUS_NAME)
        .await
        .with_context(|| format!("request name {}", bus::BUS_NAME))?;
    tracing::info!(bus = bus_kind, name = bus::BUS_NAME, "serving");

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM received, exiting"),
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT received, exiting"),
    }
    Ok(())
}

/// Directory holding STATE-backed data for a settings file at `settings_path`.
///
/// The secrets live beside the settings file, so tests that redirect
/// `MICAD_SETTINGS_PATH` into a temporary directory redirect the secrets with
/// it and never touch the host's `/var/lib/mica`.
fn state_dir_for(settings_path: &str) -> PathBuf {
    Path::new(settings_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(
            || PathBuf::from(identity::DEFAULT_STATE_DIR),
            Path::to_path_buf,
        )
}

/// Whether the service scan ([`scan`]) is constructed, from `dry_run` and the
/// raw value of `MICAD_SCAN` (`None` when it is unset).
///
/// One-way by construction. Production is unconditionally on; `MICAD_SCAN` is
/// read only to lift dry-run's suppression, so no value of it can take the
/// service registry away from a device that would otherwise have one.
fn service_scan_enabled(dry_run: bool, scan_override: Option<&str>) -> bool {
    !dry_run || scan_override == Some("1")
}

/// The features of the product file at `path`. A file that exists and cannot be
/// read is reported and treated as every feature: a daemon that cannot manage
/// the device at all is worse than one that serves more than the product
/// carries.
fn product_features(path: &std::path::Path) -> micad_settings::Features {
    micad_settings::Features::load(path).unwrap_or_else(|err| {
        tracing::error!(path = %path.display(), error = %err, "product features unreadable; serving every feature");
        micad_settings::Features::all()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        UNKNOWN_VERSION, service_scan_enabled, version_line, version_or_unknown, wants_version,
    };

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    /// The two spellings, and the positive control for the negatives below.
    #[test]
    fn both_spellings_of_the_one_flag_are_recognised() {
        assert!(wants_version(argv(&["--version"])));
        assert!(wants_version(argv(&["-V"])));
    }

    /// Driven from the failing side. Every one of these must fall through into
    /// the daemon, and `-v` is the one that matters most: it is the spelling an
    /// operator reaches for, it is NOT this flag, and a loose match on it would
    /// silently stop micad from starting on any unit that passed it.
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

    /// It is asked of the whole argv, not only of the first element -- the same
    /// place clap answers it for `mica-mqttd` and `mica-mqtt-broker`.
    #[test]
    fn the_flag_is_found_wherever_it_appears() {
        assert!(wants_version(argv(&[
            "--config",
            "/etc/x.toml",
            "--version"
        ])));
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
            .strip_prefix("micad ")
            .unwrap_or_else(|| panic!("got {line:?}"));
        assert!(!version.is_empty(), "an empty version in {line:?}");
        assert!(
            !version.contains(char::is_whitespace),
            "the version must be one token, got {version:?}"
        );
    }

    /// The pin on the asymmetry: in production `MICAD_SCAN` is inert. A gate
    /// that read the variable symmetrically would let `MICAD_SCAN=0` — or any
    /// typo — switch the service registry off on a real device.
    #[test]
    fn production_ignores_micad_scan_entirely() {
        for value in [
            None,
            Some("1"),
            Some("0"),
            Some(""),
            Some("true"),
            Some("no"),
        ] {
            assert!(
                service_scan_enabled(false, value),
                "production must scan whatever MICAD_SCAN holds; \
                 got service_scan=false for MICAD_SCAN={value:?}"
            );
        }
    }

    /// Dry-run on its own constructs no scan, as
    /// `tests/scan.rs::dry_run_constructs_no_scan` pins it end to end.
    #[test]
    fn dry_run_alone_constructs_no_scan() {
        for value in [None, Some("0"), Some(""), Some("true"), Some("yes")] {
            assert!(
                !service_scan_enabled(true, value),
                "dry-run must construct no scan unless MICAD_SCAN is exactly `1`; \
                 got service_scan=true for MICAD_SCAN={value:?}"
            );
        }
    }

    /// The one combination that turns the scan back on.
    #[test]
    fn dry_run_plus_micad_scan_one_constructs_the_scan() {
        assert!(service_scan_enabled(true, Some("1")));
    }
}
