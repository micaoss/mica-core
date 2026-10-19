//! Signed component contracts shared by publication, installation and early boot.

use aws_lc_rs::{digest, signature};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_DEPLOYMENT_BYTES: usize = 16384;
const MAX_ENVELOPE_BYTES: usize = 24576;
pub(crate) const MAX_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug)]
pub struct ContractError(pub(crate) &'static str);

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid component contract: {}", self.0)
    }
}
impl std::error::Error for ContractError {}
type Result<T> = std::result::Result<T, ContractError>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VerityGeometry {
    pub version: u64,
    pub algorithm: String,
    pub data_block_size: u64,
    pub hash_block_size: u64,
    pub data_blocks: u64,
    pub hash_offset: u64,
    pub salt: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VerityImage {
    pub image: Artifact,
    pub root_hash: String,
    pub signature: Artifact,
    pub verity: VerityGeometry,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BootArtifact {
    pub format: String,
    pub artifact: Artifact,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KernelComponent {
    pub schema: String,
    pub id: String,
    pub board: String,
    pub arch: String,
    pub build_id: String,
    pub release: String,
    pub boot: BootArtifact,
    pub support: VerityImage,
    /// What the kernel's configuration carries, as its producer composed it;
    /// absent, the kernel makes no claim (`docs/mica-core.md` 6.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RootComponent {
    pub schema: String,
    pub id: String,
    pub arch: String,
    pub content: VerityImage,
    /// The level of the interfaces the core components rely on
    /// (`docs/mica-core.md`).
    pub interface_level: u64,
    /// Root packages and their Debian versions, for the core components'
    /// `requires`; absent, the root provides nothing they could require.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provides: Option<BTreeMap<String, String>>,
}

impl RootComponent {
    /// The root interface level.
    pub fn level(&self) -> u64 {
        self.interface_level
    }

    /// What a core set's selection reads of this root.
    pub fn facts(&self) -> RootFacts<'_> {
        RootFacts {
            level: self.interface_level,
            provides: self.provides.as_ref(),
        }
    }
}

/// What a root declares to the core components composed over it: its
/// interface level and the packages it provides.
#[derive(Debug, Clone, Copy)]
pub struct RootFacts<'a> {
    pub level: u64,
    pub provides: Option<&'a BTreeMap<String, String>>,
}

/// The most packages a root declares it provides.
pub const MAX_ROOT_PROVIDES: usize = 64;
/// The most root packages a core component requires.
pub const MAX_CORE_REQUIRES: usize = 16;

/// An inclusive range; an absent `max` has no upper bound yet.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Range<T> {
    pub min: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<T>,
}

/// Another core component this one runs with, and the versions it accepts.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreNeed {
    pub package: String,
    pub min: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
}

/// A root package a core component relies on, and the Debian versions it
/// accepts; an absent bound is no bound.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootRequirement {
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
}

impl RootRequirement {
    /// Whether `root` meets it: a root that does not provide the package
    /// does, and one that provides it at a version in range.
    pub fn holds(&self, root: &RootFacts<'_>) -> bool {
        let Some(version) = root
            .provides
            .and_then(|provides| provides.get(&self.package))
        else {
            return true;
        };
        let at_least = self.min.as_deref().is_none_or(|min| {
            crate::debian_version::compare(version, min).is_some_and(|order| order.is_ge())
        });
        let at_most = self.max.as_deref().is_none_or(|max| {
            crate::debian_version::compare(version, max).is_some_and(|order| order.is_le())
        });
        at_least && at_most
    }

    /// The requirement as one reads it: `mica-podman >= 6.1.4-7`.
    pub fn describe(&self) -> String {
        let package = &self.package;
        match (&self.min, &self.max) {
            (Some(min), Some(max)) => format!("{package} >= {min}, <= {max}"),
            (Some(min), None) => format!("{package} >= {min}"),
            (None, Some(max)) => format!("{package} <= {max}"),
            (None, None) => package.clone(),
        }
    }
}

/// The core set an image carries, as its deployment names it: the signed
/// set's id, channel and generation, and its envelope's digest and length.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSetRef {
    pub id: String,
    pub channel: String,
    pub generation: u64,
    pub sha256: String,
    pub bytes: u64,
}

/// One of mica-core's packages as a component of the deployment
/// (`mica/core/v1`): its files, root-relative under `/usr` and `/etc`, in a
/// signed dm-verity image composed over the root at boot.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreComponent {
    pub schema: String,
    pub id: String,
    pub arch: String,
    /// The package the files are, e.g. `micad`.
    pub package: String,
    /// The package version, dotted numbers.
    pub version: String,
    /// The product features it serves; a product carries it when it selects one.
    pub features: Vec<String>,
    /// The other core components it needs in the same deployment.
    pub needs: Vec<CoreNeed>,
    /// The root interface levels it runs on; absent, every level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Range<u64>>,
    /// The root packages it relies on, at the versions it accepts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<RootRequirement>,
    pub content: VerityImage,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Deployment {
    pub schema: String,
    pub board: String,
    pub arch: String,
    pub generation: u64,
    pub version: String,
    pub product: String,
    pub data_policy: String,
    pub kernel: KernelComponent,
    pub rootfs: RootComponent,
    /// The core set the deployment's image carries, which a system update
    /// may install with it (`docs/mica-core.md` 6.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_set: Option<CoreSetRef>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BootIdentity {
    pub board: String,
    pub arch: String,
    pub kernel_build_id: String,
    pub kernel_release: String,
    pub support_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Envelope {
    schema: String,
    key_id: String,
    payload: String,
    signature: String,
}

#[derive(Debug)]
pub struct DeploymentPaths {
    pub rootfs: String,
    pub support: String,
    pub boot: String,
}

/// The most core components a core set carries.
pub const MAX_CORE_COMPONENTS: usize = 16;

/// A package version as its dotted numbers, for ordering.
pub(crate) fn version_key(value: &str) -> Result<Vec<u64>> {
    let parts: Vec<u64> = value
        .split('.')
        .map(|part| {
            require(
                !part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit()),
                "invalid version",
            )?;
            part.parse().map_err(|_| ContractError("invalid version"))
        })
        .collect::<Result<_>>()?;
    require((1..=4).contains(&parts.len()), "invalid version")?;
    Ok(parts)
}

pub(crate) fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(ContractError(message))
    }
}

fn hash(value: &str) -> Result<()> {
    require(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid digest",
    )
}

pub(crate) fn name(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)),
        "invalid identifier",
    )
}

pub(crate) fn integer(value: u64, maximum: u64) -> Result<()> {
    require(value > 0 && value <= maximum, "invalid integer")
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}

/// Components exclude their own ID; deployments contain no self-reference.
pub fn component_id(value: &Value) -> Result<String> {
    let mut fields = value
        .as_object()
        .ok_or(ContractError("expected object"))?
        .clone();
    fields.remove("id");
    let bytes = serde_json::to_vec(&fields).map_err(|_| ContractError("invalid JSON"))?;
    Ok(sha256(&bytes))
}

impl Artifact {
    fn validate(&self, maximum: u64) -> Result<()> {
        integer(self.bytes, maximum)?;
        hash(&self.sha256)
    }

    /// Check downloaded or published bytes; boot uses signed dm-verity instead.
    pub fn verify(&self, bytes: &[u8]) -> Result<()> {
        self.validate(MAX_INTEGER)?;
        require(
            bytes.len() as u64 == self.bytes && sha256(bytes) == self.sha256,
            "artifact length or digest mismatch",
        )
    }
}

impl VerityImage {
    fn validate(&self) -> Result<()> {
        self.image.validate(MAX_INTEGER)?;
        self.signature.validate(65536)?;
        hash(&self.root_hash)?;
        let g = &self.verity;
        require(
            g.version == 1
                && g.algorithm == "sha256"
                && g.data_block_size == 4096
                && g.hash_block_size == 4096,
            "unsupported verity geometry",
        )?;
        integer(g.data_blocks, MAX_INTEGER / 4096)?;
        integer(g.hash_offset, MAX_INTEGER)?;
        hash(&g.salt)?;
        require(
            g.hash_offset == g.data_blocks * 4096,
            "hash tree must immediately follow data",
        )?;
        let mut blocks = g.data_blocks;
        let mut tree_blocks = 0;
        while blocks > 1 {
            blocks = blocks.div_ceil(128);
            tree_blocks += blocks;
        }
        let bytes = g.hash_offset + tree_blocks * 4096;
        integer(bytes, MAX_INTEGER)?;
        require(
            self.image.bytes == bytes,
            "image length does not match verity tree",
        )
    }
}

impl CoreComponent {
    pub(crate) fn validate(&self, arch: &str, raw: &Value) -> Result<()> {
        require(self.schema == "mica/core/v1", "wrong component schema")?;
        require(self.arch == arch, "component target mismatch")?;
        hash(&self.id)?;
        name(&self.package)?;
        version_key(&self.version)?;
        require(
            !self.features.is_empty() && self.features.len() <= 16,
            "invalid core features",
        )?;
        for feature in &self.features {
            name(feature)?;
        }
        require(
            self.needs.len() <= MAX_CORE_COMPONENTS,
            "invalid core needs",
        )?;
        for need in &self.needs {
            name(&need.package)?;
            require(
                need.package != self.package,
                "a core component needs itself",
            )?;
            let min = version_key(&need.min)?;
            if let Some(max) = &need.max {
                require(version_key(max)? >= min, "empty version range")?;
            }
        }
        require(
            self.root
                .as_ref()
                .is_none_or(|root| root.max.is_none_or(|max| max >= root.min)),
            "empty root interface range",
        )?;
        require(
            self.requires.len() <= MAX_CORE_REQUIRES,
            "invalid core requires",
        )?;
        for requirement in &self.requires {
            name(&requirement.package)?;
            for bound in [&requirement.min, &requirement.max].into_iter().flatten() {
                require(
                    crate::debian_version::valid(bound),
                    "invalid required version",
                )?;
            }
            if let (Some(min), Some(max)) = (&requirement.min, &requirement.max) {
                require(
                    crate::debian_version::compare(max, min).is_some_and(|o| o.is_ge()),
                    "empty version range",
                )?;
            }
        }
        self.content.validate()?;
        require(component_id(raw)? == self.id, "component identity mismatch")
    }

    /// Whether this component runs on a root of interface `level`.
    pub fn runs_on(&self, level: u64) -> bool {
        self.root
            .as_ref()
            .is_none_or(|root| level >= root.min && root.max.is_none_or(|max| level <= max))
    }
}

impl Deployment {
    fn validate(&self, raw: &Value) -> Result<()> {
        require(
            // The system alone: a device's core components come from its
            // core set (`mica/set/v1`), never from a deployment.
            self.schema == "mica/deployment/v1" && self.data_policy == "unchanged",
            "unsupported deployment schema or DATA policy",
        )?;
        // Which board, architecture and boot format a device accepts is its
        // signed boot policy's to say ([`admit`]). What a deployment must be on
        // its own is one of the architectures and boot formats the device runs,
        // with every component agreeing with it.
        name(&self.board)?;
        require(
            matches!(self.arch.as_str(), "amd64" | "arm64" | "armhf"),
            "unsupported architecture",
        )?;
        integer(self.generation, MAX_INTEGER)?;
        name(&self.version)?;
        name(&self.product)?;
        let k = &self.kernel;
        let r = &self.rootfs;
        require(
            k.schema == "mica/kernel/v1" && r.schema == "mica/rootfs/v1",
            "wrong component schema",
        )?;
        integer(r.interface_level, MAX_INTEGER)?;
        require(
            k.board == self.board && k.arch == self.arch && r.arch == self.arch,
            "component target mismatch",
        )?;
        hash(&k.id)?;
        hash(&r.id)?;
        hash(&k.build_id)?;
        name(&k.release)?;
        require(
            matches!(k.boot.format.as_str(), "uki" | "fit"),
            "unsupported boot format",
        )?;
        k.boot.artifact.validate(MAX_INTEGER)?;
        k.support.validate()?;
        if let Some(features) = &k.features {
            require(
                features.len() <= 32
                    && features.iter().all(|f| {
                        (1..=32).contains(&f.len())
                            && f.bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    })
                    && features.windows(2).all(|pair| pair[0] < pair[1]),
                "invalid kernel features",
            )?;
        }
        r.content.validate()?;
        if let Some(provides) = &r.provides {
            require(provides.len() <= MAX_ROOT_PROVIDES, "invalid root provides")?;
            for (package, version) in provides {
                name(package)?;
                require(
                    crate::debian_version::valid(version),
                    "invalid provided version",
                )?;
            }
        }
        if let Some(core) = &self.core_set {
            hash(&core.id)?;
            hash(&core.sha256)?;
            crate::update_set::channel(&core.channel)?;
            integer(core.generation, MAX_INTEGER)?;
            require(
                core.generation > 0
                    && core.bytes > 0
                    && core.bytes <= crate::set_state::MAX_ENVELOPE_BYTES,
                "invalid core set reference",
            )?;
        }
        require(
            component_id(&raw["kernel"])? == k.id && component_id(&raw["rootfs"])? == r.id,
            "component identity mismatch",
        )?;
        Ok(())
    }

    /// Every object the deployment is made of: the kernel's boot artifact and
    /// support image and the root, each image with its root-hash signature.
    pub fn artifacts(&self) -> Vec<&Artifact> {
        let artifacts = vec![
            &self.kernel.boot.artifact,
            &self.kernel.support.image,
            &self.kernel.support.signature,
            &self.rootfs.content.image,
            &self.rootfs.content.signature,
        ];
        artifacts
    }

    /// Paths are derived only from the IDs of a validated deployment.
    pub fn paths(&self) -> Result<DeploymentPaths> {
        let raw = serde_json::to_value(self).map_err(|_| ContractError("invalid JSON"))?;
        self.validate(&raw)?;
        Ok(DeploymentPaths {
            rootfs: format!("roots/{}/rootfs.img", self.rootfs.id),
            support: format!("kernels/{}/support.img", self.kernel.id),
            boot: if self.kernel.boot.format == "uki" {
                format!("EFI/mica/kernels/{}.efi", self.kernel.id)
            } else {
                format!("kernels/{}/boot.itb", self.kernel.id)
            },
        })
    }
}

/// Every need of `core` met by one of `others`, at a version in its range; `missing`
/// names the refusal when a needed package is not among them.
pub(crate) fn needs_met<'a>(
    core: &CoreComponent,
    others: impl Iterator<Item = &'a CoreComponent> + Clone,
    missing: &'static str,
) -> Result<()> {
    for need in &core.needs {
        let found = others
            .clone()
            .find(|other| other.package == need.package)
            .ok_or(ContractError(missing))?;
        let version = version_key(&found.version)?;
        require(
            version >= version_key(&need.min)?
                && match &need.max {
                    Some(max) => version <= version_key(max)?,
                    None => true,
                },
            "a core component's need is outside its version range",
        )?;
    }
    Ok(())
}

/// Where the running root names the product it was built as.
pub const PRODUCT_FILE: &str = "/usr/lib/mica/product.conf";

/// The product of `/usr/lib/mica/product.conf`: its one unquoted `PRODUCT=<name>`
/// line. Other keys are ignored; a missing, repeated or malformed line is refused.
pub fn device_product(text: &str) -> Result<String> {
    let mut found = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("PRODUCT=") {
            require(found.is_none(), "product file names more than one product")?;
            name(value).map_err(|_| ContractError("invalid product in the product file"))?;
            found = Some(value.to_owned());
        }
    }
    found.ok_or(ContractError("product file names no product"))
}

/// Parse the compact, key-sorted payload with a strict size and schema boundary.
pub fn parse_deployment(payload: &[u8]) -> Result<Deployment> {
    require(
        payload.len() <= MAX_DEPLOYMENT_BYTES,
        "deployment too large",
    )?;
    let raw: Value = serde_json::from_slice(payload).map_err(|_| ContractError("invalid JSON"))?;
    let canonical = serde_json::to_vec(&raw).map_err(|_| ContractError("invalid JSON"))?;
    require(
        canonical == payload,
        "noncanonical or duplicate JSON fields",
    )?;
    let descriptor: Deployment = serde_json::from_value(raw.clone())
        .map_err(|_| ContractError("unknown, missing or invalid fields"))?;
    descriptor.validate(&raw)?;
    Ok(descriptor)
}

fn base64(value: &str) -> Result<Vec<u8>> {
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| ContractError("invalid base64"))?;
    require(STANDARD.encode(&bytes) == value, "noncanonical base64")?;
    Ok(bytes)
}

/// Authenticate acquisition metadata before choosing an exact tested combination.
pub fn authenticate_payload(
    bytes: &[u8],
    public_keys: &[[u8; 32]],
    limit: usize,
) -> Result<Vec<u8>> {
    require(bytes.len() <= limit * 4 / 3 + 1024, "envelope too large")?;
    let e: Envelope =
        serde_json::from_slice(bytes).map_err(|_| ContractError("invalid envelope"))?;
    require(
        serde_json::to_vec(&e).map_err(|_| ContractError("invalid envelope"))? == bytes,
        "noncanonical envelope",
    )?;
    require(
        e.schema == "mica/update-envelope/v1",
        "wrong envelope schema",
    )?;
    hash(&e.key_id)?;
    require(
        !public_keys.is_empty() && public_keys.len() <= 8,
        "invalid trust set",
    )?;
    let key = public_keys
        .iter()
        .find(|key| sha256(*key) == e.key_id)
        .ok_or(ContractError("untrusted metadata key"))?;
    let payload = base64(&e.payload)?;
    require(payload.len() <= limit, "payload too large")?;
    let signature = base64(&e.signature)?;
    require(signature.len() == 64, "invalid signature length")?;
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(&payload, &signature)
        .map_err(|_| ContractError("metadata signature rejected"))?;
    Ok(payload)
}

pub fn authenticate_deployment(bytes: &[u8], public_keys: &[[u8; 32]]) -> Result<Deployment> {
    require(bytes.len() <= MAX_ENVELOPE_BYTES, "envelope too large")?;
    parse_deployment(&authenticate_payload(
        bytes,
        public_keys,
        MAX_DEPLOYMENT_BYTES,
    )?)
}

/// Hold a parsed deployment to the device its signed boot policy describes:
/// this board, this architecture, this kernel format.
///
/// The parser cannot: a deployment for another board is well formed, and only
/// the device knows it is not the board it is running on.
pub fn admit(
    d: &Deployment,
    running: &BootIdentity,
    kernel: crate::board::KernelFormat,
) -> Result<()> {
    require(
        d.board == running.board && d.arch == running.arch,
        "board/architecture mismatch",
    )?;
    require(d.kernel.boot.format == kernel.as_str(), "wrong boot format")
}

/// Boot additionally binds the signed deployment to the running UKI/FIT.
pub fn verify_deployment(
    bytes: &[u8],
    public_keys: &[[u8; 32]],
    running: &BootIdentity,
    kernel: crate::board::KernelFormat,
) -> Result<Deployment> {
    let d = authenticate_deployment(bytes, public_keys)?;
    admit(&d, running, kernel)?;
    require(
        d.kernel.build_id == running.kernel_build_id
            && d.kernel.release == running.kernel_release
            && component_id(
                &serde_json::to_value(&d.kernel.support)
                    .map_err(|_| ContractError("invalid support metadata"))?,
            )? == running.support_id,
        "running kernel/support mismatch",
    )?;
    Ok(d)
}
