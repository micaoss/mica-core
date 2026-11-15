//! The network verbs: `network` and `wifi`, through apid's local socket.

use anyhow::Result;
use serde_json::{Value, json};

use super::secret::secret;
use super::{api, json_reads, misuse};

const NETWORK_USAGE: &str = "\
usage: mica network [--json]
       mica network show <interface> [--json]

  (none)  every interface: its state, whether it reaches the internet, and
          its addresses; * marks the uplink that carries the traffic
  show    one interface: state, addresses, gateway, DNS, and its
          reachability check with the last round";

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
    let interfaces = entries(&status);
    match name {
        None if json => println!("{status}"),
        None => print!("{}", table(&status)),
        Some(name) => {
            let Some(interface) = interfaces.iter().find(|i| i["name"] == json!(name)) else {
                return Err(super::Failure {
                    exit: super::EXIT_FAILED,
                    code: "not_found".to_string(),
                    message: format!("no interface named {name}"),
                }
                .into());
            };
            let uplink = uplink(&status, name);
            if json {
                let mut interface = interface.clone();
                if let (Some(object), Some(uplink)) = (interface.as_object_mut(), uplink) {
                    object.insert("uplink".to_string(), uplink.clone());
                }
                println!("{interface}");
            } else {
                print!(
                    "{}",
                    detail(interface, uplink, active(&status) == Some(name))
                );
            }
        }
    }
    Ok(())
}

/// The interfaces the observation lists.
fn entries(status: &Value) -> Vec<Value> {
    status
        .pointer("/interfaces/entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The uplink engine's entry for `name`.
fn uplink<'a>(status: &'a Value, name: &str) -> Option<&'a Value> {
    status
        .pointer("/uplinks/uplinks")?
        .as_array()?
        .iter()
        .find(|uplink| uplink["name"] == json!(name))
}

/// The uplink that carries the traffic.
fn active(status: &Value) -> Option<&str> {
    status.pointer("/uplinks/active").and_then(Value::as_str)
}

/// Whether the interface reaches the internet, as its check last said:
/// `up`, `down`, `checking`, `no route`, `blocked`, or `-` with no check.
fn internet(uplink: Option<&Value>) -> &'static str {
    match uplink.and_then(|uplink| uplink["state"].as_str()) {
        Some("up") => "up",
        Some("down") => "down",
        Some("unknown") => "checking",
        Some("noRoute") => "no route",
        Some("blocked") => "blocked",
        _ => "-",
    }
}

/// Every interface but `lo`, one a line under a header.
fn table(status: &Value) -> String {
    let rows: Vec<[String; 4]> = entries(status)
        .iter()
        .filter(|interface| interface["name"] != json!("lo"))
        .map(|interface| {
            let name = interface["name"].as_str().unwrap_or("?");
            let mark = if active(status) == Some(name) {
                "*"
            } else {
                ""
            };
            [
                format!("{name}{mark}"),
                state(interface).to_string(),
                internet(uplink(status, name)).to_string(),
                addresses(interface).join(" "),
            ]
        })
        .collect();
    let header = ["NAME", "STATE", "INTERNET", "ADDRESSES"].map(str::to_string);
    let width = |column: usize| {
        std::iter::once(&header)
            .chain(&rows)
            .map(|row| row[column].len())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2)];
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            let line = format!(
                "{:w0$}  {:w1$}  {:w2$}  {}",
                row[0],
                row[1],
                row[2],
                row[3],
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2]
            );
            format!("{}\n", line.trim_end())
        })
        .collect()
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

/// One interface in full, with its uplink entry; `active` when it carries
/// the traffic.
fn detail(interface: &Value, uplink: Option<&Value>, active: bool) -> String {
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
    if let Some(uplink) = uplink {
        out.push_str(&check(uplink, active));
    }
    out
}

/// A target as `address method[/port]`.
fn target(target: &Value) -> String {
    let address = target["address"].as_str().unwrap_or("?");
    let method = target["method"].as_str().unwrap_or("?");
    match target["port"].as_u64() {
        Some(port) => format!("{address} {method}/{port}"),
        None => format!("{address} {method}"),
    }
}

/// The uplink lines of `mica network show`: whether it reaches the internet,
/// its check, its counters and its last round.
fn check(uplink: &Value, active: bool) -> String {
    let mut out = format!(
        "internet: {}{}\n",
        internet(Some(uplink)),
        if active { " (carries the traffic)" } else { "" }
    );
    if let Some(metric) = uplink["metric"].as_u64() {
        out.push_str(&format!("metric: {metric}\n"));
    }
    let probe = &uplink["probe"];
    if probe.is_object() {
        let targets: Vec<String> = probe["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .map(target)
            .collect();
        out.push_str(&format!(
            "check: {}, every {} s, {} ms timeout, down after {}, up after {}: {}\n",
            if probe["enabled"] == json!(true) {
                "on"
            } else {
                "off"
            },
            probe["intervalS"],
            probe["timeoutMs"],
            probe["failuresToDown"],
            probe["successesToUp"],
            targets.join(", ")
        ));
    }
    if let Some(since) = uplink["since"].as_str() {
        out.push_str(&format!(
            "since: {since} ({} failed, {} good in a row)\n",
            uplink["failures"], uplink["successes"]
        ));
    }
    let round = &uplink["lastRound"];
    if let Some(at) = round["at"].as_str() {
        out.push_str(&format!(
            "last round: {at} {}\n",
            if round["ok"] == json!(true) {
                "ok"
            } else {
                "failed"
            }
        ));
        for result in round["targets"].as_array().into_iter().flatten() {
            let answer = match (result["ok"] == json!(true), result["latencyMs"].as_u64()) {
                (true, Some(ms)) => format!("ok {ms} ms"),
                (true, None) => "ok".to_string(),
                (false, _) => format!("failed: {}", result["detail"].as_str().unwrap_or("?")),
            };
            out.push_str(&format!("  {} {answer}\n", target(result)));
        }
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
    if let Some(ssid) = entries(&status)
        .iter()
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
        assert_eq!(
            detail(&interface, None, false),
            "name: eth0\nstate: routable\nhardware address: 02:00:00:00:00:01\naddress: 192.0.2.7/24\naddress: fe80::1\ngateway: 192.0.2.1\ndns: 192.0.2.1\n"
        );
    }

    fn status() -> Value {
        json!({
            "interfaces": { "available": true, "count": 4, "entries": [
                { "name": "lo", "link": { "operationalState": "carrier" } },
                { "name": "eth0", "link": { "operationalState": "routable" },
                  "addresses": [ { "address": "192.0.2.7", "prefixLength": 24 } ] },
                { "name": "eth1", "link": { "operationalState": "routable" },
                  "addresses": [ { "address": "198.51.100.7", "prefixLength": 24 } ] },
                { "name": "wlan0", "link": { "operationalState": "no-carrier" } },
            ] },
            "uplinks": {
                "active": "eth1", "natural": "eth0", "override": true,
                "uplinks": [
                    { "name": "eth0", "metric": 100, "state": "down",
                      "since": "2026-11-15T20:00:00Z", "failures": 4, "successes": 0,
                      "probe": { "enabled": true, "intervalS": 10, "timeoutMs": 2000,
                                 "failuresToDown": 3, "successesToUp": 5,
                                 "targets": [ { "address": "1.1.1.1", "method": "tcp", "port": 53 },
                                              { "address": "gateway", "method": "icmp" } ] },
                      "lastRound": { "at": "2026-11-15T20:00:40Z", "ok": false, "targets": [
                          { "address": "1.1.1.1", "method": "tcp", "port": 53, "ok": false,
                            "detail": "no answer within 2000 ms" },
                          { "address": "gateway", "method": "icmp", "ok": true, "latencyMs": 1 } ] } },
                    { "name": "eth1", "metric": 110, "state": "unknown" },
                ],
            },
        })
    }

    #[test]
    fn the_list_marks_the_active_uplink_and_says_whether_each_reaches_the_internet() {
        assert_eq!(
            table(&status()),
            "NAME   STATE       INTERNET  ADDRESSES\n\
             eth0   routable    down      192.0.2.7/24\n\
             eth1*  routable    checking  198.51.100.7/24\n\
             wlan0  no-carrier  -\n"
        );
    }

    #[test]
    fn show_prints_the_check_and_its_last_round() {
        let status = status();
        assert_eq!(
            check(uplink(&status, "eth0").unwrap(), false),
            "internet: down\n\
             metric: 100\n\
             check: on, every 10 s, 2000 ms timeout, down after 3, up after 5: 1.1.1.1 tcp/53, gateway icmp\n\
             since: 2026-11-15T20:00:00Z (4 failed, 0 good in a row)\n\
             last round: 2026-11-15T20:00:40Z failed\n  \
             1.1.1.1 tcp/53 failed: no answer within 2000 ms\n  \
             gateway icmp ok 1 ms\n"
        );
        assert_eq!(
            check(uplink(&status, "eth1").unwrap(), true),
            "internet: checking (carries the traffic)\nmetric: 110\n"
        );
        assert!(uplink(&status, "wlan0").is_none());
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
