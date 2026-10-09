//! Signed component contracts shared by publication, installation and early boot.

use aws_lc_rs::{digest, signature};
use base64::{Engine, engine::general_purpose::STANDARD};
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
}

impl RootComponent {
    /// The root interface level.
    pub fn level(&self) -> u64 {
        self.interface_level
    }
}

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
    /// The root interface levels it runs on.
    pub root: Range<u64>,
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
    /// The core components, sorted by package; absent when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub core: Vec<CoreComponent>,
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
    /// One per core component, in the deployment's order.
    pub core: Vec<CorePaths>,
}

/// Where a core component's objects live on SYSTEM.
#[derive(Debug)]
pub struct CorePaths {
    pub package: String,
    pub image: String,
    pub signature: String,
}

/// The longest core component list a deployment carries.
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
            self.root.max.is_none_or(|max| max >= self.root.min),
            "empty root interface range",
        )?;
        self.content.validate()?;
        require(component_id(raw)? == self.id, "component identity mismatch")
    }

    /// Whether this component runs on a root of interface `level`.
    pub fn runs_on(&self, level: u64) -> bool {
        level >= self.root.min && self.root.max.is_none_or(|max| level <= max)
    }
}

impl Deployment {
    fn validate(&self, raw: &Value) -> Result<()> {
        require(
            matches!(
                self.schema.as_str(),
                "mica/deployment/v1" | "mica/deployment/v2"
            ) && self.data_policy == "unchanged",
            "unsupported deployment schema or DATA policy",
        )?;
        // From v2 a deployment is the system alone: its core components come from the
        // device's core set (`mica/core-set/v1`).
        require(
            self.schema == "mica/deployment/v1" || self.core.is_empty(),
            "a mica/deployment/v2 names no core components",
        )?;
        // Which board, architecture and boot format a device accepts is its
        // signed boot policy's to say ([`admit`]). What a deployment must be on
        // its own is one of the architectures and boot formats the device runs,
        // with every component agreeing with it.
        name(&self.board)?;
        require(
            matches!(self.arch.as_str(), "amd64" | "arm64"),
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
        r.content.validate()?;
        require(
            component_id(&raw["kernel"])? == k.id && component_id(&raw["rootfs"])? == r.id,
            "component identity mismatch",
        )?;
        self.validate_core(raw)
    }

    /// The core components agree with one another and with this deployment's
    /// own root: one per package, in package order, each on this architecture,
    /// each running on this root's interface level, each need met here.
    fn validate_core(&self, raw: &Value) -> Result<()> {
        require(
            self.core.len() <= MAX_CORE_COMPONENTS,
            "too many core components",
        )?;
        require(
            self.core
                .windows(2)
                .all(|pair| pair[0].package < pair[1].package),
            "core components not unique and sorted by package",
        )?;
        for (index, core) in self.core.iter().enumerate() {
            core.validate(&self.arch, &raw["core"][index])?;
            require(
                core.runs_on(self.rootfs.level()),
                "a core component does not run on this root's interface level",
            )?;
            needs_met(
                core,
                self.core.iter(),
                "a core component's need is not in the deployment",
            )?;
        }
        Ok(())
    }

    /// Every object the deployment is made of: the kernel's boot artifact and
    /// support image, the root, and each core component, each image with its
    /// root-hash signature.
    pub fn artifacts(&self) -> Vec<&Artifact> {
        let mut artifacts = vec![
            &self.kernel.boot.artifact,
            &self.kernel.support.image,
            &self.kernel.support.signature,
            &self.rootfs.content.image,
            &self.rootfs.content.signature,
        ];
        for core in &self.core {
            artifacts.push(&core.content.image);
            artifacts.push(&core.content.signature);
        }
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
            core: self
                .core
                .iter()
                .map(|core| CorePaths {
                    package: core.package.clone(),
                    image: format!("cores/{}/core.img", core.id),
                    signature: format!("cores/{}/core.roothash.p7s", core.id),
                })
                .collect(),
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
