//! Syntactic checks shared by the network and system routes.

use micad_settings::{MIN_ADMIN_PASSWORD_LEN, quote_path_segment};

pub(super) const HOSTNAME_RULES: &str =
    "Hostname must be 1-63 letters, digits or hyphens and must not start or end with a hyphen.";

/// Whether an admin password is under [`MIN_ADMIN_PASSWORD_LEN`].
///
/// Bytes and not characters: `str::len` is the byte length, so a password of
/// eight non-ASCII characters is over the floor. Named rather than
/// inlined so the comparison, and not only the number, has one spelling.
///
/// The number itself is [`micad_settings::MIN_ADMIN_PASSWORD_LEN`], the one
/// statement of the rule: `micad-settings` is the crate apid and micad both
/// link, so a credential this device accepts is one it keeps accepting.
pub(super) fn password_under_floor(password: &str) -> bool {
    password.len() < MIN_ADMIN_PASSWORD_LEN
}

/// The interface-name rule micad holds every `network` key to
/// ([`micad_settings::check_iface_name`]): 1 to 15 letters, digits, `.`, `_`,
/// `-` or `:`, and not `.` or `..`. The same function, so apid cannot accept a
/// name micad refuses or refuse one it accepts.
pub(super) fn valid_iface_name(name: &str) -> bool {
    micad_settings::check_iface_name("", name).is_ok()
}

/// `^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$`
pub(super) fn valid_hostname(name: &str) -> bool {
    let bytes = name.as_bytes();
    matches!(bytes.len(), 1..=63)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
}

/// Interface form validation shared by `/network` and the setup wizard.
///
/// DHCP off with an empty address is an interface with **no** addressing, not
/// an error: a bridge port *must* carry neither `dhcp` nor `static`
/// (`validate_network` in `micad/src/reconciler/network.rs`), and it must be a
/// declared entry before a bridge may name it, so a pane that insisted on an
/// address made a bridge unbuildable through the form. An address that is
/// present and not a CIDR is still refused.
pub(super) fn validate_iface(iface: &str, dhcp: bool, address: &str) -> Result<(), &'static str> {
    if !valid_iface_name(iface) {
        return Err(
            "Interface name must be 1-15 characters of letters, digits, '.', '_', '-' or ':', and not '.' or '..'.",
        );
    }
    validate_static_address(dhcp, address)
}

/// The settings path of `iface`'s entry, with the name quoted when it carries
/// a dot: a VLAN named `eth0.100` is `network."eth0.100"`, not three segments.
pub(super) fn iface_settings_path(iface: &str) -> String {
    format!("network.{}", quote_path_segment(iface))
}

// The reconciler's own rules, run here first so the operator reads which field
// is wrong instead of a failed bus call.
pub(super) use micad_settings::{
    NetworkViolation, is_wireguard_key, map_violations, references_to, relational_violations,
    valid_cidr, validate_peers, validate_static_address,
};

// Setup wizard

#[cfg(test)]
mod tests;
