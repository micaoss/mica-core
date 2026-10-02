//! Automatic signed deployment acquisition and installation.
//! Checks and downloads share the operator policy. Installation rechecks the
//! catalog inside the maintenance window, and reboot uses the shared health gate.
//! Native failed IDs and generation floors prevent reinstalling rejected releases.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use micad_settings::{UPDATE_CHECK_EVENT, UPDATE_FETCH_EVENT, UPDATE_INSTALL_EVENT};

use crate::time_status::ClockTrust;
use crate::update_codes;
use crate::update_lifecycle::{Available, Refusal, Settled};
use crate::update_policy::{self, LoadedPolicy, RebootPolicy, UpdateMode};

/// Who the driver's actions are attributed to, wherever an operator's bus
/// name would be. One name, so an audit reading `requested_by` can tell a
/// machine's install from a human's without having to infer it.
pub const SENDER: &str = "auto-update";

/// How often the driver looks at the world.
///
/// A maintenance window is `HH:MM`-precise and may be a single minute long,
/// so a driver that slept longer could not honestly claim to act *inside*
/// one. A tick with nothing to do costs one policy-file read.
const TICK: Duration = Duration::from_secs(60);

/// The daemon facts the driver reads that are not the lifecycle's own. One
/// value because they come from one the native backend query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateFacts {
    /// A deployment is installed and activated and has not booted yet.
    pub reboot_pending: bool,
    /// `update.install.status` as the install route records it: `running`,
    /// `done` or `failed`. `None` when nothing has installed anything.
    pub install_status: Option<String>,
}

/// Everything the automatic driver can do, and nothing else.
#[async_trait::async_trait]
pub trait AutoRoutes: Send + Sync {
    /// The policy document, loaded fresh. Every decision re-reads it, so an
    /// operator's edit takes effect on the next tick with no restart.
    fn policy(&self) -> LoadedPolicy;

    /// `CheckUpdate`, awaited to its outcome.
    async fn check(&self, sender: &str) -> Result<Settled<Available>, Refusal>;

    /// `FetchUpdate`, awaited to its outcome.
    async fn fetch(&self, sender: &str) -> Result<Settled<String>, Refusal>;

    /// The candidate the last check selected.
    async fn available(&self) -> Option<Available>;

    /// The verified descriptor the last fetch staged.
    async fn staged(&self) -> Option<String>;

    /// Delete a staged descriptor the current metadata no longer names, and
    /// forget it.
    async fn discard_staged(&self, why: &str);

    /// Candidate and installation facts from one native backend query.
    /// `None` when the query did not answer — which is not "nothing is
    /// pending", so the driver defers rather than proceeding on a guess.
    async fn facts(&self) -> Option<UpdateFacts>;

    /// `InstallUpdate`, with its own gates; the error is what it answers an
    /// operator.
    async fn install(&self, sender: &str, descriptor: &str) -> Result<(), String>;

    /// `Reboot`, honouring the safe-to-reboot gate; the error is the gate's
    /// refusal, verbatim.
    async fn reboot(&self, sender: &str) -> Result<(), String>;

    /// The two signals the clock predicate reads.
    async fn clock(&self) -> ClockTrust;

    /// Record one automatic action in the device's audit ring.
    async fn audit(&self, event: &str);

    /// Record why this pass did not proceed.
    async fn defer(&self, reason: &str, detail: &str);

    /// Forget a recorded deferral; `only` clears just that reason.
    async fn resume(&self, only: Option<&str>);
}

/// Where the driver is between ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Nothing of the driver's is in flight.
    Idle,
    /// The driver asked for an install and is waiting for the native backend to finish.
    Installing,
    /// The driver's install finished; a reboot is owed under
    /// [`RebootPolicy::Window`] as soon as the window and the gate allow.
    RebootPending,
}

/// The driver's cadence clock, and the two things it is allowed to answer.
pub trait Cadence: Send + Sync {
    /// The monotonic now, which every interval is measured on.
    fn now(&self) -> Instant;

    /// The wall now, which only the check anchor reads.
    ///
    /// Separate from [`Self::now`] on purpose: an interval must not move when
    /// the clock is set, and a time of day cannot be answered without it.
    /// Read only after [`ClockTrust::believed`], so a device that does not
    /// believe its clock never schedules on one.
    fn wall(&self) -> chrono::DateTime<Utc> {
        Utc::now()
    }
}

/// The production cadence: the machine's own monotonic clock.
pub struct SystemCadence;

impl Cadence for SystemCadence {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The driver's own state between ticks.
pub struct AutoDriver {
    routes: Arc<dyn AutoRoutes>,
    cadence: Arc<dyn Cadence>,
    /// When a check was last attempted. Attempt-based rather than
    /// success-based: a check refused by policy must not retry every tick,
    /// and the cadence an operator set is a cadence of attempts.
    last_check: Instant,
    /// The last time the anchor came round that this driver counts as
    /// answered: its start, then each anchored check it makes.
    ///
    /// Seeded with the start so an anchor that passed while the device was
    /// down does not fire at boot -- a device rebooting hourly under a daily
    /// anchor would otherwise check hourly, which is what the interval
    /// already guards against.
    anchor_floor: chrono::DateTime<Utc>,
    stage: Stage,
}

/// Run the driver forever. One task, spawned by `main` on a production
/// daemon only.
pub async fn run(routes: Arc<dyn AutoRoutes>) {
    let mut driver = AutoDriver::new(routes, Arc::new(SystemCadence));
    loop {
        tokio::time::sleep(TICK).await;
        driver.tick().await;
    }
}

impl AutoDriver {
    pub fn new(routes: Arc<dyn AutoRoutes>, cadence: Arc<dyn Cadence>) -> Self {
        Self {
            routes,
            // The first check falls one interval after start, which is what
            // the cadence this replaces did by sleeping before its first
            // check: a device that reboots hourly must not check hourly.
            last_check: cadence.now(),
            anchor_floor: cadence.wall(),
            cadence,
            stage: Stage::Idle,
        }
    }

    /// One turn of the driver.
    pub async fn tick(&mut self) {
        let loaded = self.routes.policy();
        let Some(selection) = loaded.policy.selection.as_ref() else {
            // A document that exists and does not parse refuses every
            // restricted action already, and the load answers with NO
            // selection beside the error — so there is no mode to read, which
            // is stronger than declining to read one: there is no
            // falling back to the baked source here, and there is nothing
            // here to fall back to. The device initiates nothing until the
            // file is fixed; the operator's manual routes still refuse with
            // the error naming the file.
            return;
        };
        match selection.mode {
            UpdateMode::Off => {
                // No timer arms, and an owed automatic reboot is dropped
                // rather than carried: the operator has just said the device
                // initiates nothing. A pending deployment stays pending and a human
                // reboots into it.
                self.stage = Stage::Idle;
            }
            UpdateMode::Check => {
                self.check_if_due(&loaded).await;
            }
            UpdateMode::Auto => {
                if let Some(reason) = loaded.policy.auto_window_refusal() {
                    // `auto` inherited from the baked default over a document
                    // that names no window. The document-local case is a load
                    // error (`configuration::validate`); this is the case
                    // precedence creates, and it refuses the automatic
                    // install only — the check cadence and every manual route
                    // keep working.
                    tracing::warn!(reason, "automatic install refused");
                    self.check_if_due(&loaded).await;
                    return;
                }
                self.drive(&loaded).await;
            }
        }
    }

    /// The anchor's last crossing, or `None` when this device checks on the
    /// interval instead.
    ///
    /// `None` for all three ways an anchor can fail to be one: the document
    /// names none, the clock is not one this device believes, or the string
    /// is not a clock face. **The interval is the fallback, never a refusal**:
    /// a device whose clock never synchronises must keep discovering updates,
    /// which is the same reason checks and fetches are unaffected by the
    /// clock gate that stops installs.
    async fn anchored_crossing(&self, loaded: &LoadedPolicy) -> Option<chrono::DateTime<Utc>> {
        let anchor = loaded.auto_check_at()?;
        if !self.routes.clock().await.believed() {
            return None;
        }
        micad_settings::configuration::last_crossing(anchor, self.cadence.wall())
    }

    /// Step 1: the check cadence, shared by `check` and `auto`.
    async fn check_if_due(&mut self, loaded: &LoadedPolicy) {
        // `auto_check_minutes` is the one reading of "does this device check
        // on its own": `off`, a zero interval and a document that did not
        // load all answer `None`, so no caller re-derives the three.
        let Some(interval) = loaded.auto_check_minutes() else {
            return;
        };
        let now = self.cadence.now();
        match self.anchored_crossing(loaded).await {
            // Anchored: the check is due when the named time of day has come
            // round since the last one this driver answered, and at no other
            // moment. The interval is not consulted -- an operator who named
            // a time asked for that time, not for that time or sooner.
            Some(crossing) => {
                if crossing <= self.anchor_floor {
                    return;
                }
                self.anchor_floor = crossing;
            }
            None => {
                if now.duration_since(self.last_check)
                    < Duration::from_secs(interval.saturating_mul(60))
                {
                    return;
                }
            }
        }
        self.last_check = now;
        self.routes.audit(UPDATE_CHECK_EVENT).await;
        match self.routes.check(SENDER).await {
            // The device is up to date. Recorded rather than passed over:
            // the lifecycle renders this as plain `idle`, which is also what
            // a device that has never checked renders as, and an operator
            // watching a release they expect needs to be told that the
            // device looked and the source does not carry it.
            Ok(Settled::NoneCompatible) => {
                self.defer(
                    update_codes::DEFER_NO_NEWER_RELEASE,
                    "the source publishes nothing newer for this product than the running system",
                )
                .await;
            }
            // It found one: that supersedes the fact above and nothing else.
            // A window that was shut a minute ago is still shut.
            Ok(_) => {
                self.routes
                    .resume(Some(update_codes::DEFER_NO_NEWER_RELEASE))
                    .await
            }
            Err(refusal) => {
                tracing::debug!(reason = refusal.message(), "automatic check skipped");
                self.defer(update_codes::DEFER_CHECK_REFUSED, refusal.message())
                    .await;
            }
        }
    }

    /// The `auto` pass: check, fetch, re-check, install, reboot.
    async fn drive(&mut self, loaded: &LoadedPolicy) {
        match self.stage {
            Stage::Installing => return self.await_install(loaded).await,
            Stage::RebootPending => return self.reboot_if_allowed(loaded).await,
            Stage::Idle => {}
        }
        self.check_if_due(loaded).await;
        // Step 2 — fetch what the check named, unless it is already staged.
        // Comparing the candidate against the staged file (rather than
        // fetching only when nothing is staged) is what lets the pass move
        // on when the publisher released something newer between the fetch
        // and the window: the newer target is fetched, and the older staged
        // file is left where a human can still install it.
        if let Some(candidate) = self.routes.available().await {
            let staged = self.routes.staged().await;
            if staged.as_deref().and_then(descriptor_id) != Some(candidate.deployment_id.as_str()) {
                self.routes.audit(UPDATE_FETCH_EVENT).await;
                if let Err(refusal) = self.routes.fetch(SENDER).await {
                    tracing::debug!(reason = refusal.message(), "automatic fetch skipped");
                    self.defer(update_codes::DEFER_FETCH_REFUSED, refusal.message())
                        .await;
                    return;
                }
            }
        }
        // Step 3 — install, in the window, only what the metadata still names.
        if let Some(descriptor) = self.routes.staged().await {
            self.install_if_allowed(loaded, &descriptor).await;
        }
    }

    /// Recheck policy, native state and catalog selection before installation.
    async fn install_if_allowed(&mut self, loaded: &LoadedPolicy, descriptor: &str) {
        // The clock, FIRST in the list on purpose. Every other
        // precondition below is judged against a wall clock: the maintenance
        // window is UTC `HH:MM`, so a window verdict computed from a clock
        // nobody vouches for is not a verdict, and refusing on the window
        // afterwards would report the wrong reason for the same refusal.
        // `believes` is the floor and
        // `synchronized`, not a notion invented here. Checks and fetches are
        // deliberately unaffected — neither is time-keyed, and refusing them
        // would make a clockless device stop even discovering updates.
        if let Some(reason) = self.routes.clock().await.untrusted_reason() {
            self.defer(update_codes::DEFER_CLOCK_UNTRUSTED, &reason)
                .await;
            return;
        }
        // The same refusal `InstallUpdate` answers an operator with: the
        // maintenance window, which `auto` requires the document to name.
        if let Some(reason) = update_policy::install_refusal(loaded, Utc::now()) {
            self.defer(update_codes::DEFER_OUTSIDE_WINDOW, &reason)
                .await;
            return;
        }
        let Some(facts) = self.routes.facts().await else {
            self.defer(
                update_codes::DEFER_DEPLOYMENT_STATUS_UNKNOWN,
                "the native backend did not answer the deployment query",
            )
            .await;
            return;
        };
        if facts.reboot_pending {
            self.defer(
                update_codes::DEFER_REBOOT_PENDING,
                "a deployment is already installed and waiting for its first boot",
            )
            .await;
            return;
        }
        // Refresh the authenticated selection immediately before installation.
        self.routes.audit(UPDATE_CHECK_EVENT).await;
        let named = match self.routes.check(SENDER).await {
            Ok(Settled::Done(candidate)) => Some(candidate),
            Ok(Settled::NoneCompatible) => None,
            Ok(Settled::Unready(unready)) => {
                self.defer(update_codes::DEFER_WORKSPACE_UNREADY, &unready.reason())
                    .await;
                return;
            }
            Ok(Settled::Failed(failed)) => {
                self.defer(update_codes::DEFER_RECHECK_FAILED, &failed.text)
                    .await;
                return;
            }
            Err(refusal) => {
                self.defer(update_codes::DEFER_RECHECK_REFUSED, refusal.message())
                    .await;
                return;
            }
        };
        let version = match named {
            // The metadata still names it: this is the version to install.
            Some(candidate)
                if Some(candidate.deployment_id.as_str()) == descriptor_id(descriptor) =>
            {
                candidate.version
            }
            // It names something else. The staged descriptor is superseded rather
            // than provably withdrawn — a check reports the selection, not the
            // whole target list — so it is not deleted, and the next pass
            // fetches what was named.
            Some(candidate) => {
                self.defer(
                    update_codes::DEFER_SUPERSEDED,
                    &format!("the check now names {}", candidate.deployment_id),
                )
                .await;
                return;
            }
            // Nothing compatible is published at all, so the current metadata
            // does not name the staged descriptor: it was withdrawn.
            None => {
                self.routes
                    .discard_staged("the current metadata no longer names it")
                    .await;
                return;
            }
        };
        self.routes.audit(UPDATE_INSTALL_EVENT).await;
        match self.routes.install(SENDER, descriptor).await {
            Ok(()) => {
                tracing::warn!(descriptor, version, "automatic install started");
                self.stage = Stage::Installing;
                // The pass ran to its end; every reason it was refused for
                // before now is stale.
                self.routes.resume(None).await;
            }
            Err(refusal) => {
                self.defer(update_codes::DEFER_INSTALL_REFUSED, &refusal)
                    .await
            }
        }
    }

    /// Wait out the install this driver started, then owe a reboot or not.
    async fn await_install(&mut self, loaded: &LoadedPolicy) {
        // An unanswered query leaves the stage where it is: the install is
        // still whatever it was, and the next tick asks again.
        let Some(facts) = self.routes.facts().await else {
            return;
        };
        match facts.install_status.as_deref() {
            Some("running") => {}
            Some("done") => {
                self.stage = Stage::RebootPending;
                self.reboot_if_allowed(loaded).await;
            }
            // A failed install, or a status nobody wrote: either way nothing
            // is owed a reboot, and the failure is already recorded under
            // `update.install` where an operator reads it.
            other => {
                if let Some(status) = other {
                    tracing::warn!(status, "automatic install did not finish cleanly");
                }
                self.stage = Stage::Idle;
            }
        }
    }

    /// Step 4: reboot under `rebootPolicy`, in the same window, gate-honoured.
    async fn reboot_if_allowed(&mut self, loaded: &LoadedPolicy) {
        if loaded.policy.reboot_policy != RebootPolicy::Window {
            // `manual`: the install happened, the reboot is a human's. The
            // lifecycle reports `reboot-required` until one arrives.
            self.stage = Stage::Idle;
            return;
        }
        if let Some(reason) = update_policy::install_refusal(loaded, Utc::now()) {
            self.defer(update_codes::DEFER_OUTSIDE_WINDOW, &reason)
                .await;
            return;
        }
        if let Err(refusal) = self.routes.reboot(SENDER).await {
            // The gate is closed: an install in flight, or a component
            // reporting a blocking health status. Automation defers and the
            // next window re-attempts. It does not arm the override — there
            // is no method on `AutoRoutes` to arm it with.
            self.defer(update_codes::DEFER_REBOOT_GATE_CLOSED, &refusal)
                .await;
            return;
        }
        // The machine is going down; the stage it leaves behind is moot, and
        // so is every reason the pass was refused for on the way here.
        self.routes.resume(None).await;
        self.stage = Stage::Idle;
    }

    /// Say why an automatic step did not proceed, in the log AND in the
    /// state.
    async fn defer(&self, reason: &str, detail: &str) {
        tracing::info!(reason, detail, "automatic update deferred");
        self.routes.defer(reason, detail).await;
    }
}

/// The target name a staged descriptor path carries. `mica-deploy` stages a
/// verified descriptor under its target name and nothing else, so the file name
/// is what a check's selection is compared against.
fn descriptor_id(descriptor: &str) -> Option<&str> {
    Path::new(descriptor)
        .file_stem()
        .and_then(|name| name.to_str())
}

/// A cadence a test moves by hand, and the reason [`Cadence`] exists.
///
/// [`Instant`] cannot be constructed at a chosen point, but it can be offset
/// from one, so a driver holding this advances exactly as far as a test says
/// and never by sleeping. Monotonic like the production clock and for the
/// same reason: nothing here answers a time of day.
#[cfg(test)]
pub struct TestCadence {
    base: Instant,
    offset: std::sync::Mutex<Duration>,
    /// The wall clock the anchor reads, moved on its own: an interval and a
    /// time of day are different questions, and a test says so by answering
    /// them separately.
    wall: std::sync::Mutex<chrono::DateTime<Utc>>,
}

#[cfg(test)]
impl TestCadence {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            base: Instant::now(),
            offset: std::sync::Mutex::new(Duration::ZERO),
            wall: std::sync::Mutex::new(Utc::now()),
        })
    }

    /// Move the driver's notion of the time of day forward by `by`.
    pub fn advance_wall(&self, by: chrono::Duration) {
        let mut wall = self.wall.lock().expect("cadence wall");
        *wall += by;
    }

    /// Move the driver's notion of now forward by `by`.
    pub fn advance(&self, by: Duration) {
        *self.offset.lock().expect("cadence offset") += by;
    }

    /// Past any cadence these tests configure, so the next tick checks.
    pub fn advance_past_the_check_interval(&self) {
        self.advance(Duration::from_secs(61 * 60));
    }
}

#[cfg(test)]
impl Cadence for TestCadence {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().expect("cadence offset")
    }

    fn wall(&self) -> chrono::DateTime<Utc> {
        *self.wall.lock().expect("cadence wall")
    }
}

#[cfg(test)]
mod tests;
