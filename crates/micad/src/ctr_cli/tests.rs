use super::*;

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn every_command_parses_and_a_misuse_is_named() {
    assert_eq!(parse(&words("up")), Ok(Command::Up(Target::default())));
    assert_eq!(
        parse(&words("up -p app -f app.yaml --project-dir /srv/app")),
        Ok(Command::Up(Target {
            project: Some("app".into()),
            file: Some("app.yaml".into()),
            dir: Some("/srv/app".into()),
        }))
    );
    assert_eq!(
        parse(&words("down --project app")),
        Ok(Command::Down(Target {
            project: Some("app".into()),
            ..Target::default()
        }))
    );
    assert_eq!(parse(&words("list")), Ok(Command::List));
    assert_eq!(parse(&words("ps")), Ok(Command::List));
    assert_eq!(
        parse(&words("check -f -")),
        Ok(Command::Check(Target {
            file: Some("-".into()),
            ..Target::default()
        }))
    );
    assert_eq!(
        parse(&words("restart app/web")),
        Ok(Command::Verb {
            verb: Verb::Restart,
            name: "app/web".into()
        })
    );
    assert_eq!(
        parse(&words("logs app/web --tail 20")),
        Ok(Command::Logs {
            name: "app/web".into(),
            tail: 20
        })
    );
    assert_eq!(
        parse(&words("logs app/web")),
        Ok(Command::Logs {
            name: "app/web".into(),
            tail: DEFAULT_TAIL
        })
    );
    assert_eq!(parse(&[]), Ok(Command::Help));
    for misuse in [
        "up -p App",
        "up -f",
        "up -p a -p b",
        "up app.yaml",
        "down -f app.yaml",
        "put app app.yaml",
        "delete app",
        "logs app",
        "logs app/web --tail 0",
        "logs app/web --tail 1001",
        "frobnicate",
        "list extra",
    ] {
        assert!(parse(&words(misuse)).is_err(), "{misuse}");
    }
}

const FILE: &str = "services:\n  web:\n    image: a:1\n    volumes: [./data:/data]\n";

// The file is the working directory's, by docker compose's names in order;
// the project is the directory's name made a project name, and the project
// directory is the file's.
#[test]
fn up_in_a_project_directory_takes_compose_defaults() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("Bit-56_Core");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("docker-compose.yml"), "services: {}\n").unwrap();
    std::fs::write(cwd.join("compose.yml"), FILE).unwrap();

    let found = resolve(&Target::default(), &cwd, &mut std::io::empty()).unwrap();

    assert_eq!(
        found,
        Found {
            name: "bit-56_core".into(),
            dir: cwd.clone(),
            text: FILE.into(),
        }
    );
    assert_eq!(
        down_project(&Target::default(), &cwd).unwrap(),
        "bit-56_core"
    );
}

// The file's name key names the project; -p overrides it, -f names another
// file whose directory is the project's, and --project-dir another directory.
#[test]
fn the_name_key_and_the_flags_override_the_defaults() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("work");
    let site = root.path().join("site");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::create_dir(&site).unwrap();
    let named = format!("name: \"shop\" # the shop\n{FILE}");
    std::fs::write(cwd.join("compose.yaml"), &named).unwrap();
    std::fs::write(site.join("site.yaml"), FILE).unwrap();

    let found = resolve(&Target::default(), &cwd, &mut std::io::empty()).unwrap();
    assert_eq!(
        (found.name.as_str(), found.dir.as_path()),
        ("shop", cwd.as_path())
    );
    assert_eq!(down_project(&Target::default(), &cwd).unwrap(), "shop");

    let other = Target {
        file: Some("../site/./site.yaml".into()),
        ..Target::default()
    };
    let found = resolve(&other, &cwd, &mut std::io::empty()).unwrap();
    assert_eq!(
        (found.name.as_str(), found.dir.as_path()),
        ("site", site.as_path())
    );

    let overridden = Target {
        project: Some("web".into()),
        dir: Some("../site".into()),
        ..Target::default()
    };
    let found = resolve(&overridden, &cwd, &mut std::io::empty()).unwrap();
    assert_eq!(
        (found.name.as_str(), found.dir.as_path()),
        ("web", site.as_path())
    );
}

// stdin's file has the working directory; a directory without a file, a
// --project-dir that is not one and a name that is no project name are
// refused.
#[test]
fn stdin_and_what_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("app");
    std::fs::create_dir(&cwd).unwrap();
    let stdin = Target {
        file: Some("-".into()),
        ..Target::default()
    };
    let found = resolve(&stdin, &cwd, &mut FILE.as_bytes()).unwrap();
    assert_eq!(
        (found.name.as_str(), found.dir.as_path()),
        ("app", cwd.as_path())
    );

    let error = resolve(&Target::default(), &cwd, &mut std::io::empty()).unwrap_err();
    assert!(error.to_string().contains("no Compose file"), "{error}");
    let missing = Target {
        dir: Some("absent".into()),
        ..stdin
    };
    assert!(resolve(&missing, &cwd, &mut FILE.as_bytes()).is_err());

    let odd = root.path().join("__");
    std::fs::create_dir(&odd).unwrap();
    assert!(down_project(&Target::default(), &odd).is_err());
    assert_eq!(name_key("name: 'a b'\n").as_deref(), Some("a b"));
    assert!(default_project("name: 'a b'\n", &cwd).is_err());
}

#[test]
fn a_refusal_is_one_problem_a_line() {
    let outcome = json!({
        "ok": false,
        "detail": "web: privileged is not supported; name differs",
        "problems": [
            { "service": "web", "field": "privileged", "detail": "privileged is not supported" },
            { "service": "", "field": "name", "detail": "name differs" },
        ],
    });
    assert_eq!(
        render_problems("app", &outcome),
        "mica-ctr: project app is refused\n  web.privileged: privileged is not supported\n  name: name differs\n"
    );
}

#[test]
fn the_list_is_every_projects_containers() {
    let doc = json!({
        "enabled": true,
        "projects": { "app": "x", "idle": "y" },
        "projectDirs": { "app": "/srv/app" },
        "engine": {
            "available": true,
            "projects": [{ "name": "app", "services": [
                { "service": "web", "spec": { "name": "app-web-1" }, "phase": "running", "health": "healthy" },
            ] }],
        },
    });
    assert_eq!(
        render_list(&doc),
        "\
PROJECT  SERVICE  CONTAINER  PHASE        HEALTH
app      web      app-web-1  running      healthy
idle     -        -          not running  -
"
    );
    let off = json!({ "enabled": false, "projects": {}, "engine": { "available": false } });
    assert!(render_list(&off).starts_with("containers are off"));
}

/// On docker the list is every container dockerd holds: nothing is declared.
#[test]
fn on_docker_the_list_is_what_dockerd_holds() {
    let doc = json!({
        "enabled": true,
        "containerEngine": "docker",
        "projects": {},
        "engine": {
            "available": true,
            "projects": [
                { "name": "app", "services": [
                    { "service": "web", "spec": { "name": "app-web-1" }, "phase": "running", "health": "" },
                ] },
                { "name": "portainer", "services": [
                    { "service": "portainer", "spec": { "name": "portainer" }, "phase": "exited", "health": "" },
                ] },
            ],
        },
    });
    assert_eq!(
        render_list(&doc),
        "\
PROJECT    SERVICE    CONTAINER  PHASE    HEALTH
app        web        app-web-1  running  -
portainer  portainer  portainer  exited   -
"
    );
    let down = json!({ "enabled": true, "containerEngine": "docker", "projects": {},
        "engine": { "available": false, "detail": "not answering" } });
    assert!(render_list(&down).starts_with("dockerd is not answering"));
    let off =
        json!({ "enabled": false, "containerEngine": "docker", "projects": {}, "engine": {} });
    assert!(render_list(&off).starts_with("containers are off: `container.enabled`"));
}
