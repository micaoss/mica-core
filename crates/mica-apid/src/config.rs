//! Environment-driven daemon configuration.

use std::path::{Path, PathBuf};

use micad_settings::{APID_LISTENERS_PATH, WebSettings};

/// Where apid keeps its TLS identity, session key, login backoff and audit ring.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/mica/apid";

/// Which message bus to reach `micad` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    /// The system bus (production default).
    System,
    /// The session bus (tests and development).
    Session,
}

/// Runtime configuration, read once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where apid listens.
    pub listeners: Listeners,
    /// Directory holding the certificate and key material (`APID_STATE_DIR`).
    pub state_dir: PathBuf,
    /// Bus to reach `micad` on (`APID_BUS`).
    pub bus: BusKind,
    /// Where the built-in console is installed (`APID_UI_DIR`); absent is an
    /// API-only device.
    pub ui_dir: PathBuf,
}

impl Config {
    /// Read configuration from the environment, applying defaults.
    ///
    /// # Errors
    ///
    /// Fails when `APID_BUS` is set to anything but `system` or `session`.
    pub fn from_env() -> anyhow::Result<Self> {
        let listeners = Listeners::resolve();
        let state_dir = PathBuf::from(
            std::env::var("APID_STATE_DIR").unwrap_or_else(|_| DEFAULT_STATE_DIR.to_string()),
        );
        let bus = match std::env::var("APID_BUS").as_deref() {
            Err(_) | Ok("system") => BusKind::System,
            Ok("session") => BusKind::Session,
            Ok(other) => anyhow::bail!("APID_BUS must be `system` or `session`, got `{other}`"),
        };
        let ui_dir = PathBuf::from(
            std::env::var("APID_UI_DIR")
                .unwrap_or_else(|_| crate::assets::builtin::DEFAULT_DIR.to_string()),
        );
        Ok(Self {
            listeners,
            state_dir,
            bus,
            ui_dir,
        })
    }
}

/// Where apid listens, resolved once for the daemon and for `--healthcheck`,
/// so the probe asks the listener the daemon bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listeners {
    /// The HTTP listener: the console, or only a redirect to HTTPS when
    /// `https` is set.
    pub http_addr: String,
    /// The HTTPS listener, when HTTPS is enabled.
    pub https_addr: Option<String>,
}

impl Listeners {
    /// `access.web` as micad rendered it (`APID_LISTENERS_FILE`, default
    /// [`APID_LISTENERS_PATH`]), then the address overrides the tests use:
    /// `APID_HTTP_ADDR`, and `APID_HTTPS_ADDR`, which also enables HTTPS.
    pub fn resolve() -> Self {
        let file = std::env::var_os("APID_LISTENERS_FILE")
            .map_or_else(|| PathBuf::from(APID_LISTENERS_PATH), PathBuf::from);
        let mut listeners = Self::of(&read_web(&file));
        if let Ok(addr) = std::env::var("APID_HTTP_ADDR") {
            listeners.http_addr = addr;
        }
        if let Ok(addr) = std::env::var("APID_HTTPS_ADDR") {
            listeners.https_addr = Some(addr);
        }
        listeners
    }

    /// The listeners `web` describes, on every address.
    pub fn of(web: &WebSettings) -> Self {
        Self {
            http_addr: format!("0.0.0.0:{}", web.http_port),
            https_addr: web
                .https_enabled
                .then(|| format!("0.0.0.0:{}", web.https_port)),
        }
    }
}

/// The rendered `access.web`, or its defaults when there is none. A file that
/// does not parse is reported and read as the defaults rather than stopping
/// apid: the console is how an operator repairs the setting that broke it.
fn read_web(path: &Path) -> WebSettings {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), error = %err, "listeners file does not parse; using the defaults");
            WebSettings::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => WebSettings::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "listeners file unreadable; using the defaults");
            WebSettings::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_http_on_8080_and_no_https() {
        let dir = tempfile::tempdir().unwrap();
        let missing = read_web(&dir.path().join("apid.json"));
        assert_eq!(
            Listeners::of(&missing),
            Listeners {
                http_addr: "0.0.0.0:8080".to_string(),
                https_addr: None,
            }
        );
    }

    #[test]
    fn the_rendered_file_moves_the_listeners_and_a_broken_one_is_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apid.json");
        std::fs::write(
            &path,
            r#"{ "httpPort": 8081, "httpsEnabled": true, "httpsPort": 9443 }"#,
        )
        .unwrap();
        assert_eq!(
            Listeners::of(&read_web(&path)),
            Listeners {
                http_addr: "0.0.0.0:8081".to_string(),
                https_addr: Some("0.0.0.0:9443".to_string()),
            }
        );
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_web(&path), WebSettings::default());
    }
}
