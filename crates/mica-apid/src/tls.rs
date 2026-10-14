//! State-directory management: the console's TLS identity and the session
//! cookie signing key.
//!
//! The identity is one PEM file, the certificate chain and its private key,
//! published whole in one rename. apid generates a self-signed one the first
//! time it serves HTTPS; an operator may replace it with one they upload or
//! with a self-signed one generated to their own names.

use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use anyhow::Context;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// The identity's file name in the state directory.
pub const IDENTITY_FILE: &str = "identity.pem";

/// Longest certificate chain or private key PEM an upload may carry.
pub const MAX_PEM_BYTES: usize = 64 * 1024;

/// PEM-encoded server certificate and private key.
pub struct Certificate {
    /// PEM identity containing the certificate chain.
    pub cert_pem: String,
    /// The same PEM identity containing the private key.
    pub key_pem: String,
}

/// Create `dir` with mode 0700 when missing.
pub fn ensure_state_dir(dir: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::File::open(dir)?.sync_all()?;
        if let Some(parent) = dir.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
    }
    Ok(())
}

/// Load the atomic `identity.pem` from `dir`, generating a self-signed pair on
/// first start: CN `mica`, SANs `DNS:mica`, `DNS:localhost`, `IP:127.0.0.1`,
/// with rcgen's default long validity. The complete pair is synced and published
/// with mode 0600 in one rename, so interruption cannot leave mismatched halves.
pub fn load_or_generate_certificate(dir: &Path) -> anyhow::Result<Certificate> {
    let identity_path = dir.join(IDENTITY_FILE);
    if identity_path.exists() {
        let identity = std::fs::read_to_string(&identity_path)
            .with_context(|| format!("read {}", identity_path.display()))?;
        return Ok(Certificate {
            cert_pem: identity.clone(),
            key_pem: identity,
        });
    }

    let identity = generate_identity(&CertificateRequest::default())?;
    crate::persist::write_atomically(&identity_path, &identity, 0o600)?;
    tracing::info!(identity = %identity_path.display(), "generated self-signed identity");
    Ok(Certificate {
        cert_pem: identity.clone(),
        key_pem: identity,
    })
}

/// What a self-signed identity is issued to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateRequest {
    /// The subject's common name.
    pub common_name: String,
    /// DNS subject alternative names.
    pub dns_names: Vec<String>,
    /// IP subject alternative names.
    pub ip_addresses: Vec<IpAddr>,
    /// Days from now it stays valid; `None` is rcgen's long default.
    pub validity_days: Option<u32>,
}

impl Default for CertificateRequest {
    /// The first-start identity: CN `mica`, SANs `DNS:mica`, `DNS:localhost`,
    /// `IP:127.0.0.1`.
    fn default() -> Self {
        Self {
            common_name: "mica".to_string(),
            dns_names: vec!["mica".to_string(), "localhost".to_string()],
            ip_addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            validity_days: None,
        }
    }
}

/// A new self-signed identity for `request`: the certificate and a fresh
/// key, as one PEM.
pub fn generate_identity(request: &CertificateRequest) -> anyhow::Result<String> {
    let mut params = rcgen::CertificateParams::new(request.dns_names.clone())
        .context("build certificate params")?;
    params.subject_alt_names.extend(
        request
            .ip_addresses
            .iter()
            .map(|ip| rcgen::SanType::IpAddress(*ip)),
    );
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, request.common_name.as_str());
    if let Some(days) = request.validity_days {
        let now = rcgen::date_time_ymd(1970, 1, 1)
            + std::time::Duration::from_secs(
                u64::try_from(chrono::Utc::now().timestamp()).unwrap_or_default(),
            );
        params.not_before = now;
        params.not_after = now + std::time::Duration::from_secs(u64::from(days) * 86_400);
    }
    let key_pair = rcgen::KeyPair::generate().context("generate certificate key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("self-sign certificate")?;
    Ok(format!("{}{}", cert.pem(), key_pair.serialize_pem()))
}

/// Check an uploaded chain and key and join them into one identity PEM.
///
/// Refused, with a sentence an operator can act on: a chain or key that is
/// not PEM or is too long, a key rustls cannot sign with, a key that is not
/// the leaf certificate's, and a leaf outside its validity period.
pub fn identity_from_upload(chain_pem: &str, key_pem: &str) -> Result<String, String> {
    if chain_pem.len() > MAX_PEM_BYTES || key_pem.len() > MAX_PEM_BYTES {
        return Err(format!(
            "the certificate and the key are at most {MAX_PEM_BYTES} bytes each"
        ));
    }
    let chain = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("the certificate is not PEM: {err}"))?;
    let Some(leaf) = chain.first().cloned() else {
        return Err("the certificate holds no `BEGIN CERTIFICATE` block".to_string());
    };
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|err| format!("the private key is not a PEM private key: {err}"))?;
    let signing_key = rustls::crypto::aws_lc_rs::sign::any_supported_type(&key)
        .map_err(|err| format!("the private key cannot sign TLS handshakes: {err}"))?;
    rustls::sign::CertifiedKey::new(chain, signing_key)
        .keys_match()
        .map_err(|_| "the private key is not the certificate's key".to_string())?;
    let described = describe_der(&leaf).map_err(|err| err.to_string())?;
    let now = chrono::Utc::now().timestamp();
    if now < described.not_before_unix {
        return Err(format!(
            "the certificate is not valid until {}",
            described.info.not_before
        ));
    }
    if now > described.not_after_unix {
        return Err(format!(
            "the certificate expired at {}",
            described.info.not_after
        ));
    }
    Ok(format!(
        "{}\n{}\n",
        chain_pem.trim_end(),
        key_pem.trim_end()
    ))
}

/// What the console is told about an identity. Never the key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateInfo {
    /// The leaf's subject, RFC 4514.
    pub subject: String,
    /// The leaf's issuer, RFC 4514.
    pub issuer: String,
    /// Whether the leaf is issued by its own subject.
    pub self_signed: bool,
    /// DNS subject alternative names.
    pub dns_names: Vec<String>,
    /// IP subject alternative names.
    pub ip_addresses: Vec<String>,
    /// Start of validity, RFC 3339 UTC.
    pub not_before: String,
    /// End of validity, RFC 3339 UTC.
    pub not_after: String,
    /// SHA-256 of the leaf's DER, lowercase hex.
    pub sha256: String,
}

struct Described {
    info: CertificateInfo,
    not_before_unix: i64,
    not_after_unix: i64,
}

/// Describe the leaf of an identity PEM.
pub fn describe_identity(identity_pem: &str) -> anyhow::Result<CertificateInfo> {
    let leaf = CertificateDer::pem_slice_iter(identity_pem.as_bytes())
        .next()
        .context("the identity holds no certificate")?
        .context("the identity's certificate is not PEM")?;
    Ok(describe_der(&leaf)?.info)
}

fn describe_der(leaf: &CertificateDer<'_>) -> anyhow::Result<Described> {
    use x509_parser::extensions::GeneralName;
    let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|err| anyhow::anyhow!("the certificate is not X.509: {err}"))?;
    let mut dns_names = Vec::new();
    let mut ip_addresses = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            match name {
                GeneralName::DNSName(dns) => dns_names.push((*dns).to_string()),
                GeneralName::IPAddress(bytes) => {
                    if let Ok(octets) = <[u8; 4]>::try_from(*bytes) {
                        ip_addresses.push(IpAddr::from(octets).to_string());
                    } else if let Ok(octets) = <[u8; 16]>::try_from(*bytes) {
                        ip_addresses.push(IpAddr::from(octets).to_string());
                    }
                }
                _ => {}
            }
        }
    }
    let rfc3339 = |unix: i64| {
        chrono::DateTime::from_timestamp(unix, 0)
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default()
    };
    let not_before_unix = cert.validity().not_before.timestamp();
    let not_after_unix = cert.validity().not_after.timestamp();
    Ok(Described {
        info: CertificateInfo {
            subject: cert.subject().to_string(),
            issuer: cert.issuer().to_string(),
            self_signed: cert.subject() == cert.issuer(),
            dns_names,
            ip_addresses,
            not_before: rfc3339(not_before_unix),
            not_after: rfc3339(not_after_unix),
            sha256: hex::encode(aws_lc_rs::digest::digest(
                &aws_lc_rs::digest::SHA256,
                leaf.as_ref(),
            )),
        },
        not_before_unix,
        not_after_unix,
    })
}

/// The identity file apid serves HTTPS with, and the live server
/// configuration a replacement is reloaded into while HTTPS is on: a new
/// identity takes effect on the next handshake, with no restart.
pub struct IdentityStore {
    path: std::path::PathBuf,
    live: Option<axum_server::tls_rustls::RustlsConfig>,
    write: tokio::sync::Mutex<()>,
}

impl IdentityStore {
    /// The identity at `path`, reloading `live` when it is replaced.
    pub fn new(
        path: std::path::PathBuf,
        live: Option<axum_server::tls_rustls::RustlsConfig>,
    ) -> Self {
        Self {
            path,
            live,
            write: tokio::sync::Mutex::new(()),
        }
    }

    /// The same store at `path`.
    #[must_use]
    pub fn at(&self, path: std::path::PathBuf) -> Self {
        Self::new(path, self.live.clone())
    }

    /// The same store serving `live`.
    #[must_use]
    pub fn serving(&self, live: axum_server::tls_rustls::RustlsConfig) -> Self {
        Self::new(self.path.clone(), Some(live))
    }

    /// Whether apid is serving HTTPS with this identity now.
    pub fn is_serving(&self) -> bool {
        self.live.is_some()
    }

    /// The stored identity's leaf, or `None` when there is none yet.
    pub fn describe(&self) -> anyhow::Result<Option<CertificateInfo>> {
        match std::fs::read_to_string(&self.path) {
            Ok(identity) => describe_identity(&identity).map(Some),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("read {}", self.path.display())),
        }
    }

    /// Publish `identity` (0600, one rename) and reload it into the live
    /// server when HTTPS is on.
    pub async fn replace(&self, identity: &str) -> anyhow::Result<CertificateInfo> {
        let _write = self.write.lock().await;
        let info = describe_identity(identity)?;
        crate::persist::write_atomically(&self.path, identity, 0o600)?;
        if let Some(live) = &self.live {
            live.reload_from_pem(identity.as_bytes().to_vec(), identity.as_bytes().to_vec())
                .await
                .context("reload the TLS identity")?;
        }
        Ok(info)
    }
}

/// Load the 32-byte session signing key from `session.key` in `dir`,
/// generating it (mode 0600) on first start.
pub fn load_or_generate_session_key(dir: &Path) -> anyhow::Result<[u8; 32]> {
    let key_path = dir.join("session.key");
    if key_path.exists() {
        let bytes =
            std::fs::read(&key_path).with_context(|| format!("read {}", key_path.display()))?;
        return bytes.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "{} must hold exactly 32 bytes, got {}",
                key_path.display(),
                bytes.len()
            )
        });
    }
    let key = micad_settings::random_bytes::<32>();
    crate::persist::write_atomically(&key_path, key, 0o600)?;
    tracing::info!(key = %key_path.display(), "generated session signing key");
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn generates_and_reloads_material() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        ensure_state_dir(&state).unwrap();
        assert_eq!(
            std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let generated = load_or_generate_certificate(&state).unwrap();
        assert!(generated.cert_pem.contains("BEGIN CERTIFICATE"));
        let identity = std::fs::read_to_string(state.join("identity.pem")).unwrap();
        assert!(identity.contains("BEGIN CERTIFICATE"));
        assert!(identity.contains("BEGIN PRIVATE KEY"));
        assert!(!state.join("cert.pem").exists());
        assert!(!state.join("key.pem").exists());
        let key_mode = std::fs::metadata(state.join("identity.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(key_mode & 0o777, 0o600);
        let reloaded = load_or_generate_certificate(&state).unwrap();
        assert_eq!(reloaded.cert_pem, generated.cert_pem);
        assert_eq!(reloaded.key_pem, generated.key_pem);

        let key = load_or_generate_session_key(&state).unwrap();
        assert_eq!(load_or_generate_session_key(&state).unwrap(), key);
    }

    #[test]
    fn unpublished_identity_is_replaced_as_one_complete_pair() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".identity.pem.apid-tmp"),
            "partial identity",
        )
        .unwrap();
        let generated = load_or_generate_certificate(dir.path()).unwrap();
        let reloaded = load_or_generate_certificate(dir.path()).unwrap();
        assert_eq!(generated.cert_pem, reloaded.cert_pem);
        assert_eq!(generated.key_pem, reloaded.key_pem);
        assert!(!dir.path().join(".identity.pem.apid-tmp").exists());
    }

    /// A certificate outside its validity is refused however well it matches.
    #[test]
    fn an_expired_upload_is_refused() {
        let mut params = rcgen::CertificateParams::new(vec!["old".to_string()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2001, 1, 1);
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let err = identity_from_upload(&cert.pem(), &key.serialize_pem()).unwrap_err();
        assert!(err.contains("expired"), "{err}");
    }

    /// A replaced identity is reloaded into the live server, and the file is
    /// the whole new pair at 0600.
    #[tokio::test]
    async fn a_replaced_identity_reloads_the_live_server() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate_certificate(dir.path()).unwrap();
        let live = axum_server::tls_rustls::RustlsConfig::from_pem(
            first.cert_pem.into_bytes(),
            first.key_pem.into_bytes(),
        )
        .await
        .unwrap();
        let store = IdentityStore::new(dir.path().join(IDENTITY_FILE), Some(live));
        let next = generate_identity(&CertificateRequest {
            common_name: "next".to_string(),
            validity_days: Some(30),
            ..CertificateRequest::default()
        })
        .unwrap();
        let info = store.replace(&next).await.unwrap();
        assert_eq!(info.subject, "CN=next");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(IDENTITY_FILE)).unwrap(),
            next
        );
        let mode = std::fs::metadata(dir.path().join(IDENTITY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(store.is_serving());
        assert_eq!(store.describe().unwrap().unwrap(), info);
    }

    /// The leaf a TLS server at `address` presents.
    fn served_leaf(address: std::net::SocketAddr) -> Vec<u8> {
        use std::sync::Arc;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(crate::healthcheck::AnyChain(provider)))
            .with_no_client_auth();
        let mut connection = rustls::ClientConnection::new(
            Arc::new(config),
            rustls::pki_types::ServerName::from(address.ip()),
        )
        .unwrap();
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        while connection.is_handshaking() {
            connection.complete_io(&mut socket).unwrap();
        }
        connection.peer_certificates().unwrap()[0].to_vec()
    }

    /// A running HTTPS server presents the replaced identity on the next
    /// connection, without being restarted.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_running_server_serves_the_replaced_identity_on_the_next_connection() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate_certificate(dir.path()).unwrap();
        let live = axum_server::tls_rustls::RustlsConfig::from_pem(
            first.cert_pem.clone().into_bytes(),
            first.key_pem.into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = axum_server::from_tcp_rustls(listener, live.clone())
            .serve(axum::Router::new().into_make_service());
        tokio::spawn(server);

        let before = tokio::task::spawn_blocking(move || served_leaf(address))
            .await
            .unwrap();
        assert_eq!(
            hex::encode(aws_lc_rs::digest::digest(
                &aws_lc_rs::digest::SHA256,
                &before
            )),
            describe_identity(&first.cert_pem).unwrap().sha256
        );

        let store = IdentityStore::new(dir.path().join(IDENTITY_FILE), Some(live));
        let next = generate_identity(&CertificateRequest::default()).unwrap();
        let info = store.replace(&next).await.unwrap();
        let after = tokio::task::spawn_blocking(move || served_leaf(address))
            .await
            .unwrap();
        assert_eq!(
            hex::encode(aws_lc_rs::digest::digest(
                &aws_lc_rs::digest::SHA256,
                &after
            )),
            info.sha256
        );
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn published_identity_loads_into_the_actual_tls_server() {
        let dir = tempfile::tempdir().unwrap();
        let identity = load_or_generate_certificate(dir.path()).unwrap();
        axum_server::tls_rustls::RustlsConfig::from_pem(
            identity.cert_pem.into_bytes(),
            identity.key_pem.into_bytes(),
        )
        .await
        .unwrap();
    }
}
