//! What declaring a link would take to keep the configuration the image's
//! own `.network` gives it: the entry a VLAN's undeclared parent is adopted
//! with.

use serde_json::{Value, json};

/// The drop-in micad's DNS reconciler puts beside every `.network`: it is
/// micad's own, and the global DNS setting carries it whatever the entry.
const MICA_DROP_IN: &str = "90-mica-global-dns.conf";
/// The largest unit file read.
const MAX_UNIT_BYTES: u64 = 64 * 1024;

/// The entry that keeps what `units` configure, in order: the `.network`
/// then its drop-ins. `[Match]` is not read, since networkd already chose
/// the file for this link. Anything an entry cannot say is named in the
/// error: another section, another key, more than one address, DHCP for
/// IPv6, or servers beside DHCP, which an entry would use instead of the
/// lease's rather than with them.
pub fn adopted_entry(units: &[&str]) -> Result<Value, String> {
    let mut dhcp = None;
    let mut addresses = Vec::new();
    let mut gateways = Vec::new();
    let mut dns = Vec::new();
    for text in units {
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line.to_string();
                if !matches!(section.as_str(), "[Match]" | "[Network]") {
                    return Err(format!("its {section} section"));
                }
                continue;
            }
            if section == "[Match]" {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("the line `{line}`"));
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "DHCP" => match value {
                    "yes" | "true" | "on" | "1" | "ipv4" => dhcp = Some(true),
                    "no" | "false" | "off" | "0" => dhcp = Some(false),
                    _ => return Err(format!("DHCP={value}")),
                },
                "Address" => addresses.push(value.to_string()),
                "Gateway" => gateways.push(value.to_string()),
                "DNS" => dns.extend(value.split_whitespace().map(str::to_string)),
                _ => return Err(format!("{section} {key}=")),
            }
        }
    }
    if addresses.len() > 1 {
        return Err("more than one Address=".to_string());
    }
    if gateways.len() > 1 {
        return Err("more than one Gateway=".to_string());
    }
    match (dhcp == Some(true), addresses.pop()) {
        (true, Some(_)) => Err("Address= beside DHCP=".to_string()),
        (true, None) if !dns.is_empty() => Err("DNS= beside DHCP=".to_string()),
        (true, None) if !gateways.is_empty() => Err("Gateway= beside DHCP=".to_string()),
        (true, None) => Ok(json!({ "dhcp": true })),
        (false, Some(address)) => {
            let mut static_ = json!({ "address": address, "dns": dns });
            if let Some(gateway) = gateways.pop() {
                static_["gateway"] = json!(gateway);
            }
            Ok(json!({ "dhcp": false, "static": static_ }))
        }
        (false, None) if dns.is_empty() && gateways.is_empty() => Ok(json!({ "dhcp": false })),
        (false, None) => Err("DNS= or Gateway= without an address".to_string()),
    }
}

/// `adoption` of a link networkd configures from a file that is not micad's:
/// `{networkFile, entry}`, or `{networkFile, refused}` naming what an entry
/// cannot say. `None` for a link with no file, or a file of micad's own.
pub fn adoption(link: &Value) -> Option<Value> {
    let file = link.get("NetworkFile").and_then(Value::as_str)?;
    let name = std::path::Path::new(file).file_name()?.to_str()?;
    if name.starts_with("50-mica-") {
        return None;
    }
    let mut paths = vec![file];
    paths.extend(
        link.get("NetworkFileDropins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|path| !path.ends_with(MICA_DROP_IN)),
    );
    let mut texts = Vec::new();
    for path in &paths {
        match read_unit(path) {
            Ok(text) => texts.push(text),
            Err(err) => {
                return Some(
                    json!({ "networkFile": file, "refused": format!("{path} could not be read: {err}") }),
                );
            }
        }
    }
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    Some(match adopted_entry(&texts) {
        Ok(entry) => json!({ "networkFile": file, "entry": entry }),
        Err(refused) => json!({ "networkFile": file, "refused": refused }),
    })
}

fn read_unit(path: &str) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(MAX_UNIT_BYTES)
        .read_to_string(&mut text)?;
    Ok(text)
}
