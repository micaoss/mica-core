//! DATA's growth at the first boot (`docs/mica-core.md` 6.6): the factory
//! image ships the `data` volume empty, and before the kernel formats it as
//! UBIFS, which fixes the file system's largest size at the volume's, the
//! runkit grows it to the UBI device's free LEBs less the store reserve the
//! signed policy states.
use super::{DATA, UbiDevice, VolumeKind, find, lebs};
use anyhow::{Context, Result, ensure};

/// The UBI device's space as `mica-deploy status` reports it under `ubi`:
/// the LEB size, the free LEBs, DATA's LEBs, and the reserves the store keeps
/// in LEBs (`reserves.system` for each install, and the policy's
/// `storeReserve`, when it states one, kept free when DATA grew).
pub fn space(
    ubi: &dyn UbiDevice,
    system_reserve: u64,
    store_reserve: Option<u64>,
) -> Result<serde_json::Value> {
    let data = find(ubi, DATA)?.context("UBI volume data is absent")?;
    let mut space = serde_json::json!({
        "lebBytes": ubi.leb_bytes(),
        "availableLebs": ubi.available_lebs()?,
        "dataLebs": data.reserved_lebs,
        "systemReserveLebs": lebs(ubi, system_reserve),
    });
    if let Some(bytes) = store_reserve {
        space["storeReserveLebs"] = lebs(ubi, bytes).into();
    }
    Ok(space)
}

/// What [`grow_data`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataGrowth {
    /// DATA holds data, so it is formatted and keeps its size.
    Formatted { lebs: u64 },
    /// Too little is free beside the reserve: DATA keeps its size.
    Kept {
        lebs: u64,
        available: u64,
        reserve: u64,
    },
    /// DATA was empty and now has `to` LEBs.
    Grown { from: u64, to: u64 },
}

/// Grow the empty `data` volume by the device's free LEBs less
/// `store_reserve` bytes (rounded up to LEBs), when at least one LEB is left
/// to give. A volume any of whose LEBs is mapped is left alone, which is the
/// test UBIFS makes before it formats an empty volume; the volume only grows.
/// Run again, it grows by what is free then, which after a growth is nothing.
pub fn grow_data(ubi: &dyn UbiDevice, store_reserve: u64) -> Result<DataGrowth> {
    let data = find(ubi, DATA)?.context("UBI volume data is absent")?;
    ensure!(
        data.kind == VolumeKind::Dynamic && data.sound(),
        "UBI volume data is not a sound dynamic volume"
    );
    {
        let volume = ubi.open(data.id, false)?;
        for leb in 0..data.reserved_lebs {
            if ubi.mapped(&volume, u32::try_from(leb)?)? {
                return Ok(DataGrowth::Formatted {
                    lebs: data.reserved_lebs,
                });
            }
        }
        // The volume is closed here: a resize takes it exclusively.
    }
    let available = ubi.available_lebs()?;
    let reserve = lebs(ubi, store_reserve);
    let Some(grow) = available.checked_sub(reserve).filter(|grow| *grow > 0) else {
        return Ok(DataGrowth::Kept {
            lebs: data.reserved_lebs,
            available,
            reserve,
        });
    };
    let to = data.reserved_lebs + grow;
    ubi.resize(data.id, to * ubi.leb_bytes())?;
    Ok(DataGrowth::Grown {
        from: data.reserved_lebs,
        to,
    })
}

/// Empty the `data` volume for a factory reset, before the kernel mounts it:
/// a volume update of no bytes unmaps every LEB, and UBIFS formats the empty
/// volume at the mount. The volume keeps its size. An update the kernel
/// interrupts leaves its marker, and the volume is emptied again the next
/// time; nothing else is touched.
pub fn wipe_data(ubi: &dyn UbiDevice) -> Result<u64> {
    let data = find(ubi, DATA)?.context("UBI volume data is absent")?;
    ensure!(
        data.kind == VolumeKind::Dynamic,
        "UBI volume data is not a dynamic volume"
    );
    let volume = ubi.open(data.id, true)?;
    ubi.begin_update(&volume, 0)?;
    Ok(data.reserved_lebs)
}
