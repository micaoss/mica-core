use super::*;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

mod compat;
mod hostile;
mod pickup;

/// The dual-major recommendation, as a served set: two members, and
/// `current` is the *second* one. Used wherever a test needs the outgoing
/// major to be distinguishable from `current`.
const DUAL: [&str; 2] = ["v1", "v2"];
/// `current` for [`DUAL`]. The outgoing major is `v1`.
const DUAL_CURRENT: &str = "v2";

// Fixtures.

fn store() -> (tempfile::TempDir, Store) {
    fresh()
}

/// A journal-only audit sink, for the tests that drive `discover` itself.
fn journal_audit() -> Arc<crate::audit::Audit> {
    Arc::new(crate::audit::Audit::journal_only())
}

/// The same fixture under a second name, for the tests that need a second
/// store while the first is still bound to `store`.
fn fresh() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(dir.path().join("ui"));
    (dir, store)
}

/// Stage a minimal valid tree: one `index.html` and one asset.
fn stage(store: &Store, generation: u64) -> PathBuf {
    let staging = store.staging_dir(generation);
    fs::create_dir_all(staging.join("assets")).expect("create staging");
    fs::write(staging.join("index.html"), b"<!doctype html>").expect("write index");
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
        staging.join("mica-ui.json"),
        serde_json::to_vec(&manifest).expect("serialise manifest"),
    )
    .expect("write manifest");
}

/// Activate a bundle declaring `declared`, against `served`. `served` must
/// intersect `declared` or activation refuses — which is the activation
/// half of class 5 and is `bundle.rs`'s test, not this module's.
fn activate(store: &Store, generation: u64, declared: &[&str], served: &[&str]) {
    let staging = stage(store, generation);
    write_manifest(&staging, declared);
    store
        .activate(generation, served)
        .expect("activation must succeed");
}

// Log capture.

/// A subscriber that is interested in everything and does nothing with it,
/// registered once for the life of the test binary.
///
/// `tracing` caches each callsite's `Interest` the first time that
/// callsite is reached, computing it from the dispatchers registered at
/// that instant, and a cached `never` is never reconsidered. With only
/// scoped subscribers in the process there are instants with none: a
/// thread that reaches a callsite while holding no dispatcher of its own
/// gets `Interest::never()` cached for it, and every later event at that
/// callsite is dropped before any subscriber sees it -- including a
/// [`capture`] in flight on another thread, which then reads an empty log
/// about code that logged correctly. Keeping one always-interested
/// dispatcher registered for good makes `never` unreachable. It changes
/// nothing about where events go: `with_default` still routes them to the
/// capturing subscriber on the capturing thread, and they reach this one
/// only on threads that are not capturing, which drop them.
struct Interested;

impl tracing::Subscriber for Interested {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `f` with every `tracing` event going into a string.
fn capture<T>(f: impl FnOnce() -> T) -> (T, String) {
    // Before the scoped subscriber and before `f`, so that no callsite
    // reached from here on can be cached as uninteresting.
    static FLOOR: std::sync::Once = std::sync::Once::new();
    FLOOR.call_once(|| {
        let _ = tracing::subscriber::set_global_default(Interested);
    });

    let sink = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(sink.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let bytes = sink.0.lock().expect("log buffer").clone();
    (out, String::from_utf8_lossy(&bytes).into_owned())
}

/// The named-set assertion, applied to a log line: declare the fragments
/// expected by identity, observe which are present, and name what is
/// missing. Never a count.
fn names(log: &str, expected: &[&str], what: &str) {
    let missing: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|fragment| !log.contains(fragment))
        .collect();
    assert!(
        missing.is_empty(),
        "{what}: the log does not name {missing:?}\n--- log ---\n{log}"
    );
}

fn active(store: &Store) -> Option<u64> {
    store.active_generation().expect("active generation")
}

// The served-set constant.
