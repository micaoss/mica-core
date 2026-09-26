//! The built-in console, served from the directory the `mica-apid-ui`
//! package installs.
//!
//! The console is not compiled into apid: a product that wants the API and not
//! the console leaves the package out, and then `/_ui` answers 404 and nothing
//! else changes. The directory is in the dm-verity root, so what is served is
//! as authenticated as the embedded tree it replaces.
//!
//! The tree is indexed once, when apid starts: regular files only, every name a
//! safe logical path, bounded in count and size. A request is answered from the
//! index and never resolved against the filesystem, so no request can name a
//! file the index did not admit.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;

use super::mime::{self, CacheClass, NOSNIFF, X_CONTENT_TYPE_OPTIONS};
use super::path::LogicalPath;
use crate::routes::AppState;

/// Where the `mica-apid-ui` package installs the console.
pub const DEFAULT_DIR: &str = "/usr/share/mica-apid/ui";

/// Bounds on the tree: far above a console, far below what could exhaust a
/// device.
const MAX_FILES: usize = 4096;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// One admitted file: its logical path and the length it had when indexed.
#[derive(Debug)]
struct Asset {
    path: String,
    bytes: u64,
}

/// The indexed console.
#[derive(Debug)]
pub struct Builtin {
    root: PathBuf,
    assets: Vec<Asset>,
}

impl Builtin {
    /// Index the console under `root`: `Ok(None)` when no console is
    /// installed, an error when the tree breaks a rule.
    ///
    /// # Errors
    ///
    /// A symlink or special file, a name that is not a safe logical path, a
    /// tree over its bounds, or no `index.html`.
    pub fn load(root: &Path) -> anyhow::Result<Option<Self>> {
        match fs::symlink_metadata(root) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => anyhow::bail!("{} is not a directory", root.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        }
        let mut assets = Vec::new();
        let mut total = 0_u64;
        walk(root, root, &mut assets, &mut total)?;
        assets.sort_by(|a, b| a.path.cmp(&b.path));
        anyhow::ensure!(
            assets.iter().any(|asset| asset.path == "index.html"),
            "the console at {} has no index.html",
            root.display()
        );
        Ok(Some(Self {
            root: root.to_path_buf(),
            assets,
        }))
    }

    /// Every admitted logical path, sorted.
    #[cfg(test)]
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.assets.iter().map(|asset| asset.path.as_str())
    }

    fn lookup(&self, path: &str) -> Option<&Asset> {
        self.assets
            .binary_search_by(|asset| asset.path.as_str().cmp(path))
            .ok()
            .map(|index| &self.assets[index])
    }

    /// The bytes of an admitted file, read no further than its indexed length.
    fn read(&self, asset: &Asset) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        fs::File::open(self.root.join(&asset.path))
            .ok()?
            .take(asset.bytes)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 == asset.bytes).then_some(bytes)
    }
}

fn walk(root: &Path, dir: &Path, assets: &mut Vec<Asset>, total: &mut u64) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            walk(root, &path, assets, total)?;
            continue;
        }
        anyhow::ensure!(meta.is_file(), "{} is not a regular file", path.display());
        let logical = path
            .strip_prefix(root)?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("{} is not UTF-8", path.display()))?
            .to_owned();
        anyhow::ensure!(
            LogicalPath::parse(&logical).is_ok(),
            "{logical} is not a safe logical path"
        );
        *total += meta.len();
        anyhow::ensure!(
            assets.len() < MAX_FILES && meta.len() <= MAX_FILE_BYTES && *total <= MAX_TOTAL_BYTES,
            "the console at {} exceeds its bounds",
            root.display()
        );
        assets.push(Asset {
            path: logical,
            bytes: meta.len(),
        });
    }
    Ok(())
}

const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; font-src 'self'; style-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

/// Stable built-in SPA entry at both `/_ui` and `/_ui/`.
pub async fn index(State(state): State<AppState>) -> Response {
    let Some(builtin) = state.builtin() else {
        return not_found();
    };
    builtin
        .lookup("index.html")
        .and_then(|asset| asset_response(builtin, asset))
        .unwrap_or_else(internal_error)
}

/// One path below the already-selected `/_ui` namespace.
pub async fn serve(State(state): State<AppState>, OriginalUri(uri): OriginalUri) -> Response {
    let Some(builtin) = state.builtin() else {
        return not_found();
    };
    let Some(relative) = uri.path().strip_prefix("/_ui/") else {
        return not_found();
    };
    let logical = match LogicalPath::parse(relative) {
        Ok(logical) => logical,
        Err(_) => return not_found(),
    };
    if let Some(asset) = builtin.lookup(logical.as_str()) {
        return asset_response(builtin, asset).unwrap_or_else(internal_error);
    }

    let route_like = logical
        .as_str()
        .rsplit('/')
        .next()
        .is_some_and(|segment| !segment.contains('.'));
    if route_like {
        return index(State(state)).await;
    }
    not_found()
}

fn asset_response(builtin: &Builtin, asset: &Asset) -> Option<Response> {
    let cache = if asset.path == "index.html" {
        CacheClass::NoStore
    } else if asset.path.starts_with("assets/") {
        CacheClass::Immutable
    } else {
        CacheClass::NoCache
    };
    Some(response(
        StatusCode::OK,
        Body::from(builtin.read(asset)?),
        Some(mime::content_type(Path::new(&asset.path))),
        cache,
    ))
}

fn not_found() -> Response {
    response(
        StatusCode::NOT_FOUND,
        Body::empty(),
        None,
        CacheClass::NoCache,
    )
}

fn internal_error() -> Response {
    response(
        StatusCode::INTERNAL_SERVER_ERROR,
        Body::empty(),
        None,
        CacheClass::NoStore,
    )
}

fn response(
    status: StatusCode,
    body: Body,
    content_type: Option<&'static str>,
    cache: CacheClass,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    if let Some(content_type) = content_type {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(cache.header_value()),
    );
    headers.insert(
        HeaderName::from_static(X_CONTENT_TYPE_OPTIONS),
        HeaderValue::from_static(NOSNIFF),
    );
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    response
}
