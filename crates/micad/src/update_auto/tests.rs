use super::*;
use crate::time_status::SyncStatus;
use crate::update_policy::PolicyStore;
use chrono::Duration as Wall;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

mod anchors;
mod deferrals;
mod installs;

/// One thing the driver did, in the order it did it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Check,
    Fetch,
    Discard(String),
    Install(String),
    Reboot,
    Audit(String),
    Defer(String, String),
    Resume(Option<String>),
}

/// Scripted lifecycle: catalog selections and acquired descriptors follow
/// the same state transitions as the native service.
struct FakeDaemon {
    policy: PolicyStore,
    check: StdMutex<VecDeque<Result<Settled<Available>, Refusal>>>,
    fetch: StdMutex<VecDeque<Result<Settled<String>, Refusal>>>,
    available: StdMutex<Option<Available>>,
    staged: StdMutex<Option<String>>,
    facts: StdMutex<Option<UpdateFacts>>,
    install: StdMutex<Result<(), String>>,
    reboot: StdMutex<Result<(), String>>,
    clock: StdMutex<ClockTrust>,
    log: StdMutex<Vec<Call>>,
}

impl FakeDaemon {
    fn new(policy: PolicyStore) -> Arc<Self> {
        Arc::new(Self {
            policy,
            check: StdMutex::new(VecDeque::new()),
            fetch: StdMutex::new(VecDeque::new()),
            available: StdMutex::new(None),
            staged: StdMutex::new(None),
            facts: StdMutex::new(Some(UpdateFacts::default())),
            install: StdMutex::new(Ok(())),
            reboot: StdMutex::new(Ok(())),
            clock: StdMutex::new(trusted_clock()),
            log: StdMutex::new(Vec::new()),
        })
    }

    fn will_check(&self, answer: Result<Settled<Available>, Refusal>) {
        self.check.lock().expect("check").push_back(answer);
    }

    fn will_fetch(&self, answer: Result<Settled<String>, Refusal>) {
        self.fetch.lock().expect("fetch").push_back(answer);
    }

    fn set<T>(slot: &StdMutex<T>, value: T) {
        *slot.lock().expect("slot") = value;
    }

    fn calls(&self) -> Vec<Call> {
        self.log.lock().expect("log").clone()
    }

    fn installs(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Install(descriptor) => Some(descriptor),
                _ => None,
            })
            .collect()
    }

    /// Every deferral recorded so far, in order.
    fn deferrals(&self) -> Vec<(String, String)> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Defer(reason, detail) => Some((reason, detail)),
                _ => None,
            })
            .collect()
    }

    /// The one deferral this pass recorded. Panics on none or several: a
    /// test that meant one refusal and got two has learned something and
    /// should say so rather than pick the one it hoped for.
    fn the_deferral(&self) -> (String, String) {
        let deferrals = self.deferrals();
        assert_eq!(
            deferrals.len(),
            1,
            "expected exactly one deferral, got {deferrals:?}"
        );
        deferrals.into_iter().next().expect("one deferral")
    }

    fn log(&self, call: Call) {
        self.log.lock().expect("log").push(call);
    }
}

#[async_trait::async_trait]
impl AutoRoutes for FakeDaemon {
    fn policy(&self) -> LoadedPolicy {
        self.policy.load()
    }

    async fn check(&self, _sender: &str) -> Result<Settled<Available>, Refusal> {
        self.log(Call::Check);
        let answer = self
            .check
            .lock()
            .expect("check")
            .pop_front()
            .expect("the driver checked more times than this test scripted");
        // What `settle_check` records, so the step after this one reads
        // what a real daemon would have left behind.
        match &answer {
            Ok(Settled::Done(selected)) => Self::set(&self.available, Some(selected.clone())),
            Ok(Settled::NoneCompatible) => Self::set(&self.available, None),
            _ => {}
        }
        answer
    }

    async fn fetch(&self, _sender: &str) -> Result<Settled<String>, Refusal> {
        self.log(Call::Fetch);
        let answer = self
            .fetch
            .lock()
            .expect("fetch")
            .pop_front()
            .expect("the driver fetched more times than this test scripted");
        // What `settle_fetch` records.
        if let Ok(Settled::Done(path)) = &answer {
            Self::set(&self.staged, Some(path.clone()));
        }
        answer
    }

    async fn available(&self) -> Option<Available> {
        self.available.lock().expect("available").clone()
    }

    async fn staged(&self) -> Option<String> {
        self.staged.lock().expect("staged").clone()
    }

    async fn discard_staged(&self, why: &str) {
        self.log(Call::Discard(why.to_string()));
        Self::set(&self.staged, None);
    }

    async fn facts(&self) -> Option<UpdateFacts> {
        self.facts.lock().expect("facts").clone()
    }

    async fn install(&self, _sender: &str, descriptor: &str) -> Result<(), String> {
        self.log(Call::Install(descriptor.to_string()));
        self.install.lock().expect("install").clone()
    }

    async fn reboot(&self, _sender: &str) -> Result<(), String> {
        self.log(Call::Reboot);
        self.reboot.lock().expect("reboot").clone()
    }

    async fn clock(&self) -> ClockTrust {
        *self.clock.lock().expect("clock")
    }

    async fn audit(&self, event: &str) {
        self.log(Call::Audit(event.to_string()));
    }

    async fn defer(&self, reason: &str, detail: &str) {
        self.log(Call::Defer(reason.to_string(), detail.to_string()));
    }

    async fn resume(&self, only: Option<&str>) {
        self.log(Call::Resume(only.map(str::to_string)));
    }
}

/// A clock the device believes, by the first of the two limbs.
fn trusted_clock() -> ClockTrust {
    ClockTrust {
        status: Some(SyncStatus::Synchronized),
        floor_advanced: false,
    }
}

/// The case the clock rule is written for: `offline-degraded` with no advance.
fn untrusted_clock() -> ClockTrust {
    ClockTrust {
        status: Some(SyncStatus::OfflineDegraded),
        floor_advanced: false,
    }
}

/// `HH:MM` UTC, `offset` from now.
fn clock_face(offset: Wall) -> String {
    (Utc::now() + offset).format("%H:%M").to_string()
}

/// A window open right now: two hours either side of it, so the verdict
/// does not depend on the minute the suite happens to run in, and the
/// wrap-past-midnight arithmetic is exercised whenever it does run near
/// one.
fn open_window() -> String {
    format!(
        r#"{{"start": "{}", "end": "{}"}}"#,
        clock_face(-Wall::hours(2)),
        clock_face(Wall::hours(2))
    )
}

/// A window shut right now: it opens in two hours.
fn shut_window() -> String {
    format!(
        r#"{{"start": "{}", "end": "{}"}}"#,
        clock_face(Wall::hours(2)),
        clock_face(Wall::hours(4))
    )
}

/// The operator document an `auto` device carries.
fn auto_document(window: &str, reboot_policy: &str) -> String {
    format!(
        r#"{{"policy": "auto", "checkIntervalMinutes": 60,
             "rebootPolicy": "{reboot_policy}",
             "source": {{"url": "http://mirror/tuf"}},
             "maintenance": {{"windows": [{window}]}}}}"#
    )
}

/// The document of a device that names a time of day to check at.
fn anchored_document(at: &str, window: &str) -> String {
    format!(
        r#"{{"policy": "auto", "checkIntervalMinutes": 60, "checkAt": "{at}",
             "rebootPolicy": "manual",
             "source": {{"url": "http://mirror/tuf"}},
             "maintenance": {{"windows": [{window}]}}}}"#
    )
}

const BUNDLE: &str =
    "/mica/updates/verified/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json";

fn candidate(name: &str, version: &str) -> Available {
    Available {
        deployment_id: name
            .strip_suffix(".json")
            .expect("descriptor name")
            .to_string(),
        version: version.to_string(),
        core: false,
    }
}

/// The release the staged [`BUNDLE`] carries.
fn the_candidate() -> Available {
    candidate(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json",
        "1.5.0",
    )
}

/// One device: its documents on disk, its daemon, its cadence and the
/// driver ticking against all three.
struct Scene {
    dir: tempfile::TempDir,
    daemon: Arc<FakeDaemon>,
    cadence: Arc<TestCadence>,
    driver: AutoDriver,
}

impl Scene {
    /// An `auto` device with the window and reboot policy named, a
    /// believed clock, an answering deployment query and nothing staged.
    fn auto(window: &str, reboot_policy: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("updates.json");
        std::fs::write(&path, auto_document(window, reboot_policy))
            .expect("seed the policy document");
        let daemon = FakeDaemon::new(PolicyStore::at(path));
        let cadence = TestCadence::new();
        let routes: Arc<dyn AutoRoutes> = daemon.clone();
        let clock: Arc<dyn Cadence> = cadence.clone();
        let driver = AutoDriver::new(routes, clock);
        Self {
            dir,
            daemon,
            cadence,
            driver,
        }
    }

    /// Rewrite the document the store already points at. Every decision
    /// re-reads it, so this is how an operator's edit lands mid-run.
    fn rewrite(&self, window: &str, reboot_policy: &str) {
        std::fs::write(
            self.dir.path().join("updates.json"),
            auto_document(window, reboot_policy),
        )
        .expect("rewrite the policy document");
    }

    fn write_document(&self, body: &str) {
        std::fs::write(self.dir.path().join("updates.json"), body)
            .expect("rewrite the policy document");
    }

    async fn tick(&mut self) {
        self.driver.tick().await;
    }
}
