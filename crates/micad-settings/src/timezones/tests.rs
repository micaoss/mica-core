use super::*;

/// A tzdata tree: `Europe/Berlin`, `America/Argentina/Buenos_Aires` and the
/// alias `Arctic/Longyearbyen` as zone files, a table naming the first two
/// (one of them twice, and one zone the tree lacks), and a version.
fn tzdata(dir: &Path) {
    for zone in [
        "Europe/Berlin",
        "America/Argentina/Buenos_Aires",
        "Arctic/Longyearbyen",
    ] {
        let path = dir.join(zone);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "TZif2").unwrap();
    }
    std::fs::write(dir.join("UTC"), "TZif2").unwrap();
    std::fs::write(
        dir.join("zone1970.tab"),
        "# tzdb timezone descriptions\n\
         DE,DK,NO,SE,SJ\t+5230+01322\tEurope/Berlin\tmost of Germany\n\
         AR\t-3436-05827\tAmerica/Argentina/Buenos_Aires\tBuenos Aires (BA, CF)\n\
         XX\t+0000+00000\tAtlantis/Made_Up\n\
         DE\t+5230+01322\tEurope/Berlin\n",
    )
    .unwrap();
    std::fs::write(dir.join("tzdata.zi"), "# version 2026c\n# a comment\n").unwrap();
}

#[test]
fn the_catalog_is_utc_then_the_canonical_installed_zones() {
    let dir = tempfile::tempdir().unwrap();
    tzdata(dir.path());
    let catalog = zone_catalog(dir.path()).unwrap();
    assert_eq!(catalog.tzdata_version.as_deref(), Some("2026c"));
    let zone = |id: &str, region: Option<&str>, city: Option<&str>| Zone {
        id: id.into(),
        region: region.map(Into::into),
        city: city.map(Into::into),
    };
    assert_eq!(
        catalog.zones,
        vec![
            zone("UTC", None, None),
            zone(
                "America/Argentina/Buenos_Aires",
                Some("America"),
                Some("Argentina/Buenos_Aires")
            ),
            zone("Europe/Berlin", Some("Europe"), Some("Berlin")),
        ]
    );
    // An alias the table does not name is not offered, and is still a zone.
    assert!(zone_installed(dir.path(), "Arctic/Longyearbyen"));
}

#[test]
fn zone_tab_stands_in_and_a_tzdata_without_either_table_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    tzdata(dir.path());
    std::fs::rename(dir.path().join("zone1970.tab"), dir.path().join("zone.tab")).unwrap();
    std::fs::remove_file(dir.path().join("tzdata.zi")).unwrap();
    let catalog = zone_catalog(dir.path()).unwrap();
    assert_eq!(catalog.zones.len(), 3);
    assert_eq!(catalog.tzdata_version, None);

    std::fs::remove_file(dir.path().join("zone.tab")).unwrap();
    assert!(zone_catalog(dir.path()).is_err());
}

#[test]
fn only_a_tzif_file_under_the_tree_is_a_zone() {
    let dir = tempfile::tempdir().unwrap();
    tzdata(dir.path());
    assert!(zone_installed(dir.path(), "UTC"));
    assert!(zone_installed(dir.path(), "Europe/Berlin"));
    for refused in [
        "zone1970.tab",
        "tzdata.zi",
        "Atlantis/Made_Up",
        "../etc/passwd",
        "",
    ] {
        assert!(!zone_installed(dir.path(), refused), "{refused}");
    }
}
