//! The network verbs: `network` and `wifi`, through apid's local socket.

use anyhow::Result;
use serde_json::{Value, json};

use super::secret::secret;
use super::{api, json_reads, misuse};

const NETWORK_USAGE: &str = "\
usage: mica network [--json]
       mica network show <interface> [--json]

  (none)  every interface: its state and addresses
  show    one interface: state, addresses, gateway, DNS";

const WIFI_USAGE: &str = "\
usage: mica wifi [--json]
       mica wifi scan [--json]
       mica wifi on|off
       mica wifi add <ssid> [--password-file FILE] [--open] [--hidden]
       mica wifi remove <ssid>

  (none)  the station: whether it runs, its interface, the network it is on,
          and the known networks
  scan    what the radio finds
  on, off turn the station on or off (wifi.client.enabled)
  add     remember a network; its key is asked for on the terminal unless
          --password-file names a file (- is stdin), or --open says it has none
  remove  forget a network";

/// `mica network`.
pub fn network(args: &[String], json: bool) -> Result<()> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let name = match words.as_slice() {
        ["help" | "--help"] => {
            println!("{NETWORK_USAGE}");
            return Ok(());
        }
        [] => None,
        ["show", name] => Some(*name),
        _ => misuse("network", "unknown command or arguments", NETWORK_USAGE),
    };
    let status = api::get("/v1/network/status")?;
    let interfaces = status["interfaces"].as_array().cloned().unwrap_or_default();
    match name {
        None if json => println!("{status}"),
        None => {
            for interface in &interfaces {
                if interface["name"] != json!("lo") {
                    println!("{}", line(interface));
                }
            }
        }
        Some(name) => {
            let Some(interface) = interfaces.iter().find(|i| i["name"] == json!(name)) else {
                return Err(super::Failure {
                    exit: super::EXIT_FAILED,
                    code: "not_found".to_string(),
                    message: format!("no interface named {name}"),
                }
                .into());
            };
            if json {
                println!("{interface}");
            } else {
                print!("{}", detail(interface));
            }
        }
    }
    Ok(())
}

fn addresses(interface: &Value) -> Vec<String> {
    interface["addresses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|address| {
            let text = address["address"].as_str()?;
            Some(match address["prefixLength"].as_u64() {
                Some(prefix) => format!("{text}/{prefix}"),
                None => text.to_string(),
            })
        })
        .collect()
}

fn state(interface: &Value) -> &str {
    interface
        .pointer("/link/operationalState")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

/// One interface on one line: name, state, addresses.
fn line(interface: &Value) -> String {
    let mut out = format!(
        "{} {}",
        interface["name"].as_str().unwrap_or("?"),
        state(interface)
    );
    for address in addresses(interface) {
        out.push(' ');
        out.push_str(&address);
    }
    out
}

/// One interface in full.
fn detail(interface: &Value) -> String {
    let mut out = format!(
        "name: {}\nstate: {}\n",
        interface["name"].as_str().unwrap_or("?"),
        state(interface)
    );
    if let Some(mac) = interface["hardwareAddress"].as_str() {
        out.push_str(&format!("hardware address: {mac}\n"));
    }
    for address in addresses(interface) {
        out.push_str(&format!("address: {address}\n"));
    }
    for route in interface["routes"].as_array().into_iter().flatten() {
        if let (Some("0.0.0.0/0" | "::/0"), Some(via)) =
            (route["dest"].as_str(), route["via"].as_str())
        {
            out.push_str(&format!("gateway: {via}\n"));
        }
    }
    for server in interface["dns"].as_array().into_iter().flatten() {
        if let Some(server) = server.as_str() {
            out.push_str(&format!("dns: {server}\n"));
        }
    }
    if let Some(ssid) = interface.pointer("/wifi/ssid").and_then(Value::as_str) {
        out.push_str(&format!("wifi: {ssid}\n"));
    }
    out
}

/// `mica wifi`.
pub fn wifi(args: &[String], json: bool) -> Result<()> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["help" | "--help"] => {
            println!("{WIFI_USAGE}");
            return Ok(());
        }
        [] | ["scan"] => {}
        [switch @ ("on" | "off")] => {
            json_reads(json, false, "wifi", WIFI_USAGE);
            let on = *switch == "on";
            api::wait(&api::send(
                "PUT",
                "/v1/settings/wifi.client.enabled",
                &json!(on),
            )?)?;
            println!("wifi.client.enabled is {on}");
            return Ok(());
        }
        ["add", ssid, rest @ ..] => {
            json_reads(json, false, "wifi", WIFI_USAGE);
            return add(ssid, rest);
        }
        ["remove", ssid] => {
            json_reads(json, false, "wifi", WIFI_USAGE);
            api::delete(&format!("/v1/wifi/client/networks/{}", api::segment(ssid)))?;
            println!("forgot {ssid}");
            return Ok(());
        }
        _ => misuse("wifi", "unknown command or arguments", WIFI_USAGE),
    }
    if words.as_slice() == ["scan"] {
        let found = api::send("POST", "/v1/wifi/client/scan", &json!({}))?;
        if json {
            println!("{found}");
        } else {
            print!("{}", render_scan(&found));
        }
        return Ok(());
    }
    let client = api::get("/v1/wifi/client")?;
    let networks = api::get("/v1/wifi/client/networks")?;
    if json {
        println!("{}", json!({ "client": client, "networks": networks }));
        return Ok(());
    }
    println!(
        "enabled: {}\ninterface: {}",
        client["enabled"] == json!(true),
        client["interface"].as_str().unwrap_or("?")
    );
    let status = api::get("/v1/network/status")?;
    if let Some(ssid) = status["interfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["name"] == client["interface"])
        .and_then(|i| i.pointer("/wifi/ssid"))
        .and_then(Value::as_str)
    {
        println!("connected: {ssid}");
    }
    for network in networks.as_array().into_iter().flatten() {
        println!(
            "known: {} (priority {}{}{})",
            network["ssid"].as_str().unwrap_or("?"),
            network["priority"],
            if network["psk"].is_null() {
                ", open"
            } else {
                ""
            },
            if network["hidden"] == json!(true) {
                ", hidden"
            } else {
                ""
            },
        );
    }
    Ok(())
}

/// `mica wifi add`.
fn add(ssid: &str, rest: &[&str]) -> Result<()> {
    let (mut file, mut open, mut hidden) = (None, false, false);
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match *word {
            "--password-file" if file.is_none() => {
                let Some(value) = words.next() else {
                    misuse("wifi", "--password-file needs a value", WIFI_USAGE);
                };
                file = Some(*value);
            }
            "--open" if !open => open = true,
            "--hidden" if !hidden => hidden = true,
            other => misuse("wifi", &format!("unexpected argument: {other}"), WIFI_USAGE),
        }
    }
    if open && file.is_some() {
        misuse("wifi", "--open takes no key", WIFI_USAGE);
    }
    let mut body = json!({ "ssid": ssid, "hidden": hidden });
    if !open {
        body["psk"] = json!(secret(file, "Network key", true)?);
    }
    api::send("POST", "/v1/wifi/client/networks", &body)?;
    println!("remembered {ssid}");
    Ok(())
}

/// What the radio found, strongest first, or why it could not look.
fn render_scan(found: &Value) -> String {
    if found["available"] != json!(true) {
        return format!(
            "no scan: {}\n",
            found["detail"]
                .as_str()
                .or(found["reason"].as_str())
                .unwrap_or("no reason given")
        );
    }
    let mut networks = found["networks"].as_array().cloned().unwrap_or_default();
    networks
        .sort_by_key(|network| std::cmp::Reverse(network["signalDbm"].as_i64().unwrap_or(-999)));
    let mut out = String::new();
    for network in &networks {
        out.push_str(&format!(
            "{} {} dBm {} {}\n",
            network["ssid"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("(hidden)"),
            network["signalDbm"],
            network["channel"],
            network["security"].as_str().unwrap_or("?"),
        ));
    }
    if out.is_empty() {
        out.push_str("no networks found\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interface_shows_its_state_addresses_gateway_and_dns() {
        let interface = json!({
            "name": "eth0", "link": { "operationalState": "routable" },
            "hardwareAddress": "02:00:00:00:00:01",
            "addresses": [ { "address": "192.0.2.7", "prefixLength": 24 }, { "address": "fe80::1" } ],
            "routes": [ { "dest": "0.0.0.0/0", "via": "192.0.2.1" }, { "dest": "192.0.2.0/24" } ],
            "dns": [ "192.0.2.1" ],
        });
        assert_eq!(line(&interface), "eth0 routable 192.0.2.7/24 fe80::1");
        assert_eq!(
            detail(&interface),
            "name: eth0\nstate: routable\nhardware address: 02:00:00:00:00:01\naddress: 192.0.2.7/24\naddress: fe80::1\ngateway: 192.0.2.1\ndns: 192.0.2.1\n"
        );
    }

    #[test]
    fn a_scan_is_listed_strongest_first_or_says_why_not() {
        let found = json!({ "available": true, "networks": [
            { "ssid": "far", "signalDbm": -80, "channel": 1, "security": "wpa2" },
            { "ssid": "", "signalDbm": -40, "channel": 36, "security": "open" },
        ] });
        assert_eq!(
            render_scan(&found),
            "(hidden) -40 dBm 36 open\nfar -80 dBm 1 wpa2\n"
        );
        assert_eq!(
            render_scan(&json!({ "available": false, "reason": "no-station" })),
            "no scan: no-station\n"
        );
    }
}
