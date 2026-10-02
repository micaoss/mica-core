//! Access settings: the web administrator, the claim, SSH, the console, the web
//! listeners, device credentials and API tokens.

/// Access control settings.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessSettings {
    /// Web admin credentials; absent until apid sets them.
    #[serde(rename = "webAdmin", default, skip_serializing_if = "Option::is_none")]
    pub web_admin: Option<WebAdminSettings>,
    /// How this device was claimed (schema v11); absent until it is, and
    /// absent on one claimed device by design — see [`ClaimSettings`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimSettings>,
    /// SSH channel policy.
    #[serde(default)]
    pub ssh: SshSettings,
    /// Local console policy.
    #[serde(default)]
    pub console: ConsoleSettings,
    /// Where apid listens.
    #[serde(default)]
    pub web: WebSettings,
    /// Device credential metadata; never holds a plaintext secret.
    #[serde(default)]
    pub device: DeviceCredentialSettings,
    /// Bearer API tokens, hashes only.
    ///
    /// Empty by default, and empty is not written out: a device that never
    /// minted a token has a v8 document identical to its v7 form but for the
    /// version integer, which is what makes the v7 -> v8 bump additive and the
    /// A/B rollback survivable (see [`crate::MigrateV7ToV8`]).
    #[serde(rename = "apiTokens", default, skip_serializing_if = "Vec::is_empty")]
    pub api_tokens: Vec<ApiToken>,
}

/// Web admin credentials, written by apid.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebAdminSettings {
    /// Argon2id password hash in PHC string format.
    pub password_hash: String,
}

/// How the device left the unclaimed state (schema v11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimSettings {
    /// Which channel minted the first administrator credential.
    pub via: ClaimChannel,
    /// Seconds since the UNIX epoch as the device clock read them when the
    /// claim committed, saturating at 0.
    pub at: u64,
    /// Whether the credential that claimed the device is still a bootstrap
    /// secret and must be rotated before the device accepts any other
    /// authenticated write.
    #[serde(rename = "rotationRequired")]
    pub rotation_required: bool,
}

/// Which channel claimed the device.
///
/// Exactly the two channels that can mint a first administrator credential.
/// There is no `unknown` member: a third channel would be an unauthenticated
/// write nobody decided to add, and naming one here would make room for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimChannel {
    /// `POST /api/v1/setup`.
    Setup,
    /// A provisioning document.
    ProvisioningDocument,
}

/// SSH channel policy, reconciled into dropbear's arguments and the managed
/// accounts' `~/.ssh/authorized_keys`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshSettings {
    /// Whether dropbear is started.
    pub enabled: bool,
    /// TCP port dropbear listens on.
    pub port: u16,
    /// Whether the root account may log in.
    #[serde(rename = "permitRootLogin")]
    pub permit_root_login: bool,
    /// Whether password authentication is offered; the password is the device
    /// password.
    #[serde(rename = "passwordAuthentication")]
    pub password_authentication: bool,
    /// Addresses dropbear binds to (at most 10); empty means every address.
    #[serde(rename = "listenAddresses")]
    pub listen_addresses: Vec<String>,
    /// Public keys rendered into every managed account's `authorized_keys`
    /// file.
    #[serde(rename = "authorizedKeys")]
    pub authorized_keys: Vec<AuthorizedKey>,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 22,
            permit_root_login: true,
            password_authentication: true,
            listen_addresses: Vec::new(),
            authorized_keys: Vec::new(),
        }
    }
}

/// One SSH public key authorized to log in.
///
/// The comment lives in its own field rather than inside `key` so that the
/// canonical key text is what duplicate detection runs on: two operators
/// pasting the same key under different labels must not end up with two
/// entries granting the same access.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedKey {
    /// Canonical single-line key text, `<type> <base64blob>`, with no comment.
    pub key: String,
    /// Operator-supplied label; absent when the key was pasted without one.
    #[serde(rename = "comment", default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Local console policy.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConsoleSettings {
    /// Whether the tty3 root shell is started; only the `debug` image profile
    /// ships that shell at all.
    #[serde(rename = "shellEnabled")]
    pub shell_enabled: bool,
}

/// Where apid listens, reconciled into the file apid reads at start.
///
/// Off the well-known ports by default, so the console does not take 80 or 443
/// from a service the device runs: plain HTTP on 8080. With HTTPS enabled apid
/// serves on `httpsPort` with its self-signed identity, and the HTTP listener
/// only redirects there, so no login crosses the network in the clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WebSettings {
    /// TCP port of the HTTP listener.
    pub http_port: u16,
    /// Whether apid serves HTTPS.
    pub https_enabled: bool,
    /// TCP port of the HTTPS listener.
    pub https_port: u16,
}

/// The file micad renders `access.web` into and apid reads at start: runtime
/// state derived from the settings tree, re-rendered on every boot before apid
/// starts.
pub const APID_LISTENERS_PATH: &str = "/run/mica/apid.json";

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            http_port: 8080,
            https_enabled: false,
            https_port: 8443,
        }
    }
}

impl WebSettings {
    /// Whether these are the defaults, which `system.json` does not write.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The ports apid would listen on must be ports and must not be one another's
/// or those of the enabled SSH and MQTT listeners: two daemons cannot bind one
/// port, and the one that loses is the console an operator reaches the device
/// through.
pub(crate) fn validate_listeners(
    web: &WebSettings,
    ssh: &SshSettings,
    mqtt: &super::MqttSettings,
) -> Result<(), String> {
    let mut claimed = vec![("access.web.httpPort", web.http_port)];
    if web.https_enabled {
        claimed.push(("access.web.httpsPort", web.https_port));
    }
    if ssh.enabled {
        claimed.push(("access.ssh.port", ssh.port));
    }
    if mqtt.enabled {
        claimed.push(("mqtt.listen.port", mqtt.listen.port));
    }
    if web.http_port == 0 || web.https_port == 0 {
        return Err("access.web ports must be between 1 and 65535".to_string());
    }
    for (index, (name, port)) in claimed.iter().enumerate() {
        if let Some((other, _)) = claimed[..index].iter().find(|(_, taken)| taken == port) {
            return Err(format!("{name} {port} is already {other}"));
        }
    }
    Ok(())
}

/// Device credential metadata.
///
/// Holds the hash of the per-device password and its revision, never the
/// password itself. Both stay `None`/`0` in a freshly built tree: a non-empty
/// default here would be a fleet-wide shared secret baked into the signed
/// rootfs.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceCredentialSettings {
    /// Argon2id password hash in PHC string format; absent until first boot
    /// generates the credential.
    #[serde(
        rename = "passwordHash",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub password_hash: Option<String>,
    /// Revision of the stored credential, bumped on every regeneration.
    pub generation: u32,
}

/// One bearer API token, as the settings tree holds it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiToken {
    /// Stable identity of this token, lowercase hex.
    ///
    /// Identity is this field and never a list position: an index is
    /// meaningful only against the list the caller last read, and a concurrent
    /// mint slides it onto a different entry.
    pub id: String,
    /// Operator-supplied label, the only thing that tells one token from
    /// another in a listing.
    pub name: String,
    /// SHA-256 hex digest of the token secret, lowercase, 64 characters.
    ///
    /// SHA-256 and not argon2id deliberately: the secret is machine-generated
    /// and has nothing to guess, so a work factor would buy no security and
    /// would be paid on every API request rather than once per login.
    pub hash: String,
    /// Seconds since the UNIX epoch as the device clock read them when the
    /// token was minted, saturating at 0.
    pub created: u64,
}
