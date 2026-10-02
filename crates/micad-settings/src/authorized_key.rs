//! Parser and validator for SSH authorized keys.
//!
//! This module is a security boundary, not a convenience. Whatever it accepts
//! is written verbatim into a file the SSH server reads and acts on, so it parses the
//! narrowest grammar that still expresses a usable key — `<type> <blob>` with
//! an optional trailing comment — and rejects everything else, including
//! constructs OpenSSH itself would happily honour.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD_NO_PAD};

use crate::error::SettingsError;
use crate::model::AuthorizedKey;

/// Dot-path reported by every error raised here.
const KEYS_PATH: &str = "access.ssh.authorizedKeys";

/// Key types accepted, matched byte-for-byte.
///
/// The list is a whitelist rather than a "anything sshd knows" check: DSA and
/// the retired `ssh-rsa` SHA-1 signature variants are absent because a key the
/// operator cannot use is a smaller problem than a key that is weaker than the
/// operator believes.
const ACCEPTED_TYPES: [&str; 7] = [
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// Largest number of keys the settings tree will hold.
///
/// A bound has to exist because the list is rendered into a file read on every
/// login attempt; 32 is far past any real appliance and far short of a list an
/// operator could use to fill STATE.
pub const MAX_KEYS: usize = 32;

/// Largest comment accepted, in bytes.
const MAX_COMMENT_BYTES: usize = 256;

/// Smallest decoded blob accepted, in bytes.
///
/// The shortest real public key blob (Ed25519) decodes to 51 bytes; 32 is a
/// floor that rejects truncated paste-ins without hard-coding per-type sizes.
const MIN_BLOB_BYTES: usize = 32;

/// Standard-alphabet base64 as key lines carry it: `=` padding optional, and
/// the unused low bits of a final symbol not held to zero.
const KEY_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Parse one authorized-key line into its canonical form.
pub fn parse_authorized_key(line: &str) -> Result<AuthorizedKey, SettingsError> {
    check_line_shape(line)?;

    let mut fields = line.splitn(3, ' ');
    let key_type = fields.next().unwrap_or_default();
    let blob = fields
        .next()
        .ok_or_else(|| invalid("line has no base64 blob: expected `<type> <blob> [comment]`"))?;
    let comment = fields.next();

    if !ACCEPTED_TYPES.contains(&key_type) {
        return Err(invalid(format!(
            "key type field is not one of the {} accepted types",
            ACCEPTED_TYPES.len()
        )));
    }
    check_blob(blob)?;
    let decoded = decode_base64(blob)
        .ok_or_else(|| invalid("base64 blob does not decode to a byte string"))?;
    if decoded.len() < MIN_BLOB_BYTES {
        return Err(invalid(format!(
            "base64 blob decodes to {} bytes, below the {MIN_BLOB_BYTES}-byte minimum",
            decoded.len()
        )));
    }
    check_blob_declares(&decoded, key_type)?;

    let comment = match comment {
        None => None,
        Some(comment) => {
            check_comment(comment)?;
            Some(comment.to_string())
        }
    };
    Ok(AuthorizedKey {
        key: format!("{key_type} {blob}"),
        comment,
    })
}

/// Validate a whole authorized-key list as it will be persisted.
///
/// Re-parses every entry rather than trusting the stored text: the settings
/// file is an editable file on STATE, so a key that only ever passed through
/// [`parse_authorized_key`] on the way in is not the same as a key that still
/// parses on the way out.
pub fn validate_authorized_keys(keys: &[AuthorizedKey]) -> Result<(), SettingsError> {
    if keys.len() > MAX_KEYS {
        return Err(invalid(format!(
            "list holds {} keys, above the maximum of {MAX_KEYS}",
            keys.len()
        )));
    }
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, entry) in keys.iter().enumerate() {
        let parsed = parse_authorized_key(&entry.key).map_err(|err| {
            let message = match err {
                SettingsError::Validation { message, .. } => message,
                other => other.to_string(),
            };
            invalid(format!("entry {index}: {message}"))
        })?;
        if parsed.comment.is_some() {
            return Err(invalid(format!(
                "entry {index}: `key` carries a comment; the comment belongs in the `comment` field"
            )));
        }
        if let Some(comment) = &entry.comment {
            check_comment(comment).map_err(|err| {
                let message = match err {
                    SettingsError::Validation { message, .. } => message,
                    other => other.to_string(),
                };
                invalid(format!("entry {index}: {message}"))
            })?;
        }
        if let Some(first) = seen.insert(entry.key.as_str(), index) {
            return Err(invalid(format!(
                "entry {index} duplicates the key already held by entry {first}"
            )));
        }
    }
    Ok(())
}

/// Decode standard-alphabet base64, with or without `=` padding.
#[must_use]
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    KEY_BASE64.decode(input).ok()
}

/// Encode bytes as standard-alphabet base64 without `=` padding.
#[must_use]
pub fn encode_base64_nopad(input: &[u8]) -> String {
    STANDARD_NO_PAD.encode(input)
}

/// OpenSSH fingerprint of a canonical `<type> <blob>` key line: `SHA256:`
/// and the unpadded base64 of the SHA-256 digest of the decoded blob, the
/// string `ssh-keygen -lf` prints. `None` when the line has no blob or the
/// blob does not decode.
#[must_use]
pub fn ssh_fingerprint(key: &str) -> Option<String> {
    let blob = decode_base64(key.split(' ').nth(1)?)?;
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &blob);
    Some(format!("SHA256:{}", encode_base64_nopad(digest.as_ref())))
}

/// Reject bytes that must not appear anywhere in the line, and any leading or
/// trailing space.
fn check_line_shape(line: &str) -> Result<(), SettingsError> {
    if line.is_empty() {
        return Err(invalid("line is empty"));
    }
    for (offset, ch) in line.char_indices() {
        let forbidden = match ch {
            '\0' => "NUL",
            '\n' => "line feed",
            '\r' => "carriage return",
            '\t' => "tab",
            '\u{b}' => "vertical tab",
            '\u{c}' => "form feed",
            _ => continue,
        };
        return Err(invalid(format!(
            "line holds a {forbidden} at byte {offset}"
        )));
    }
    if line.starts_with(' ') {
        return Err(invalid("line starts with a space"));
    }
    if line.ends_with(' ') {
        return Err(invalid("line ends with a space"));
    }
    Ok(())
}

/// Reject a blob that is not a syntactically valid base64 word.
///
/// An empty blob is how a double space between the type and the blob shows up
/// after splitting, so the message says so: the two are the same mistake.
fn check_blob(blob: &str) -> Result<(), SettingsError> {
    if blob.is_empty() {
        return Err(invalid(
            "base64 blob is empty; fields are separated by exactly one space",
        ));
    }
    let bytes = blob.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(invalid(format!(
            "base64 blob is {} characters, not a multiple of 4",
            bytes.len()
        )));
    }
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 {
        return Err(invalid(format!(
            "base64 blob ends with {padding} padding characters, at most 2 are allowed"
        )));
    }
    for (offset, byte) in bytes[..bytes.len() - padding].iter().enumerate() {
        if !matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/') {
            return Err(invalid(format!(
                "base64 blob holds a character outside `A-Za-z0-9+/` at byte {offset}"
            )));
        }
    }
    Ok(())
}

/// Reject a blob whose own algorithm name disagrees with the declared type.
fn check_blob_declares(decoded: &[u8], key_type: &str) -> Result<(), SettingsError> {
    let Some((length_bytes, rest)) = decoded.split_at_checked(4) else {
        return Err(invalid(
            "base64 blob is too short to hold an algorithm name",
        ));
    };
    let length = u32::from_be_bytes([
        length_bytes[0],
        length_bytes[1],
        length_bytes[2],
        length_bytes[3],
    ]);
    let name = usize::try_from(length)
        .ok()
        .and_then(|length| rest.get(..length))
        .ok_or_else(|| {
            invalid("base64 blob declares an algorithm name longer than the blob itself")
        })?;
    if name != key_type.as_bytes() {
        return Err(invalid(
            "algorithm name inside the base64 blob does not match the declared key type",
        ));
    }
    Ok(())
}

/// Reject a comment that could change how the rendered file parses, or that is
/// unbounded.
fn check_comment(comment: &str) -> Result<(), SettingsError> {
    if comment.is_empty() {
        return Err(invalid(
            "comment is empty; omit the trailing space instead of leaving it blank",
        ));
    }
    if comment.len() > MAX_COMMENT_BYTES {
        return Err(invalid(format!(
            "comment is {} bytes, above the maximum of {MAX_COMMENT_BYTES}",
            comment.len()
        )));
    }
    for (offset, ch) in comment.char_indices() {
        if (ch as u32) < 0x20 || ch == '\u{7f}' {
            return Err(invalid(format!(
                "comment holds a control character at byte {offset}"
            )));
        }
    }
    Ok(())
}

/// Build the one error variant this module raises.
fn invalid(message: impl Into<String>) -> SettingsError {
    SettingsError::Validation {
        path: KEYS_PATH.to_string(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
