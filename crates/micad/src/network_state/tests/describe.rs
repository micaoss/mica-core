//! The observed network from networkd's description.

use serde_json::json;

use super::*;

#[test]
pub(super) fn describe_is_reduced_to_stable_interface_details() {
    let normalized = normalize(serde_json::json!({
        "Interfaces": [{
            "Index": 2,
            "Name": "eth0",
            "OperationalState": "routable",
            "CarrierState": "carrier",
            "Addresses": [{"Address": [192, 0, 2, 10], "PrefixLength": 24}],
            "UnstableFutureField": "ignored"
        }]
    }))
    .unwrap();
    assert_eq!(normalized["interfaceCount"], 1);
    assert_eq!(normalized["interfaces"][0]["name"], "eth0");
    assert_eq!(normalized["interfaces"][0]["operationalState"], "routable");
    assert!(normalized["interfaces"][0]["UnstableFutureField"].is_null());
}

/// The observed surface over the fixture: carrier, formatted addresses,
/// the lease from the DHCP client object, the one default route, the
/// per-link DNS, and nothing from the desired settings anywhere.
#[test]
pub(super) fn the_observed_surface_carries_link_address_lease_route_and_dns() {
    let describe = describe_fixture();
    let observed = observed_json(
        Ok(&describe),
        &WifiEvidence::default(),
        None,
        &RadioEvidence::default(),
    );

    assert_eq!(observed["interfaces"]["available"], true);
    assert_eq!(observed["interfaces"]["count"], 3);
    let eth0 = &observed["interfaces"]["entries"][1];
    assert_eq!(eth0["name"], "eth0");
    assert_eq!(eth0["link"]["carrier"], true);
    assert_eq!(eth0["link"]["operationalState"], "routable");
    assert_eq!(eth0["hardwareAddress"], "02:42:ac:11:00:02");
    assert_eq!(eth0["addresses"][0]["address"], "192.0.2.10");
    assert_eq!(eth0["addresses"][0]["family"], "ipv4");
    assert_eq!(eth0["addresses"][0]["prefixLength"], 24);
    assert_eq!(eth0["addresses"][0]["configSource"], "DHCPv4");
    assert_eq!(eth0["addresses"][1]["address"], "fe80::42:acff:fe11:2");
    assert_eq!(eth0["addresses"][1]["family"], "ipv6");
    assert_eq!(eth0["dhcp"]["available"], true);
    assert_eq!(eth0["dhcp"]["inferred"], false);
    assert_eq!(eth0["dhcp"]["state"], "bound");
    assert_eq!(eth0["dhcp"]["lease"]["address"], "192.0.2.10");
    assert_eq!(eth0["dhcp"]["lease"]["server"], "192.0.2.1");
    assert_eq!(eth0["dhcp"]["lease"]["router"], "192.0.2.1");
    assert_eq!(eth0["dhcp"]["lease"]["lifetimeSeconds"], 86_400);
    assert_eq!(eth0["dns"], json!(["192.0.2.1"]));
    assert!(eth0.get("wifi").is_none());

    let wlan0 = &observed["interfaces"]["entries"][2];
    assert_eq!(wlan0["link"]["carrier"], false);
    assert_eq!(wlan0["dhcp"]["available"], false);
    assert!(wlan0["dhcp"]["detail"].is_string());
    assert!(wlan0.get("UnstableFutureField").is_none());

    assert_eq!(observed["defaultRoutes"]["available"], true);
    assert_eq!(observed["defaultRoutes"]["count"], 1);
    let route = &observed["defaultRoutes"]["entries"][0];
    assert_eq!(route["gateway"], "192.0.2.1");
    assert_eq!(route["interface"], "eth0");
    assert_eq!(route["interfaceIndex"], 2);
    assert_eq!(route["metric"], 1024);
    assert_eq!(route["protocol"], "dhcp");
    assert_eq!(route["family"], "ipv4");

    assert_eq!(observed["dns"]["linkServers"], json!(["192.0.2.1"]));
    assert_eq!(observed["dns"]["available"], false);
    assert!(
        observed["dns"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("no DNS probe"))
    );

    // Desired settings are not here under any name: no `configured`
    // member, no interface kinds from the settings map, no static
    // addressing block. (`administrativeState: "configured"` is
    // networkd's word for its own state and is observed, not desired.)
    let text = observed.to_string();
    for desired in ["\"configured\":", "\"kind\":\"wireguard\"", "\"static\":"] {
        assert!(
            !text.contains(desired),
            "{desired} leaked into the observed surface"
        );
    }
}

/// Every rtnetlink enum on this surface is a NAME, and the id
/// it was resolved from travels beside it.
#[test]
pub(super) fn rtnetlink_enums_are_names_beside_the_ids_they_came_from() {
    let observed = observed_json(
        Ok(&describe_fixture()),
        &WifiEvidence::default(),
        None,
        &RadioEvidence::default(),
    );
    let route = &observed["defaultRoutes"]["entries"][0];
    assert_eq!(route["protocol"], "dhcp");
    assert_eq!(route["protocolId"], 16);
    assert_eq!(route["table"], "main");
    assert_eq!(route["tableId"], 254);

    let eth0 = &observed["interfaces"]["entries"][1];
    assert_eq!(eth0["addresses"][0]["scope"], "global");
    assert_eq!(eth0["addresses"][0]["scopeId"], 0);
    assert_eq!(eth0["addresses"][1]["scope"], "link");
    assert_eq!(eth0["addresses"][1]["scopeId"], 253);

    // A routing daemon's runtime-assigned protocol, a scope out of the
    // kernel's user-defined range, and a route table nothing named --
    // networkd spells all three as the decimal id, and none of the three
    // reaches the surface as a number.
    let unnamed = json!({
        "Interfaces": [{
            "Index": 2, "Name": "eth0",
            "Addresses": [{"Family": 2, "Address": [192,0,2,10], "PrefixLength": 24, "Scope": 77, "ScopeString": "77"}],
            "Routes": [{
                "Family": 2, "Destination": [0,0,0,0], "DestinationPrefixLength": 0,
                "Gateway": [192,0,2,1], "Protocol": 70, "ProtocolString": "70",
                "Table": 100, "TableString": "100"
            }]
        }]
    });
    let observed = observed_json(
        Ok(&unnamed),
        &WifiEvidence::default(),
        None,
        &RadioEvidence::default(),
    );
    let route = &observed["defaultRoutes"]["entries"][0];
    assert_eq!(route["protocol"], "unknown");
    assert_eq!(route["protocolId"], 70);
    assert_eq!(route["table"], "unknown");
    assert_eq!(route["tableId"], 100);
    let address = &observed["interfaces"]["entries"][0]["addresses"][0];
    assert_eq!(address["scope"], "unknown");
    assert_eq!(address["scopeId"], 77);

    // Absence stays absence: a route networkd described without the
    // numeric member gets no name invented for it.
    let bare = json!({
        "Interfaces": [{
            "Index": 2, "Name": "eth0",
            "Routes": [{"Family": 2, "Destination": [0,0,0,0], "DestinationPrefixLength": 0, "Gateway": [192,0,2,1]}]
        }]
    });
    let observed = observed_json(
        Ok(&bare),
        &WifiEvidence::default(),
        None,
        &RadioEvidence::default(),
    );
    let route = &observed["defaultRoutes"]["entries"][0];
    assert!(route.get("protocol").is_none());
    assert!(route.get("protocolId").is_none());
    assert!(route.get("table").is_none());
    assert!(route.get("tableId").is_none());
}

/// networkd absent: the interface and route members say so, the radios
/// are still reported, and nothing reads as healthy.
#[test]
pub(super) fn an_unanswering_networkd_is_absent_not_empty() {
    let radios = RadioEvidence {
        wifi_interfaces: vec!["wlan0".to_string()],
        ..RadioEvidence::default()
    };
    let observed = observed_json(
        Err("networkd observation timed out"),
        &WifiEvidence::default(),
        None,
        &radios,
    );
    assert_eq!(observed["interfaces"]["available"], false);
    assert!(
        observed["interfaces"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("timed out"))
    );
    assert_eq!(observed["defaultRoutes"]["available"], false);
    assert_eq!(observed["capabilities"]["wifi"]["supported"], true);
    assert_eq!(observed["wifi"]["available"], false);
    assert!(
        observed["wifi"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("control directory"))
    );
}

/// A DHCP lease inferred from the address list when networkd reports no
/// client object, marked as inferred.
#[test]
pub(super) fn a_lease_is_inferred_from_a_dhcp_sourced_address_and_says_so() {
    let mut describe = describe_fixture();
    describe["Interfaces"][1]
        .as_object_mut()
        .unwrap()
        .remove("DHCPv4Client");
    let observed = observed_json(
        Ok(&describe),
        &WifiEvidence::default(),
        None,
        &RadioEvidence::default(),
    );
    let dhcp = &observed["interfaces"]["entries"][1]["dhcp"];
    assert_eq!(dhcp["available"], true);
    assert_eq!(dhcp["inferred"], true);
    assert_eq!(dhcp["lease"]["address"], "192.0.2.10");
    assert_eq!(dhcp["lease"]["server"], "192.0.2.1");
}

/// No default route at all is real evidence: count zero, available.
#[test]
pub(super) fn no_default_route_is_reported_as_zero_not_absent() {
    let mut describe = describe_fixture();
    describe["Interfaces"][1]["Routes"] = json!([]);
    let observed = observed_json(
        Ok(&describe),
        &WifiEvidence::default(),
        None,
        &RadioEvidence::default(),
    );
    assert_eq!(observed["defaultRoutes"]["available"], true);
    assert_eq!(observed["defaultRoutes"]["count"], 0);
}
