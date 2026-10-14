use super::*;

/// apid takes exactly the names micad does: an alias with `:` is one, `.`
/// and `..` are not, and neither is anything longer than `IFNAMSIZ` allows.
#[test]
fn interface_names_follow_micad_s_rule() {
    for name in ["eth0", "eth0.100", "br-lan_1", "eth0:1", "wlan0"] {
        assert!(valid_iface_name(name), "{name:?} was refused");
    }
    for name in ["", ".", "..", "eth 0", "eth0/1", "abcdefghijklmnop"] {
        assert!(!valid_iface_name(name), "{name:?} was accepted");
    }
}
