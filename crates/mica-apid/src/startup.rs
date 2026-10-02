//! Start-up bundle discovery and the compatibility re-check.
//!
//! Nothing in this module returns an error, and nothing in it may `?`,
//! `unwrap`, `expect` or panic its way out. Discovery runs
//! after the listeners bind and after `APID_LISTENING` is printed, and every
//! outcome — an unreadable disk, a garbage manifest, an absent `/mica/ui` — to
//! be a [`BundleState`] the daemon holds. `main` propagates every earlier
//! start-up step with `?` and the unit is `Restart=on-failure`
//! (`dist/apid.service`), so an error out of here is a crash loop with
//! no listener bound.
//!
//! Two of the five classes are detected here, and both are detected at
//! start-up rather than per request.

use std::fmt;

use crate::bundle::{CompatCheck, Store};

/// The **served set**: the `versions` array of `GET /api/versions`.
///
/// The shape is an array, because the answer can legitimately have
/// more than one member, with [`CURRENT_API_VERSION`] always among them. This
/// constant is that array today.
///
/// `GET /api/versions` is declared (`routes::api_router`) and serves this
/// constant rather than a second copy of it; it is unauthenticated.
/// Every path under `/api/` that is not a declared route still 404s.
pub const SERVED_API_VERSIONS: &[&str] = &["v1"];

/// The `current`: the member a client with no preference should use.
///
/// Always a member of [`SERVED_API_VERSIONS`], and a test asserts it.
/// The check does not compare against this value; it is logged
/// so that a deactivation can be read back, and read by nothing else.
pub const CURRENT_API_VERSION: &str = "v1";

/// Why start-up removed the active pointer. Both are deactivations and
/// both can hold at once, so the state carries a list rather than one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Class 3: the tree no longer hashes to the digest recorded at
    /// activation. A tree that cannot be hashed at all counts as a mismatch,
    /// because it is one.
    DigestMismatch,
    /// Class 5: the declared range and the served set have no member in
    /// common. Both sets are carried so the log line can name both — that is
    /// what makes a correct deactivation distinguishable from an incorrect one
    /// after the fact.
    Incompatible {
        /// The manifest's declared API versions.
        declared: Vec<String>,
        /// The served set they were compared against.
        served: Vec<String>,
    },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DigestMismatch => {
                f.write_str("the tree no longer matches the digest recorded at activation")
            }
            Self::Incompatible { declared, served } => write!(
                f,
                "declared API versions {declared:?} have no member in common with the served set {served:?}"
            ),
        }
    }
}

/// What start-up discovery concluded — a state the daemon holds, never an
/// error it returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleState {
    /// No custom bundle is active. class 1: the shipped state of every
    /// device, and not an error.
    BuiltIn,
    /// A bundle is active and survived both re-checks.
    Active {
        /// The generation `current` resolves to.
        generation: u64,
        /// What the compatibility check decided, or that it could not run.
        compat: CompatCheck,
    },
    /// A bundle was active and start-up took the pointer down.
    Deactivated {
        /// The generation that was active.
        generation: u64,
        /// Every deactivation trigger that fired, in class order.
        reasons: Vec<Reason>,
        /// Whether the pointer actually came down. `false` means the reasons
        /// hold but the removal failed, which is a worse state than either and
        /// is therefore not collapsed into the same word.
        removed: bool,
    },
    /// The store could not be evaluated at all. The asset router reads the
    /// same store per request and answers with the built-in UI when it cannot
    /// read it, so this is a degraded state and not a fatal one.
    Unavailable {
        /// What went wrong, for the operator reading the log.
        why: String,
    },
}

impl fmt::Display for BundleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuiltIn => f.write_str(crate::bundle::NO_CUSTOM_BUNDLE),
            Self::Active { generation, compat } => {
                write!(f, "generation {generation} is active: {}", describe(compat))
            }
            Self::Deactivated {
                generation,
                reasons,
                removed,
            } => {
                let verb = if *removed {
                    "was deactivated at start-up"
                } else {
                    "should have been deactivated at start-up and the pointer is still in place"
                };
                let why = reasons
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                write!(f, "generation {generation} {verb}: {why}")
            }
            Self::Unavailable { why } => {
                write!(f, "the bundle store could not be evaluated: {why}")
            }
        }
    }
}

/// `main`'s entry point, and the whole of the start-up half.
///
/// Called after the listeners bind and after `APID_LISTENING` is printed.
/// It takes no `Result` out and it takes no `Result` back in: the return type
/// has no error variant, so `main` cannot propagate one by accident.
///
/// The evaluation runs on the blocking pool for two reasons and both matter.
/// It re-hashes a whole tree, which does not belong on an async worker; and a
/// panic raised anywhere beneath it is delivered as a `JoinError` rather than
/// unwinding `main`, so the "a bundle cannot stop apid from listening"
/// property survives a bug in code this module only calls.
pub async fn discover(store: Store, audit: std::sync::Arc<crate::audit::Audit>) -> BundleState {
    join(tokio::task::spawn_blocking(move || run(&store, &audit)).await)
}

/// Turn the blocking task's join result into a state. A `JoinError` here means
/// something below this module panicked; there is exactly one response to
/// that and it is not re-panicking.
fn join(joined: Result<BundleState, tokio::task::JoinError>) -> BundleState {
    match joined {
        Ok(state) => state,
        Err(err) => {
            let why = format!("bundle discovery did not complete: {err}");
            tracing::error!(error = %err, "bundle discovery did not complete; the built-in UI is what the asset router will serve");
            BundleState::Unavailable { why }
        }
    }
}

/// The synchronous body, against the served set this binary actually serves.
fn run(store: &Store, audit: &crate::audit::Audit) -> BundleState {
    evaluate(store, audit, SERVED_API_VERSIONS)
}

/// The body against an arbitrary served set.
///
/// The set is a parameter so that the A/B case this exists for — a bundle
/// activated against one image's API and re-checked against the next image's —
/// is reachable in a test. `run` is the only caller that chooses it, and it
/// chooses [`SERVED_API_VERSIONS`].
fn evaluate(store: &Store, audit: &crate::audit::Audit, served: &[&str]) -> BundleState {
    pick_up_staged(store, audit, served);
    recheck(store, served)
}

/// The second local install path. Failure is logged and start-up
/// continues: a staged tree that cannot be activated must not prevent the
/// already-active one from being evaluated.
///
/// A successful pick-up goes to the audit trail as well as the journal: it is
/// the one path that changes which UI the appliance serves without any HTTP
/// request, so the "custom-UI activate" event is recorded here.
/// The source is `local` — the trigger is a directory staged on the disk, not
/// a network peer.
fn pick_up_staged(store: &Store, audit: &crate::audit::Audit, served: &[&str]) {
    match store.pick_up_staged(served) {
        Ok(None) => {}
        Ok(Some(activation)) => {
            tracing::info!(
                generation = activation.generation,
                digest = %activation.digest,
                compat = %describe(&activation.compat),
                "picked up a staged UI bundle at start-up"
            );
            // The device acted on what it found on the disk at start-up.
            // Recording it as an operator's would be the exact lie the actor
            // field exists to prevent: nobody asked for this one.
            audit.record_as(
                "custom-ui",
                "activated",
                "local",
                micad_settings::ACTOR_DEVICE,
            );
        }
        Err(err) => tracing::warn!(
            error = %format!("{err:#}"),
            "a staged UI bundle could not be activated at start-up; nothing else changed"
        ),
    }
}

/// Classes 3 and 5 against the active bundle.
fn recheck(store: &Store, served: &[&str]) -> BundleState {
    let recheck = match store.recheck_active(served) {
        Ok(Some(recheck)) => recheck,
        Ok(None) => {
            // Class 1. INFO, never WARN and never ERROR: this is the
            // shipped state of every device and "it must not be logged as one".
            tracing::info!(
                root = %store.root().display(),
                served = ?served,
                current = CURRENT_API_VERSION,
                "{}",
                crate::bundle::NO_CUSTOM_BUNDLE
            );
            return BundleState::BuiltIn;
        }
        Err(err) => {
            let why = format!("{err:#}");
            tracing::warn!(
                root = %store.root().display(),
                error = %why,
                "the UI bundle store could not be evaluated at start-up; the built-in UI is what the asset router will serve"
            );
            return BundleState::Unavailable { why };
        }
    };
    let generation = recheck.generation;

    let mut reasons = Vec::new();
    if recheck.corrupt() {
        // Class 3: "deactivate and serve the built-in UI, logging the
        // mismatch".
        tracing::warn!(
            generation,
            "the UI bundle no longer matches the digest recorded at activation; deactivating"
        );
        reasons.push(Reason::DigestMismatch);
    }
    if let CompatCheck::Ran {
        declared,
        served,
        compatible: false,
    } = &recheck.compat
    {
        // Class 5. Both sets go in the line, because "recording the
        // served set in the log line is what makes the two distinguishable
        // after the fact".
        tracing::warn!(
            generation,
            declared = ?declared,
            served = ?served,
            current = CURRENT_API_VERSION,
            "the UI bundle's declared API versions have no member in common with the served set; deactivating"
        );
        reasons.push(Reason::Incompatible {
            declared: declared.clone(),
            served: served.clone(),
        });
    }

    if reasons.is_empty() {
        tracing::info!(
            generation,
            compat = %describe(&recheck.compat),
            "a custom UI bundle is active"
        );
        return BundleState::Active {
            generation,
            compat: recheck.compat,
        };
    }

    let removed = match store.deactivate() {
        Ok(removed) => removed,
        Err(err) => {
            tracing::error!(
                generation,
                error = %format!("{err:#}"),
                "the UI bundle could not be deactivated; the pointer is still in place"
            );
            false
        }
    };
    BundleState::Deactivated {
        generation,
        reasons,
        removed,
    }
}

/// One line for a compatibility result, including the "could not be checked"
/// case that is a degradation rather than a rejection.
fn describe(compat: &CompatCheck) -> String {
    match compat {
        CompatCheck::NotRun => {
            "unchecked — the bundle carries no manifest, so nothing could be compared against the served set".to_string()
        }
        CompatCheck::Ran {
            declared,
            served,
            compatible,
        } => format!(
            "declared {declared:?} against the served set {served:?} — {}",
            if *compatible {
                "compatible"
            } else {
                "no member in common"
            }
        ),
    }
}

#[cfg(test)]
mod tests;
