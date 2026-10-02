//! The bounded MICA boot record inside U-Boot's redundant MMC environment.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub const ENV_SIZE: usize = 65536;

/// The largest boot partition the device accepts, in 512-byte sectors (1 TiB):
/// a bound on arithmetic, not on any board.
const MAX_SECTORS: u64 = 1 << 31;

/// Where the two boot record copies live, from the signed boot policy.
///
/// Authenticated data, never the disk: disk contents do not choose writable
/// offsets. Every value is valid by construction -- both copies are
/// `ENV_SIZE` long, sector aligned, inside the partition and apart -- so a
/// caller holding one can seek without checking again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "Records", into = "Records")]
pub struct FitLayout {
    start_sector: u64,
    sectors: u64,
    offsets: [u64; 2],
}

/// The policy's spelling of a [`FitLayout`]: the boot partition's start and
/// length in sectors, the copies' offsets inside it, and their size.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Records {
    start_sector: u64,
    sectors: u64,
    offsets: [u64; 2],
    size: u64,
}

impl FitLayout {
    /// A layout of two `ENV_SIZE` copies at `offsets` inside the partition of
    /// `sectors` sectors starting at `start_sector`.
    ///
    /// # Errors
    ///
    /// Names the first rule the geometry breaks.
    pub fn new(start_sector: u64, sectors: u64, offsets: [u64; 2]) -> Result<Self> {
        ensure!(
            (1..=MAX_SECTORS).contains(&start_sector) && (1..=MAX_SECTORS).contains(&sectors),
            "invalid boot partition geometry"
        );
        let size = ENV_SIZE as u64;
        for offset in offsets {
            ensure!(offset % 512 == 0, "boot record copy is not sector aligned");
            ensure!(
                offset + size <= sectors * 512,
                "boot record copy outside its partition"
            );
        }
        ensure!(
            offsets[0].abs_diff(offsets[1]) >= size,
            "boot record copies overlap"
        );
        Ok(Self {
            start_sector,
            sectors,
            offsets,
        })
    }

    /// Relative to the boot partition, which starts at [`Self::start_sector`].
    #[must_use]
    pub const fn offsets(self) -> [u64; 2] {
        self.offsets
    }

    #[must_use]
    pub const fn start_sector(self) -> u64 {
        self.start_sector
    }

    #[must_use]
    pub const fn sectors(self) -> u64 {
        self.sectors
    }
}

impl TryFrom<Records> for FitLayout {
    type Error = anyhow::Error;

    fn try_from(records: Records) -> Result<Self> {
        ensure!(
            records.size == ENV_SIZE as u64,
            "boot record size is not {ENV_SIZE}"
        );
        Self::new(records.start_sector, records.sectors, records.offsets)
    }
}

impl From<FitLayout> for Records {
    fn from(layout: FitLayout) -> Self {
        Self {
            start_sector: layout.start_sector,
            sectors: layout.sectors,
            offsets: layout.offsets,
            size: ENV_SIZE as u64,
        }
    }
}
const MAX_GENERATION: u64 = 9_007_199_254_740_991;
const KEY: &[u8] = b"mica_entries=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub kernel_id: String,
    pub generation: u64,
    pub tries_left: Option<u8>,
}

pub fn parse_records(text: &str) -> Result<Vec<Record>> {
    ensure!(text.len() <= 512, "boot records exceed bound");
    let mut records = Vec::new();
    for row in text
        .strip_prefix("v1|")
        .context("invalid boot record version")?
        .split(';')
    {
        let fields: Vec<_> = row.split(',').collect();
        ensure!(
            fields.len() == 4 && records.len() < 2,
            "invalid boot record count or fields"
        );
        crate::deployments::valid_id(fields[0])?;
        crate::deployments::valid_id(fields[1])?;
        let generation: u64 = fields[2].parse()?;
        ensure!(
            generation > 0 && generation <= MAX_GENERATION && generation.to_string() == fields[2],
            "invalid boot generation"
        );
        ensure!(
            records
                .iter()
                .all(|r: &Record| r.id != fields[0] && r.generation > generation),
            "ambiguous boot record order"
        );
        let tries_left = match fields[3] {
            "-" => None,
            "0" => Some(0),
            "1" => Some(1),
            "2" => Some(2),
            "3" => Some(3),
            _ => anyhow::bail!("invalid boot attempt counter"),
        };
        records.push(Record {
            id: fields[0].into(),
            kernel_id: fields[1].into(),
            generation,
            tries_left,
        });
    }
    Ok(records)
}

pub fn render_records(records: &[Record]) -> Result<String> {
    let text = format!(
        "v1|{}",
        records
            .iter()
            .map(|r| format!(
                "{},{},{},{}",
                r.id,
                r.kernel_id,
                r.generation,
                r.tries_left.map_or_else(|| "-".into(), |v| v.to_string())
            ))
            .collect::<Vec<_>>()
            .join(";")
    );
    parse_records(&text)?;
    Ok(text)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

pub fn encode(records: &[Record], flag: u8) -> Result<Vec<u8>> {
    let value = render_records(records)?;
    let mut bytes = vec![0; ENV_SIZE];
    bytes[4] = flag;
    bytes[5..5 + KEY.len()].copy_from_slice(KEY);
    bytes[5 + KEY.len()..5 + KEY.len() + value.len()].copy_from_slice(value.as_bytes());
    let crc = crc32(&bytes[5..]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

fn valid_crc(bytes: &[u8]) -> bool {
    bytes.len() == ENV_SIZE && bytes[..4] == crc32(&bytes[5..]).to_le_bytes()
}

fn decode(bytes: &[u8]) -> Result<Vec<Record>> {
    let data = &bytes[5..];
    ensure!(
        data.starts_with(KEY),
        "boot records absent from environment"
    );
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .context("unterminated boot environment")?;
    ensure!(
        end + 1 < data.len() && data[end..].iter().all(|byte| *byte == 0),
        "unexpected boot environment data"
    );
    parse_records(std::str::from_utf8(&data[KEY.len()..end])?)
}

#[derive(Debug)]
pub struct Environment {
    pub records: Vec<Record>,
    pub slot: usize,
    pub flag: u8,
    layout: FitLayout,
}

impl Environment {
    pub fn load(region: &Path, layout: FitLayout) -> Result<Self> {
        let mut file = File::open(region)?;
        let mut copies = [vec![0; ENV_SIZE], vec![0; ENV_SIZE]];
        let mut valid = [false; 2];
        for i in 0..2 {
            if file.seek(SeekFrom::Start(layout.offsets()[i])).is_ok()
                && file.read_exact(&mut copies[i]).is_ok()
            {
                valid[i] = valid_crc(&copies[i]);
            }
        }
        // Match env_check_redund in the pinned U-Boot source, including wrap.
        let slot = match valid {
            [true, false] => 0,
            [false, true] => 1,
            [false, false] => anyhow::bail!("no valid redundant boot environment"),
            [true, true] => {
                let (a, b) = (copies[0][4], copies[1][4]);
                if (a == 255 && b == 0) || (b > a && !(b == 255 && a == 0)) {
                    1
                } else {
                    0
                }
            }
        };
        Ok(Self {
            records: decode(&copies[slot])?,
            slot,
            flag: copies[slot][4],
            layout,
        })
    }

    /// The caller holds DATA's deployment transaction lock. Write the inactive
    /// environment, flush the block device, then require identical read-back.
    pub fn save(&self, region: &Path) -> Result<()> {
        ensure!(self.slot < 2, "invalid active environment slot");
        let slot = 1 - self.slot;
        let flag = self.flag.wrapping_add(1);
        let bytes = encode(&self.records, flag)?;
        let mut file = OpenOptions::new().write(true).open(region)?;
        file.seek(SeekFrom::Start(self.layout.offsets()[slot]))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let saved = Self::load(region, self.layout)?;
        ensure!(
            saved.slot == slot && saved.flag == flag && saved.records == self.records,
            "boot environment read-back mismatch"
        );
        Ok(())
    }
}
