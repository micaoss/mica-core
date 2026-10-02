//! Importing a staged provisioning document, and the record of every attempt.

use anyhow::{Context, Result};
use micad_settings::{ProvisioningDocumentSettings, ProvisioningImport, Settings, Store};
use std::path::Path;

use super::*;

/// Read, validate and apply a provisioning document, if one is offered.
///
/// Consults [`SOURCES`] in order under `staging_root` and takes the FIRST that
/// carries a document; the other is not read. On success `settings` is
/// replaced with the applied tree, so the caller's first reconcile already
/// sees it.
pub fn import(store: &Store, settings: &mut Settings, staging_root: &Path) -> Result<Outcome> {
    let Some((source, path)) = find_document(staging_root) else {
        return Ok(Outcome::NoDocument);
    };
    tracing::info!(%source, path = %path.display(), "provisioning document offered");

    let document = match read_document(&path) {
        Ok(document) => document,
        Err(rejection) => return reject(store, settings, source, rejection),
    };
    if let Err(rejection) = validate(&document) {
        return reject(store, settings, source, rejection);
    }
    let digest = digest_of(&document);

    // The short-circuit, BEFORE the claim gate: a device that was claimed by
    // the very document being offered must report `unchanged`, not
    // `already-claimed`. Nothing is written on this path at all, so a stick
    // left in a socket costs no flash write per boot.
    if settings
        .provisioning
        .document
        .as_ref()
        .and_then(|record| record.applied_digest.as_deref())
        == Some(digest.as_str())
    {
        let outcome = Outcome::Unchanged { source, digest };
        record_attempt(store, settings, source, &outcome)?;
        tracing::info!(%source, "provisioning document already applied; nothing to do");
        return Ok(outcome);
    }

    // The claim gate. A document is the FIRST-RUN channel: once an
    // administrator credential exists the device is claimed, and reconfiguring
    // it goes through the authenticated API. Neither transport carries a
    // signature, so without this gate a stick pushed into a fielded device
    // would reconfigure it — including its administrator password.
    if settings.access.web_admin.is_some() {
        return reject(
            store,
            settings,
            source,
            Rejection::whole(
                "this device is already claimed: it has an administrator credential, so \
                 configuration changes go through the authenticated API rather than through a \
                 provisioning document",
            ),
        );
    }

    // Everything from here mutates a private clone. `*settings` is replaced
    // only after the save that commits the tree has returned.
    let mut applied = settings.clone();
    if let Err(rejection) = apply_into(&document, &mut applied) {
        return reject(store, settings, source, rejection);
    }
    let outcome = Outcome::Applied {
        source,
        version: document.version,
        digest: digest.clone(),
    };
    applied.provisioning.document = Some(ProvisioningDocumentSettings {
        applied_version: Some(document.version),
        applied_digest: Some(digest),
        last_import: Some(import_record(source, &outcome)),
    });
    store
        .save(&applied)
        .context("persist the applied provisioning document")?;
    tracing::info!(
        %source,
        version = document.version,
        "provisioning document applied"
    );
    *settings = applied;
    Ok(outcome)
}

/// Record a refusal and return it. Nothing the document named is applied.
pub(super) fn reject(
    store: &Store,
    settings: &mut Settings,
    source: Source,
    rejection: Rejection,
) -> Result<Outcome> {
    tracing::warn!(
        %source,
        key = rejection.key,
        reason = rejection.reason,
        "provisioning document refused; the device is unchanged"
    );
    let outcome = Outcome::Rejected { source, rejection };
    record_attempt(store, settings, source, &outcome)?;
    Ok(outcome)
}

/// Persist the import record for an attempt that applied nothing.
pub(super) fn record_attempt(
    store: &Store,
    settings: &mut Settings,
    source: Source,
    outcome: &Outcome,
) -> Result<()> {
    let record = import_record(source, outcome);
    let existing = settings
        .provisioning
        .document
        .as_ref()
        .and_then(|document| document.last_import.as_ref());
    if let Some(existing) = existing
        && existing.source == record.source
        && existing.outcome == record.outcome
        && existing.reason == record.reason
    {
        return Ok(());
    }
    let mut updated = settings.clone();
    let document = updated
        .provisioning
        .document
        .get_or_insert_with(ProvisioningDocumentSettings::default);
    document.last_import = Some(record);
    store
        .save(&updated)
        .context("persist the provisioning import record")?;
    *settings = updated;
    Ok(())
}

/// The settings record for one attempt.
pub(super) fn import_record(source: Source, outcome: &Outcome) -> ProvisioningImport {
    let (name, reason) = match outcome {
        Outcome::Applied { .. } => ("applied", None),
        Outcome::Unchanged { .. } => ("unchanged", None),
        Outcome::Rejected { rejection, .. } => ("rejected", Some(rejection.to_string())),
        // Not reachable: `NoDocument` records nothing. Spelled rather than
        // `unreachable!` so a future caller cannot panic a boot path.
        Outcome::NoDocument => ("none", None),
    };
    ProvisioningImport {
        source: source.as_str().to_string(),
        outcome: name.to_string(),
        reason,
        at: now_seconds(),
    }
}

/// The device clock as seconds since the epoch, saturating at 0.
///
/// A label and never a deadline: the import runs before any time source has
/// been consulted, so this reading is whatever the clock happened to say.
pub(super) fn now_seconds() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0)
}
