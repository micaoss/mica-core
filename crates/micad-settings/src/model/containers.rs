//! Declared Compose projects and their rules.

use std::collections::BTreeMap;

/// Container engine policy, reconciled by `ContainerReconciler`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerSettings {
    /// Whether mica-containerd runs and the declared projects with it.
    pub enabled: bool,
    /// The Compose projects this device runs: each name to its Compose text.
    ///
    /// Stored as files of their own (`container/<project>.yaml`), not in
    /// `container.json`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, String>,
    /// Each project's directory, where mica-containerd resolves its relative
    /// host paths; a project without one has `/mica/volumes/<project>/`.
    #[serde(
        default,
        rename = "projectDirs",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub project_dirs: BTreeMap<String, String>,
}

/// The most projects one device declares.
pub const MAX_CONTAINER_PROJECTS: usize = 32;
/// The largest one project's Compose text may be.
pub const MAX_CONTAINER_PROJECT_BYTES: usize = 64 * 1024;
/// The largest every project's Compose text may be together: what keeps one
/// set, JSON-escaped, under mica-containerd's 1 MiB body.
pub const MAX_CONTAINER_PROJECTS_BYTES: usize = 256 * 1024;
/// The longest a project directory may be.
const MAX_PROJECT_DIR_LEN: usize = 4096;
/// The longest a project name may be.
const MAX_PROJECT_NAME_LEN: usize = 40;

/// Whether `name` is a project name mica-containerd takes: 1 to 40 of
/// `[a-z0-9_-]`, starting with a letter or a digit.
#[must_use]
pub fn is_project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PROJECT_NAME_LEN
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// Refuse a project set mica-containerd could not be handed: a name it does
/// not take, an empty text, or more or larger texts than one set carries.
/// Whether each text is a Compose file it runs is its own check's to say.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_container_projects(projects: &BTreeMap<String, String>) -> Result<(), String> {
    if projects.len() > MAX_CONTAINER_PROJECTS {
        return Err(format!(
            "{} projects are declared; a device declares at most {MAX_CONTAINER_PROJECTS}",
            projects.len()
        ));
    }
    let mut total = 0;
    for (name, text) in projects {
        if !is_project_name(name) {
            return Err(format!(
                "project name {name:?} is not 1 to {MAX_PROJECT_NAME_LEN} of a-z, 0-9, `_` and `-`, starting with a letter or a digit"
            ));
        }
        if text.trim().is_empty() {
            return Err(format!("project {name:?} is empty"));
        }
        if text.len() > MAX_CONTAINER_PROJECT_BYTES {
            return Err(format!(
                "project {name:?} is {} bytes; a project is at most 64 KiB",
                text.len()
            ));
        }
        total += text.len();
    }
    if total > MAX_CONTAINER_PROJECTS_BYTES {
        return Err(format!(
            "the projects are {total} bytes together; together they are at most 256 KiB"
        ));
    }
    Ok(())
}

/// Refuse a project directory mica-containerd would not take, or one of a
/// project that is not declared.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_container_project_dirs(container: &ContainerSettings) -> Result<(), String> {
    for (name, dir) in &container.project_dirs {
        if !container.projects.contains_key(name) {
            return Err(format!(
                "projectDirs names {name:?}, which is not a declared project"
            ));
        }
        if !dir.starts_with('/') || dir.len() > MAX_PROJECT_DIR_LEN || dir.contains('\0') {
            return Err(format!(
                "the directory of project {name:?} is not an absolute path of at most {MAX_PROJECT_DIR_LEN} bytes"
            ));
        }
    }
    Ok(())
}
