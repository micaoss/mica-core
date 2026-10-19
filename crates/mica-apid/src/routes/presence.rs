//! Physical presence, the assertion the recovery routes require.

use std::path::PathBuf;

use super::*;

/// The named board capability that decides what a presence assertion IS.
///
/// **One seam, keyed by one capability, and the capability's answer is the
/// BOARD's**. A board declares the physical
/// recovery actions it implements and what each maps to; the mechanisms those
/// actions name are what this board answers `recovery.presence` with, and this
/// crate knows nothing about which of them produced an assertion. There is no
/// per-board branch here and no button, console or medium code: adopting a
/// mechanism is a change to a board's declaration and to the BSP that
/// implements it, and to no flow, route or tier.
///
/// **Both shipped boards declare NONE**, so on a fielded mica device this
/// capability is answered with nothing at all and every presence-gated flow
/// refuses with [`NoPresence::BoardDeclaresNone`].
pub(crate) const PRESENCE_CAPABILITY: &str = "recovery.presence";

/// The audit event a credential recovery is recorded under when there is no
/// mechanism to name — every refusal taken before presence is established.
///
/// A refusal has no door to record, so it records none. The success and
/// aborted lines carry `credential-recovery-<mechanism>` instead
/// (`micad_settings::credential_recovery_event`), which is what makes "which
/// door was used" greppable.
pub(super) const CREDENTIAL_RECOVERY_EVENT: &str = "credential-recovery";

/// A presence assertion made at the device.
pub(crate) struct Assertion {
    /// The mechanism, which is also what the audit event is named for. A
    /// value the board declared; this crate never invents one.
    pub(super) mechanism: String,
    /// The channel that proved presence, and therefore the ONE channel a
    /// minted credential may be published on.
    pub(super) channel: PathBuf,
}

impl Assertion {
    /// The assertion a test's presence seam hands back.
    ///
    /// Test-only, and the channel is deliberately a path nothing opens: a test
    /// seam captures what it was asked to publish rather than writing it, so
    /// there is no file anywhere for a minted credential to be left in. The
    /// mechanism is the test's to choose, because a mechanism is a board fact
    /// and no board in this tree declares one.
    #[cfg(test)]
    pub(crate) fn for_test(mechanism: &str) -> Self {
        Self {
            mechanism: mechanism.to_string(),
            channel: PathBuf::from("/dev/null"),
        }
    }
}

/// Why an assertion was not established. Named, because a refusal is audited
/// and an operator has to be able to tell "nobody is at the device" from "the
/// assertion has run out" — and both of those from "this board has no way to
/// assert presence at all", which is not a thing standing at the device fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoPresence {
    /// The board declares no physical recovery action, so there is no action
    /// an operator could take. Both shipped boards.
    BoardDeclaresNone,
    /// The board declares actions and this build cannot read the declaration.
    DeclarationUnreadable,
    /// No assertion has been made.
    Absent,
    /// One was made and its window has passed.
    Expired,
    /// The marker is there but this build cannot read it as an assertion.
    Malformed,
    /// The assertion names a mechanism no action this board declares uses.
    UnknownMechanism(Vec<String>),
    /// One was made and a credential recovery has already spent it. The
    /// bound: one rotation per presence assertion, and the next one is
    /// re-performed at the device.
    Spent,
}

impl NoPresence {
    /// The sentence the refusal carries. It names no path and no value the
    /// marker held: a refusal is read by whoever asked, and what it may tell
    /// them is that presence was not established.
    pub(super) fn message(&self) -> String {
        match self {
            Self::BoardDeclaresNone => format!(
                "this board declares no physical recovery action, so presence cannot be asserted \
                 on it; it answers `{PRESENCE_CAPABILITY}` with nothing, and the action a board \
                 declares is implemented by its BSP"
            ),
            Self::DeclarationUnreadable => {
                "this board's physical recovery actions could not be read, so no presence \
                 assertion is accepted on it"
                    .to_string()
            }
            Self::Absent => {
                "this operation requires physical presence at the device, and none is asserted"
                    .to_string()
            }
            Self::Expired => {
                "the physical-presence assertion has expired; assert it again at the device"
                    .to_string()
            }
            Self::Malformed => {
                "the physical-presence assertion could not be read; assert it again at the device"
                    .to_string()
            }
            Self::UnknownMechanism(declared) => format!(
                "the physical-presence assertion names a mechanism this board does not offer; \
                 it answers `{PRESENCE_CAPABILITY}` with `{}`",
                declared.join("`, `")
            ),
            Self::Spent => {
                "the physical-presence assertion has already been spent by a credential \
                 recovery; assert it again at the device to run another"
                    .to_string()
            }
        }
    }
}

/// The ONE seam every presence-gated operation passes through.
///
/// Two operations and not one, because rule 2 binds them together: the
/// credential is returned "on the channel that proved presence", so whatever
/// decides presence is also what decides where a secret may be written. A
/// design that asserted here and published somewhere else could publish over
/// the network, which is the one thing that is forbidden outright.
pub(crate) trait Presence: Send + Sync {
    /// The assertion standing at this moment, or why there is none.
    fn assert(&self) -> Result<Assertion, NoPresence>;

    /// Write `secret` on the channel that proved presence, exactly once.
    ///
    /// # Errors
    ///
    /// Returns an error when the channel cannot be written, which is the audited
    /// `aborted`: presence was established and the flow did not complete.
    fn publish(&self, assertion: &Assertion, secret: &str) -> anyhow::Result<()>;

    /// Spend the assertion, so that it authorizes nothing further.
    ///
    /// The first rule states the bound this method IS: "one rotation per
    /// presence assertion, and the assertion is re-performed physically for
    /// the next one". Called after a rotation has committed, and never after
    /// one that refused or aborted — an assertion an operator spent a trip to
    /// the device on is not taken by a flow that did nothing.
    ///
    /// Spending only ever REMOVES authority, which is why it does not
    /// contradict the rule that apid never creates a presence assertion.
    ///
    /// # Errors
    ///
    /// Returns an error when the assertion could not be taken away. The
    /// rotation it followed still happened; the caller reports the failure
    /// rather than unsaying the commit.
    fn spend(&self) -> anyhow::Result<()>;
}

/// The shipped reader: the assertion micad left after mapping a board-declared
/// physical recovery action, published back on the channel that action named.
///
/// **This crate never CREATES a marker.** micad writes it, from an intent that
/// arrived on the kernel command line before Linux ran
/// (`micad_settings::Declaration::map_intent`); there is no route, no settings
/// path and no line in apid that creates it, so an API that could set it would
/// have to be written first — which is the forbidden change. apid reads it,
/// and [`Presence::spend`] takes it away once it has authorized its one
/// rotation, which only ever removes authority.
pub(crate) struct MarkerPresence {
    pub(super) marker: PathBuf,
    pub(super) declaration: PathBuf,
    /// The assertion this process spent, if it has spent one.
    ///
    /// **The marker itself, not a flag.** micad re-maps the command line if it
    /// restarts inside a boot, so a bare "something was spent" bit would
    /// refuse the fresh assertion that restart wrote; an assertion carries a
    /// deadline, so the one that was spent is identifiable.
    ///
    /// It is not the authority — the unlink in [`MarkerPresence::spend`] is,
    /// and it survives an apid restart where this does not. What this adds is
    /// the operator's answer: without it a second rotation attempt inside one
    /// presence window is told that presence "is not asserted", which is the
    /// wrong sentence for someone who is standing at the device having just
    /// asserted it. It also keeps the bound if the unlink fails.
    pub(super) spent: std::sync::Mutex<Option<micad_settings::PresenceMarker>>,
}

impl MarkerPresence {
    /// The shipped reader. Two paths and no syscall until something asserts.
    pub(crate) fn at_default() -> Self {
        Self::at(
            micad_settings::presence_marker_path(),
            micad_settings::declaration_path(),
        )
    }

    /// The same reader over given paths, so a test can drive the SHIPPED one
    /// rather than a seam that stands in for it.
    pub(crate) fn at(marker: PathBuf, declaration: PathBuf) -> Self {
        Self {
            marker,
            declaration,
            spent: std::sync::Mutex::new(None),
        }
    }

    /// Whether `marker` is the assertion this process already spent.
    pub(super) fn is_spent(&self, marker: &micad_settings::PresenceMarker) -> bool {
        self.spent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|spent| spent == marker)
    }

    /// Whether anything has been spent at all, which is what tells an absent
    /// marker that was taken from one that was never written.
    pub(super) fn has_spent(&self) -> bool {
        self.spent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
}

impl Presence for MarkerPresence {
    fn assert(&self) -> Result<Assertion, NoPresence> {
        // The board's declaration FIRST, because what it says changes what an
        // absent marker means: on a board that declares no action there is
        // nothing an operator could have done, and telling them presence is
        // merely "not asserted" would send them looking for a door that does
        // not exist.
        let declaration = micad_settings::Declaration::read(&self.declaration);
        match &declaration {
            micad_settings::Declaration::None => return Err(NoPresence::BoardDeclaresNone),
            micad_settings::Declaration::Unreadable(reason) => {
                tracing::warn!(%reason, "the board's recovery declaration could not be read");
                return Err(NoPresence::DeclarationUnreadable);
            }
            micad_settings::Declaration::Actions(_) => {}
        }

        let bytes = std::fs::read(&self.marker).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                // The ordinary way a spent assertion is gone: `spend` unlinked
                // it. Saying so is the difference between "assert presence
                // again" and "presence was never asserted".
                if self.has_spent() {
                    NoPresence::Spent
                } else {
                    NoPresence::Absent
                }
            } else {
                NoPresence::Malformed
            }
        })?;
        let marker: micad_settings::PresenceMarker =
            serde_json::from_slice(&bytes).map_err(|_| NoPresence::Malformed)?;
        // A marker that is still on the filesystem because the unlink failed
        // is still spent. Checked before the mechanism and the deadline: what
        // it says about itself does not put back the rotation it authorized.
        if self.is_spent(&marker) {
            return Err(NoPresence::Spent);
        }
        if !declaration.declares_mechanism(&marker.mechanism) {
            return Err(NoPresence::UnknownMechanism(
                declaration
                    .mechanisms()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ));
        }
        if marker.expires <= device_clock_seconds() {
            return Err(NoPresence::Expired);
        }
        Ok(Assertion {
            mechanism: marker.mechanism,
            channel: marker.channel,
        })
    }

    fn publish(&self, assertion: &Assertion, secret: &str) -> anyhow::Result<()> {
        use std::io::Write;

        // A console is a character device. A REGULAR FILE is refused, and that
        // refusal is the whole of the check: publishing to a file would leave
        // the one copy of a minted credential on a filesystem, which is
        // the "reading, decrypting or exporting any stored secret" arrived
        // at from the other side.
        let metadata = std::fs::metadata(&assertion.channel)
            .with_context(|| format!("open {}", assertion.channel.display()))?;
        anyhow::ensure!(
            !metadata.is_file(),
            "{} is a regular file, not a console",
            assertion.channel.display()
        );
        let mut channel = std::fs::OpenOptions::new()
            .write(true)
            .open(&assertion.channel)
            .with_context(|| format!("open {}", assertion.channel.display()))?;
        writeln!(
            channel,
            "mica recovery: the new administrator password is {secret}"
        )?;
        channel.flush()?;
        Ok(())
    }

    fn spend(&self) -> anyhow::Result<()> {
        // Remembered BEFORE the unlink, and from the file rather than from the
        // `Assertion` in hand: an `Assertion` carries no deadline, and the
        // deadline is what distinguishes the assertion that was spent from a
        // later one written by a micad that restarted inside this boot.
        if let Ok(bytes) = std::fs::read(&self.marker)
            && let Ok(marker) = serde_json::from_slice::<micad_settings::PresenceMarker>(&bytes)
        {
            *self
                .spent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(marker);
        }
        match std::fs::remove_file(&self.marker) {
            Ok(()) => Ok(()),
            // Already gone is spent, not an error: this method's whole
            // postcondition is that the marker authorizes nothing, and it
            // does not.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err)
                .with_context(|| format!("spend the assertion at {}", self.marker.display())),
        }
    }
}
