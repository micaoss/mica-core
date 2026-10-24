//! The device code, and the access point's default key derived from it.
//!
//! The code names the device by its hardware: a digest of one value the board
//! declares in [`CONFIG_PATH`], which survives reflashing. The default
//! access-point key is an HMAC of the code under the product's secret
//! ([`SECRET_PATH`]), so the label side derives the same key offline from the
//! printed code (`docs/factory-ap-key.md`). The secret ships in a public
//! image: the key is a first-contact credential, used only while the device
//! is unclaimed.

use std::path::{Path, PathBuf};

/// The board's declaration of its code's source: `source=<name>`.
pub const CONFIG_PATH: &str = "usr/share/mica/device-code.conf";
/// The product secret the default access-point key is derived under.
pub const SECRET_PATH: &str = "usr/share/mica/ap-secret";
/// The fewest bytes a product secret holds.
pub const MIN_SECRET_BYTES: usize = 32;

/// The 32 characters a code and a key are spelled in: digits and capitals
/// without `0`, `O`, `1` and `I`.
const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// Characters of a code before its check character.
const DATA_CHARS: usize = 10;
/// Characters of a derived key.
const KEY_CHARS: usize = 16;
/// Characters of the default SSID's suffix.
const SSID_CHARS: usize = 4;
const CODE_DOMAIN: &[u8] = b"mica-device-code-v1";
const SSID_DOMAIN: &[u8] = b"mica-ssid-v1";
const KEY_DOMAIN: &[u8] = b"mica-ap-psk-v1";
/// The largest source value read.
const MAX_SOURCE_BYTES: u64 = 4096;

/// DMI strings firmware leaves where a serial was never written.
const DMI_PLACEHOLDERS: &[&str] = &[
    "to be filled by o.e.m.",
    "default string",
    "system serial number",
    "not specified",
    "none",
    "0",
];
/// The UUID AMI firmware ships on boards nobody gave one.
const DMI_PLACEHOLDER_UUID: &str = "03000200-0400-0500-0006-000700080009";

/// Where a device code comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The CID of the one non-removable eMMC.
    EmmcCid,
    /// The devicetree's `serial-number`, which the boot loader writes.
    DtSerial,
    /// The DMI product serial.
    DmiSerial,
    /// The DMI product UUID.
    DmiUuid,
    /// The permanent address of the onboard Ethernet port with the lowest
    /// device path.
    Mac,
}

impl Source {
    /// The name `device-code.conf` and the API spell it with.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::EmmcCid => "emmc-cid",
            Self::DtSerial => "dt-serial",
            Self::DmiSerial => "dmi-serial",
            Self::DmiUuid => "dmi-uuid",
            Self::Mac => "mac",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [
            Self::EmmcCid,
            Self::DtSerial,
            Self::DmiSerial,
            Self::DmiUuid,
            Self::Mac,
        ]
        .into_iter()
        .find(|source| source.name() == name)
    }
}

/// A device's code and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// The 11 canonical characters.
    pub code: String,
    pub source: Source,
}

impl DeviceCode {
    /// `{code, source}`, the code in its printed form.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "code": printed(&self.code), "source": self.source.name() })
    }
}

fn read_limited(path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_SOURCE_BYTES).read_to_string(&mut text))
        .map_err(|err| format!("{} could not be read: {err}", path.display()))?;
    Ok(text)
}

/// The source `root`'s board declares.
///
/// # Errors
/// The file is missing or names no source this build knows.
pub fn configured_source(root: &Path) -> Result<Source, String> {
    let text = read_limited(&root.join(CONFIG_PATH))?;
    let name = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("source="))
        .ok_or_else(|| format!("{CONFIG_PATH} names no source"))?;
    Source::parse(name.trim())
        .ok_or_else(|| format!("{CONFIG_PATH} names an unknown source {name:?}"))
}

/// Whether `value` is nothing but `digit` characters, dashes and colons.
fn only(value: &str, digit: char) -> bool {
    value
        .chars()
        .filter(|c| !matches!(c, '-' | ':'))
        .all(|c| c.eq_ignore_ascii_case(&digit))
}

fn sys_value(path: &Path) -> Result<String, String> {
    Ok(read_limited(path)?
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string())
}

/// The value `source` reads under `root`.
///
/// # Errors
/// The value is absent, or is a placeholder rather than the hardware's own.
pub fn read_source(root: &Path, source: Source) -> Result<String, String> {
    let value = match source {
        Source::EmmcCid => emmc_cid(root)?,
        Source::DtSerial => sys_value(&root.join("sys/firmware/devicetree/base/serial-number"))?,
        Source::DmiSerial => {
            let value = sys_value(&root.join("sys/class/dmi/id/product_serial"))?;
            if DMI_PLACEHOLDERS.contains(&value.to_ascii_lowercase().as_str()) || only(&value, 'f')
            {
                return Err(format!("the DMI serial is the placeholder {value:?}"));
            }
            value
        }
        Source::DmiUuid => {
            let value = sys_value(&root.join("sys/class/dmi/id/product_uuid"))?;
            if value.eq_ignore_ascii_case(DMI_PLACEHOLDER_UUID) || only(&value, 'f') {
                return Err(format!("the DMI UUID is the placeholder {value}"));
            }
            value
        }
        Source::Mac => permanent_mac(root)?,
    };
    if value.is_empty() || only(&value, '0') {
        return Err(format!("{} is empty", source.name()));
    }
    Ok(value)
}

/// The CID of the lowest-numbered MMC (not SD) block device.
fn emmc_cid(root: &Path) -> Result<String, String> {
    let block = root.join("sys/block");
    let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(&block)
        .map_err(|err| format!("{} could not be read: {err}", block.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let index = name.strip_prefix("mmcblk")?.parse().ok()?;
            let device = entry.path().join("device");
            (sys_value(&device.join("type")).ok()? == "MMC").then_some((index, device))
        })
        .collect();
    found.sort();
    let (_, device) = found.first().ok_or("no eMMC is attached")?;
    sys_value(&device.join("cid"))
}

/// The permanent, globally administered address of the onboard Ethernet port
/// with the lowest device path: USB and wireless links are not onboard.
fn permanent_mac(root: &Path) -> Result<String, String> {
    let net = root.join("sys/class/net");
    let mut found: Vec<(PathBuf, String)> = std::fs::read_dir(&net)
        .map_err(|err| format!("{} could not be read: {err}", net.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let link = entry.path();
            let device = std::fs::canonicalize(link.join("device")).ok()?;
            let ethernet = sys_value(&link.join("type")).ok()? == "1";
            let permanent = sys_value(&link.join("addr_assign_type")).ok()? == "0";
            let wireless = link.join("phy80211").exists() || link.join("wireless").exists();
            let usb = device.to_string_lossy().contains("/usb");
            let address = sys_value(&link.join("address")).ok()?.to_ascii_lowercase();
            let first = u8::from_str_radix(address.get(..2)?, 16).ok()?;
            (ethernet && permanent && !wireless && !usb && first & 0x02 == 0)
                .then_some((device, address))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .next()
        .map(|(_, address)| address)
        .ok_or_else(|| "no onboard Ethernet port has a permanent address".to_string())
}

fn sha256(parts: &[&[u8]]) -> aws_lc_rs::digest::Digest {
    let mut context = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    for part in parts {
        context.update(part);
    }
    context.finish()
}

/// The Luhn mod 32 check character over `data`.
fn check_char(data: &[u8]) -> u8 {
    let n = ALPHABET.len() as u32;
    let mut factor = 2;
    let mut sum = 0;
    for c in data.iter().rev() {
        let point = ALPHABET.iter().position(|a| a == c).unwrap_or(0) as u32;
        let addend = factor * point;
        sum += addend / n + addend % n;
        factor = if factor == 2 { 1 } else { 2 };
    }
    ALPHABET[((n - sum % n) % n) as usize]
}

/// The first `count` characters of the alphabet `digest` spells, 5 bits
/// each, most significant bit first.
fn spell(digest: &aws_lc_rs::digest::Digest, count: usize) -> Vec<u8> {
    let bytes = digest.as_ref();
    (0..count)
        .map(|index| {
            let bit = index * 5;
            let window = u16::from_be_bytes([bytes[bit / 8], bytes[bit / 8 + 1]]);
            ALPHABET[usize::from((window >> (11 - bit % 8)) & 0x1f)]
        })
        .collect()
}

/// The canonical code of `raw`, read from `source`.
#[must_use]
pub fn code_of(source: Source, raw: &str) -> String {
    let digest = sha256(&[
        CODE_DOMAIN,
        b"\0",
        source.name().as_bytes(),
        b"\0",
        raw.as_bytes(),
    ]);
    let mut data = spell(&digest, DATA_CHARS);
    data.push(check_char(&data));
    String::from_utf8(data).unwrap_or_default()
}

/// `code` as printed: `XXXX-XXXX-XXX`.
#[must_use]
pub fn printed(code: &str) -> String {
    format!("{}-{}-{}", &code[..4], &code[4..8], &code[8..])
}

/// The canonical form of a typed code: dashes and spaces dropped, letters
/// upper-cased, its check character verified.
///
/// # Errors
/// It is not 11 characters of the alphabet, or its check character is wrong.
pub fn parse_code(input: &str) -> Result<String, String> {
    let code: String = input
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if code.len() != DATA_CHARS + 1 || !code.bytes().all(|c| ALPHABET.contains(&c)) {
        return Err(format!("{input:?} is not a device code"));
    }
    if check_char(&code.as_bytes()[..DATA_CHARS]) != code.as_bytes()[DATA_CHARS] {
        return Err(format!("{input:?} has a wrong check character"));
    }
    Ok(code)
}

/// The default SSID of a device with the canonical `code`: a hash of the
/// code under its own domain, so the SSID repeats none of the code's
/// characters.
#[must_use]
pub fn ssid_of(code: &str) -> String {
    let suffix = spell(&sha256(&[SSID_DOMAIN, b"\0", code.as_bytes()]), SSID_CHARS);
    format!("mica-{}", String::from_utf8(suffix).unwrap_or_default())
}

/// The default access-point key of `code` under `secret`.
#[must_use]
pub fn key_of(secret: &[u8], code: &str) -> String {
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, secret);
    let mut context = aws_lc_rs::hmac::Context::with_key(&key);
    context.update(KEY_DOMAIN);
    context.update(b"\0");
    context.update(code.as_bytes());
    let tag = context.sign();
    tag.as_ref()[..KEY_CHARS]
        .iter()
        .map(|byte| char::from(ALPHABET[usize::from(byte % 32)]))
        .collect()
}

/// A product secret file's key bytes: the file without one trailing newline.
///
/// # Errors
/// It cannot be read or holds fewer than [`MIN_SECRET_BYTES`].
pub fn secret_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = std::fs::read(path)
        .map_err(|err| format!("{} could not be read: {err}", path.display()))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() < MIN_SECRET_BYTES {
        return Err(format!(
            "{} holds fewer than {MIN_SECRET_BYTES} bytes",
            path.display()
        ));
    }
    Ok(bytes)
}

/// The image's product secret under `root`, readable by its owner only.
///
/// # Errors
/// It is missing, too short, or readable by others.
pub fn product_secret(root: &Path) -> Result<Vec<u8>, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = root.join(SECRET_PATH);
    let mode = std::fs::metadata(&path)
        .map_err(|err| format!("{} could not be read: {err}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(format!("{} is readable by others", path.display()));
    }
    secret_bytes(&path)
}

/// The device code of the device under `root`.
///
/// # Errors
/// The board declares no source, or the source has no value.
pub fn device_code(root: &Path) -> Result<DeviceCode, String> {
    let source = configured_source(root)?;
    let raw = read_source(root, source)?;
    Ok(DeviceCode {
        code: code_of(source, &raw),
        source,
    })
}

/// The key the label prints for the device under `root`: its code under the
/// product secret.
///
/// # Errors
/// The device has no code, or no usable secret.
pub fn label_key(root: &Path) -> Result<String, String> {
    let code = device_code(root)?;
    Ok(key_of(&product_secret(root)?, &code.code))
}

/// Where micad puts the device code for the login prompt. Base links
/// `/etc/issue.d/50-mica-device-code.issue` here; agetty shows the files of
/// `/etc/issue.d` after `/etc/issue`.
pub const ISSUE_PATH: &str = "/run/mica/device-code.issue";

/// Put this device's code where whoever has the device in hand reads it: the
/// login prompt, through `issue`, and the log micad starts with. A device
/// without a code gets no line.
pub fn announce(root: &Path, issue: &Path) {
    match device_code(root) {
        Ok(code) => {
            let printed = printed(&code.code);
            tracing::info!(code = %printed, source = code.source.name(), "device code");
            if let Err(err) = write_issue(issue, &format!("Device code: {printed}\n\n")) {
                tracing::warn!(path = %issue.display(), error = %err, "the device code could not be put on the login prompt");
            }
        }
        Err(reason) => {
            tracing::info!(%reason, "this device has no code");
            if let Err(err) = std::fs::remove_file(issue)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %issue.display(), error = %err, "a stale device code could not be removed");
            }
        }
    }
}

/// `text` at `path`, replaced whole: a getty never reads half a file.
fn write_issue(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let staged = path.with_extension("issue.tmp");
    std::fs::write(&staged, text)?;
    std::fs::rename(&staged, path)
}

/// `micad device-code`: print this device's code and its source.
pub fn device_code_main() -> anyhow::Result<()> {
    match device_code(Path::new("/")) {
        Ok(code) => {
            println!("{} ({})", printed(&code.code), code.source.name());
            Ok(())
        }
        Err(reason) => {
            eprintln!("micad device-code: this device has no code: {reason}");
            std::process::exit(1);
        }
    }
}

/// `micad derive-ap-key --secret FILE CODE`: print the default SSID and key a
/// device with `CODE` has under the product secret in `FILE`.
pub fn derive_ap_key_main(args: &[String]) -> anyhow::Result<()> {
    const USAGE: &str = "usage: micad derive-ap-key --secret FILE CODE";
    let (secret, code) = match args {
        [flag, file, code] if flag == "--secret" => (file, code),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let code = parse_code(code).map_err(anyhow::Error::msg)?;
    let secret = secret_bytes(Path::new(secret)).map_err(anyhow::Error::msg)?;
    println!("ssid={}", ssid_of(&code));
    println!("psk={}", key_of(&secret, &code));
    Ok(())
}

#[cfg(test)]
mod tests;
