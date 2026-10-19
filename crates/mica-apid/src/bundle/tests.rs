use super::*;

mod compat;
mod integrity;
mod selection;
mod trees;

/// The served set while two majors are served. Passed in rather than looked
/// up: version negotiation is not implemented and this module must not
/// invent the set.
const SERVED: [&str; 2] = ["v1", "v2"];

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(dir.path().join("ui"));
    (dir, store)
}

/// Stage a minimal valid tree: one `index.html` and one asset.
fn stage_valid(store: &Store, generation: u64) -> PathBuf {
    let staging = store.staging_dir(generation);
    fs::create_dir_all(staging.join("assets")).expect("create staging");
    fs::write(staging.join(INDEX_NAME), b"<!doctype html>").expect("write index");
    fs::write(staging.join("assets/app.js"), b"console.log(1)").expect("write asset");
    staging
}

fn write_manifest(staging: &Path, api_versions: &[&str]) {
    let manifest = serde_json::json!({
        "name": "demo",
        "version": "1.2.3",
        "immutableDir": "assets",
        "apiVersions": api_versions,
    });
    fs::write(
        staging.join(MANIFEST_NAME),
        serde_json::to_vec(&manifest).expect("serialise manifest"),
    )
    .expect("write manifest");
}

fn rejection(err: &anyhow::Error) -> Rejection {
    err.downcast_ref::<Rejection>()
        .unwrap_or_else(|| panic!("expected a Rejection, got: {err:#}"))
        .clone()
}

fn custom(installed: &Installed) -> &CustomUi {
    match installed {
        Installed::Custom(ui) => ui,
        Installed::BuiltIn => panic!("expected an active bundle"),
    }
}
