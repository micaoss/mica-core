//! On a `ubi` board an object the store keeps in a static volume is written
//! straight into it as it arrives, never staged on DATA (`docs/mica-core.md`
//! 6.3, 6.6): through `p.<name>`, read back against its signed digest before
//! it is renamed to `<name>`. A power cut loses only the object in flight.

use super::*;
use crate::deployments::Place;

impl Acquisition<'_> {
    /// The volumes each object of `places` is written into, by digest; none
    /// off a `ubi` board. An object any of whose places is a file is staged as
    /// before, since installation copies that file from the workspace.
    pub(super) fn volumes<'p>(
        &self,
        places: impl IntoIterator<Item = (Place, &'p Artifact)>,
    ) -> BTreeMap<String, Vec<String>> {
        let mut volumes: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();
        if self.store.ubi().is_none() {
            return BTreeMap::new();
        }
        for (place, artifact) in places {
            let entry = volumes
                .entry(artifact.sha256.clone())
                .or_insert_with(|| Some(Vec::new()));
            match (place, entry.as_mut()) {
                (Place::Volume(name), Some(names)) => names.push(name),
                (Place::File(_), _) => *entry = None,
                (Place::Volume(_), None) => {}
            }
        }
        volumes
            .into_iter()
            .filter_map(|(sha, names)| Some((sha, names?)))
            .collect()
    }

    /// The volumes of `names` not yet in the store.
    fn unheld(&self, names: &[String]) -> Result<Vec<String>> {
        let ubi = self
            .store
            .ubi()
            .context("UBI object on a board without UBI")?;
        let mut unheld = Vec::new();
        for name in names {
            if crate::ubi::find(ubi, name)?.is_none() {
                unheld.push(name.clone());
            }
        }
        Ok(unheld)
    }

    /// Copy the object in the first of `names` into the others, each read
    /// back against `artifact` as any published object is.
    fn replicate(&self, names: &[String], artifact: &Artifact) -> Result<()> {
        let ubi = self
            .store
            .ubi()
            .context("UBI object on a board without UBI")?;
        let first = crate::ubi::find(ubi, &names[0])?.context("written UBI object vanished")?;
        for name in &names[1..] {
            crate::ubi::publish_object(ubi, &ubi.path(first.id), name, artifact)?;
        }
        Ok(())
    }

    /// Hold `missing` to the device before any of it is written. The objects
    /// that go to `volumes` count against the budget with the workspace, and
    /// must fit by installation's rule, the system reserve left free. When
    /// they do not and `incoming` is the deployment being fetched, the
    /// fallback is retired first, as installing it would.
    pub(super) fn room(
        &self,
        missing: &BTreeMap<String, u64>,
        volumes: &BTreeMap<String, Vec<String>>,
        incoming: Option<&Deployment>,
    ) -> Result<()> {
        let mut staged = BTreeMap::new();
        let mut written = Vec::new();
        for (sha, bytes) in missing {
            match volumes.get(sha) {
                Some(names) => written.extend(self.unheld(names)?.into_iter().map(|_| *bytes)),
                None => {
                    staged.insert(sha.clone(), *bytes);
                }
            }
        }
        let Some(ubi) = self.store.ubi().filter(|_| !written.is_empty()) else {
            return self.reserve_missing(&staged);
        };
        let bytes = written.iter().sum::<u64>();
        let used = self
            .files()?
            .into_iter()
            .map(|(_, bytes)| bytes)
            .sum::<u64>();
        let budget = self.budget()?;
        ensure!(
            used.checked_add(bytes).is_some_and(|total| total <= budget),
            "workspace budget exhausted"
        );
        let lebs = written
            .iter()
            .map(|bytes| crate::ubi::lebs(ubi, *bytes))
            .sum::<u64>()
            + crate::ubi::lebs(ubi, self.store.reserves.system());
        if ubi.available_lebs()? < lebs
            && let (Some(incoming), Some(release)) = (incoming, self.release)
        {
            release(incoming)?;
        }
        ensure!(ubi.available_lebs()? >= lebs, "insufficient UBI space");
        self.reserve_missing(&staged)
    }

    /// Fetch `object` straight into the volumes `names`: into the first by
    /// delta while the origin serves indexes, else whole, then copied into the
    /// rest. A delta that fails is dropped with its volume and the whole
    /// object written afresh.
    pub(super) fn fetch_into(
        &self,
        object: &SourceObject,
        names: &[String],
        seeds: &mut Option<chunks::SeedMap>,
        origin_has_indexes: &mut bool,
    ) -> Result<()> {
        let artifact = Artifact {
            sha256: object.sha256.clone(),
            bytes: object.bytes,
        };
        self.fetch_one(object, &names[0], seeds, origin_has_indexes)?;
        self.replicate(names, &artifact)
    }

    fn fetch_one(
        &self,
        object: &SourceObject,
        name: &str,
        seeds: &mut Option<chunks::SeedMap>,
        origin_has_indexes: &mut bool,
    ) -> Result<()> {
        let ubi = self
            .store
            .ubi()
            .context("UBI object on a board without UBI")?;
        let artifact = Artifact {
            sha256: object.sha256.clone(),
            bytes: object.bytes,
        };
        if *origin_has_indexes {
            let cut = match seeds {
                Some(cut) => cut,
                None => seeds.insert(chunks::seed_map(&self.seeds())?),
            };
            let assembled = crate::ubi::publish_from(ubi, name, &artifact, &mut |output| {
                self.assemble(object, cut, output)
            });
            if assembled.is_ok() {
                return Ok(());
            }
            *origin_has_indexes = false;
            self.drop_delta(&object.sha256);
        }
        crate::ubi::publish_from(ubi, name, &artifact, &mut |output| {
            download_into(&object.url, output, artifact.bytes)
        })
    }

    /// Copy one archive object from `input` straight into the volumes
    /// `names`, checking it against its digest as it passes. An object
    /// already held in the first is only read through.
    pub(super) fn import_into(
        &self,
        input: &mut impl Read,
        names: &[String],
        artifact: &Artifact,
    ) -> Result<()> {
        let ubi = self
            .store
            .ubi()
            .context("UBI object on a board without UBI")?;
        if crate::ubi::find(ubi, &names[0])?.is_some() {
            copy_checked(input, artifact, None)?;
            crate::ubi::verify_object(ubi, &names[0], artifact)?;
        } else {
            crate::ubi::publish_from(ubi, &names[0], artifact, &mut |output| {
                copy_checked(input, artifact, Some(output))
            })?;
        }
        self.replicate(names, artifact)
    }
}

/// Copy exactly `artifact`'s bytes from `input` into `output`, or only read
/// them through without one, and hold them to its digest.
pub(super) fn copy_checked(
    input: &mut impl Read,
    artifact: &Artifact,
    mut output: Option<&mut dyn Write>,
) -> Result<()> {
    let mut remaining = artifact.bytes;
    let mut buffer = [0; 65536];
    let mut hash = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..count])?;
        hash.update(&buffer[..count]);
        if let Some(output) = &mut output {
            output.write_all(&buffer[..count])?;
        }
        remaining -= count as u64;
    }
    ensure!(
        hex::encode(hash.finish()) == artifact.sha256,
        "archive object digest mismatch"
    );
    Ok(())
}
