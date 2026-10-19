//! Declared Compose projects in the tree.

/// A device that declares no project writes `container.json` with the switch
/// alone.
#[test]
pub(super) fn a_device_with_no_projects_serializes_the_switch_alone() {
    let settings = micad_settings::Settings::default();
    let json = serde_json::to_value(&settings.container).unwrap();

    assert_eq!(json, serde_json::json!({ "enabled": false }));
}

// --- Compose projects --------------------------------------------------------

const WEB: &str = "services:\n  web:\n    image: docker.io/library/nginx:1\n";

/// A project is its Compose text under its name, and the tree carries it.
#[test]
pub(super) fn a_project_round_trips_through_the_tree() {
    let mut settings = micad_settings::Settings::default();
    settings
        .set("container.projects.app", serde_json::json!(WEB))
        .expect("a project is accepted");

    assert_eq!(settings.container.projects["app"], WEB);
    assert_eq!(
        settings.get("container.projects.app").unwrap(),
        serde_json::json!(WEB)
    );
}

/// The name is mica-containerd's rule, and the sizes keep one set under the
/// daemon's body bound.
#[test]
pub(super) fn a_project_set_the_daemon_could_not_take_is_refused() {
    let one = |name: &str, text: String| {
        micad_settings::validate_container_projects(&std::collections::BTreeMap::from([(
            name.to_string(),
            text,
        )]))
    };
    for name in ["app", "a", "0web", "my_app-2", &"a".repeat(40)] {
        assert!(one(name, WEB.to_string()).is_ok(), "{name}");
    }
    for name in [
        "",
        "App",
        "-app",
        "_app",
        "a.b",
        "a/b",
        "a b",
        &"a".repeat(41),
    ] {
        let error = one(name, WEB.to_string()).unwrap_err();
        assert!(error.contains("project name"), "{name}: {error}");
    }
    let error = one("app", "x".repeat(64 * 1024 + 1)).unwrap_err();
    assert!(error.contains("64 KiB"), "{error}");
    assert!(one("app", String::new()).unwrap_err().contains("empty"));

    let many: std::collections::BTreeMap<String, String> = (0..33)
        .map(|n| (format!("p{n}"), WEB.to_string()))
        .collect();
    assert!(
        micad_settings::validate_container_projects(&many)
            .unwrap_err()
            .contains("32")
    );
    let heavy: std::collections::BTreeMap<String, String> = (0..5)
        .map(|n| (format!("p{n}"), "x".repeat(60 * 1024)))
        .collect();
    assert!(
        micad_settings::validate_container_projects(&heavy)
            .unwrap_err()
            .contains("256 KiB")
    );
}

/// Each project is a file of its own beside `container.json`.
#[test]
pub(super) fn projects_are_files_of_their_own() {
    let dir = tempfile::tempdir().unwrap();
    let store = super::store_at(&dir);
    let mut settings = micad_settings::Settings::default();
    settings.container.enabled = true;
    settings
        .container
        .projects
        .insert("app".to_string(), WEB.to_string());
    settings
        .container
        .projects
        .insert("db".to_string(), "services: {}\n".to_string());
    store.save(&settings).unwrap();

    let projects = dir.path().join("config/container");
    assert_eq!(
        std::fs::read_to_string(projects.join("app.yaml")).unwrap(),
        WEB
    );
    assert_eq!(super::mode_of(&projects), 0o700);
    assert_eq!(super::mode_of(&projects.join("app.yaml")), 0o600);
    assert_eq!(
        super::config_document(&dir, micad_settings::CONTAINER_DOCUMENT),
        serde_json::json!({ "schema_version": 1, "container": { "enabled": true } })
    );
    assert_eq!(store.load().unwrap(), settings);

    // A project removed from the tree is removed from the directory.
    settings.container.projects.remove("db");
    store.save(&settings).unwrap();
    assert!(!projects.join("db.yaml").exists());
    assert_eq!(store.load().unwrap(), settings);
}

/// A file not named as a project is not one, and a project file that is not
/// text refuses the `container` subtree like any document that does not parse.
#[test]
pub(super) fn the_project_directory_is_read_by_its_names() {
    let dir = tempfile::tempdir().unwrap();
    let store = super::store_at(&dir);
    let projects = dir.path().join("config/container");
    std::fs::create_dir(&projects).unwrap();
    std::fs::write(projects.join("app.yaml"), WEB).unwrap();
    for ignored in ["App.yaml", "notes.txt", "app.yml", ".app.yaml.tmp"] {
        std::fs::write(projects.join(ignored), "x").unwrap();
    }
    let settings = store.load().unwrap();
    assert_eq!(
        settings.container.projects,
        std::collections::BTreeMap::from([("app".to_string(), WEB.to_string())])
    );

    std::fs::write(projects.join("bad.yaml"), [0xff, 0xfe]).unwrap();
    let loaded = store.load_with_refusals().unwrap();
    assert_eq!(loaded.refusals.len(), 1);
    assert_eq!(loaded.refusals[0].document, "container/bad.yaml");
    assert_eq!(loaded.refusals[0].subtrees, ["container"]);
    assert!(loaded.settings.container.projects.is_empty());
    assert!(store.load().is_err());
}

/// A `container.json` that still declares containers of its own (`units`) is
/// a document this build does not read: the `container` subtree is refused,
/// micad still starts, and a write of `container` replaces it.
#[test]
pub(super) fn a_container_document_with_units_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = super::store_at(&dir);
    std::fs::create_dir_all(dir.path().join("config")).unwrap();
    std::fs::write(
        dir.path()
            .join("config")
            .join(micad_settings::CONTAINER_DOCUMENT),
        r#"{"schema_version":1,"container":{"enabled":true,"units":{"web":{"image":"nginx:1"}}}}"#,
    )
    .unwrap();

    let loaded = store.load_with_refusals().unwrap();
    assert_eq!(loaded.refusals.len(), 1);
    assert_eq!(
        loaded.refusals[0].document,
        micad_settings::CONTAINER_DOCUMENT
    );
    assert_eq!(loaded.refusals[0].subtrees, ["container"]);
    assert!(!loaded.settings.container.enabled);
    assert!(store.load().is_err());
}

/// A project's directory is kept in `container.json` beside the switch.
#[test]
pub(super) fn project_directories_are_kept_in_the_container_document() {
    let dir = tempfile::tempdir().unwrap();
    let store = super::store_at(&dir);
    let mut settings = micad_settings::Settings::default();
    settings
        .set(
            "container",
            serde_json::json!({
                "enabled": true,
                "projects": { "app": WEB, "site": WEB },
                "projectDirs": { "site": "/srv/site" },
            }),
        )
        .expect("a project directory is accepted");
    store.save(&settings).unwrap();

    assert_eq!(
        super::config_document(&dir, micad_settings::CONTAINER_DOCUMENT),
        serde_json::json!({ "schema_version": 1, "container": {
            "enabled": true,
            "projectDirs": { "site": "/srv/site" },
        } })
    );
    assert_eq!(store.load().unwrap(), settings);
}

/// A project directory is an absolute path of a declared project, and goes
/// with its project.
#[test]
pub(super) fn a_project_directory_is_absolute_and_goes_with_its_project() {
    let mut settings = micad_settings::Settings::default();
    for (dirs, refusal) in [
        (serde_json::json!({ "app": "srv/app" }), "absolute path"),
        (
            serde_json::json!({ "other": "/srv/other" }),
            "not a declared project",
        ),
    ] {
        let error = settings
            .set(
                "container",
                serde_json::json!({ "projects": { "app": WEB }, "projectDirs": dirs }),
            )
            .unwrap_err();
        assert!(error.to_string().contains(refusal), "{error}");
    }

    settings
        .set(
            "container",
            serde_json::json!({ "projects": { "app": WEB, "db": WEB }, "projectDirs": { "app": "/srv/app" } }),
        )
        .unwrap();
    settings
        .set(
            "container.projects.app",
            serde_json::json!(format!("{WEB}# edited\n")),
        )
        .unwrap();
    assert_eq!(settings.container.project_dirs["app"], "/srv/app");
    settings
        .set("container.projects", serde_json::json!({ "db": WEB }))
        .expect("a project is removed with its directory");
    assert!(settings.container.project_dirs.is_empty());
}
