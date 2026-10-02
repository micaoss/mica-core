//! Read-only observation of timesyncd's synchronization state.
//!
//! The shape [`crate::network_state`] takes: a trait with an unavailable
//! default so a dry-run daemon or a test can never inspect its host, and a
//! production implementation only `main.rs` attaches. The observation is
//! served by the `GetTimeStatus` bus method and surfaced read-only through
//! apid; nothing here writes anything, and nothing here can stop timesyncd's
//! retries — the classification REPORTS a degraded state, it never acts on
//! one.

use anyhow::Result;
use serde_json::{Value as Json, json};
use zbus::zvariant::Value;

/// One classified synchronization state, one of four names.
///
/// `Unknown` is deliberately a fifth, and it is the state for a signal that
/// could not be READ rather than for a particular daemon being down: it is
/// what the surface reports when the evidence a state rests on never
/// arrived. Reporting one of the four there would claim evidence nobody has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// The kernel reports a bounded clock error: timedate1's
    /// `NTPSynchronized`, which is `adjtimex.maxerror < 16 s`.
    Synchronized,
    /// timesyncd has a server and is exchanging packets with it, and the
    /// kernel does not report a bounded clock error.
    Polling,
    /// timesyncd is running and has no usable server — no network, no
    /// resolvable name — and keeps retrying on the pinned 30-second policy.
    OfflineDegraded,
    /// A server answered and its replies cannot be used: an unsynchronized
    /// leap indicator or an out-of-range stratum.
    InvalidSource,
    /// A signal the reported state would rest on could not be read:
    /// timesyncd is not observable on the bus at all, or timedate1 did not
    /// answer `NTPSynchronized`.
    Unknown,
}

impl SyncStatus {
    /// The wire spelling the API and UI consume.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Synchronized => "synchronized",
            Self::Polling => "polling",
            Self::OfflineDegraded => "offline-degraded",
            Self::InvalidSource => "invalid-source",
            Self::Unknown => "unknown",
        }
    }
}

/// The last NTP reply, as far as timesyncd's `NTPMessage` property tells it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NtpSample {
    /// Leap indicator; 3 is "clock not synchronized" and marks the source
    /// unusable.
    pub leap: u32,
    /// Server stratum; 0 (kiss-of-death) and 16+ (unsynchronized) mark the
    /// source unusable.
    pub stratum: u32,
    /// Whether timesyncd flagged the sample as a spike and discarded it.
    pub spike: bool,
    /// The sample's clock offset in seconds, from the four NTP timestamps.
    pub offset_seconds: f64,
    /// How many replies this server has given over the current connection.
    pub packet_count: u64,
}

/// Everything the classifier reads. Each field is what one soft bus read
/// produced; absent means the read did not answer.
#[derive(Debug, Clone, Default)]
pub struct TimesyncEvidence {
    /// Whether `org.freedesktop.timesync1` answered at all.
    pub service_reachable: bool,
    /// timedate1's `NTPSynchronized`: the kernel's own synchronized bit.
    pub ntp_synchronized: Option<bool>,
    /// The server timesyncd currently talks to, by configured name.
    pub server_name: Option<String>,
    /// The same server's resolved address, formatted.
    pub server_address: Option<String>,
    /// The last reply, when the server has answered at least once.
    pub sample: Option<NtpSample>,
}

/// timesyncd's own step/slew boundary (`NTP_MAX_ADJUST`, 0.4 s): an offset
/// beyond it is corrected by stepping the clock, within it by slewing.
const STEP_THRESHOLD_SECONDS: f64 = 0.4;

/// Classify `evidence` into the one status the surface reports.
#[must_use]
pub fn classify(evidence: &TimesyncEvidence) -> SyncStatus {
    if !evidence.service_reachable {
        return SyncStatus::Unknown;
    }
    if let Some(sample) = &evidence.sample
        && (sample.leap == 3 || sample.stratum == 0 || sample.stratum >= 16)
    {
        return SyncStatus::InvalidSource;
    }
    // Every state below rests on timedate1's bit: `synchronized` asserts it,
    // `polling` and `offline-degraded` assert its absence, because both rank
    // under `synchronized` and are only reached once it is ruled out. A read
    // that did not answer is not a read that answered `false`, so none of the
    // three may be reported off it. `invalid-source` above is not
    // affected: it rests on the sample alone, already outranks the bit, and
    // claims nothing about the clock -- only that the source's replies are
    // unusable, which WAS read.
    let Some(synchronized) = evidence.ntp_synchronized else {
        return SyncStatus::Unknown;
    };
    if synchronized {
        return SyncStatus::Synchronized;
    }
    if evidence.server_name.is_some() || evidence.server_address.is_some() {
        return SyncStatus::Polling;
    }
    SyncStatus::OfflineDegraded
}

/// Which unread signal left the state `unknown`.
///
/// The state is one word for "a signal this rests on did not answer", and the
/// two signals live in different services, so the detail names the one that
/// went missing: sending an operator to a daemon that answered fine is the
/// failure a single fixed sentence would cause.
fn unknown_detail(evidence: &TimesyncEvidence) -> &'static str {
    if evidence.service_reachable {
        "systemd-timedated did not answer NTPSynchronized: the kernel's bound on the clock error was not read"
    } else {
        "systemd-timesyncd is not reachable on the bus"
    }
}

/// `evidence` rendered as the JSON the bus method serves.
#[must_use]
pub fn status_json(evidence: &TimesyncEvidence) -> Json {
    let status = classify(evidence);
    let mut root = serde_json::Map::new();
    root.insert("status".to_string(), json!(status.as_str()));
    if let Some(synchronized) = evidence.ntp_synchronized {
        root.insert("synchronized".to_string(), json!(synchronized));
    }
    if evidence.server_name.is_some() || evidence.server_address.is_some() {
        root.insert(
            "server".to_string(),
            json!({
                "name": evidence.server_name,
                "address": evidence.server_address,
            }),
        );
    }
    if let Some(sample) = &evidence.sample {
        let correction = if sample.offset_seconds.abs() > STEP_THRESHOLD_SECONDS {
            "step"
        } else {
            "slew"
        };
        root.insert(
            "sample".to_string(),
            json!({
                "leap": sample.leap,
                "stratum": sample.stratum,
                "spike": sample.spike,
                "offsetSeconds": sample.offset_seconds,
                "packetCount": sample.packet_count,
                "correction": correction,
            }),
        );
    }
    if status == SyncStatus::Unknown {
        root.insert("detail".to_string(), json!(unknown_detail(evidence)));
    }
    Json::Object(root)
}

/// Read-only source of time-synchronization evidence.
#[async_trait::async_trait]
pub trait TimeStatusSource: Send + Sync {
    /// Observe the current evidence.
    ///
    /// # Errors
    ///
    /// Returns an error only when this daemon has no observer at all; a
    /// production observer answers with absent evidence instead.
    async fn observe(&self) -> Result<TimesyncEvidence>;
}

/// The observer a daemon without host access has: none.
pub struct UnavailableTimeStatus;

#[async_trait::async_trait]
impl TimeStatusSource for UnavailableTimeStatus {
    async fn observe(&self) -> Result<TimesyncEvidence> {
        Err(anyhow::anyhow!(
            "this daemon observes no time synchronization"
        ))
    }
}

/// Production observer over the system bus.
///
/// The connection is made lazily inside every call, the hostname executor's
/// shape: constructing this never touches the host, and a bus that comes and
/// goes costs a reconnect, not a wedged cache.
pub struct SystemdTimesync;

/// Field indexes of timesync1's `NTPMessage` structure
/// (`(uuuuittayttttbtt)`): leap, version, mode, stratum, precision,
/// root delay, root dispersion, reference id, and then the four NTP
/// timestamps (origin, receive, transmit, destination, in microseconds),
/// the spike flag, the packet count and the jitter.
const FIELD_LEAP: usize = 0;
const FIELD_STRATUM: usize = 3;
const FIELD_ORIGIN: usize = 8;
const FIELD_RECEIVE: usize = 9;
const FIELD_TRANSMIT: usize = 10;
const FIELD_DESTINATION: usize = 11;
const FIELD_SPIKE: usize = 12;
const FIELD_PACKET_COUNT: usize = 13;

fn field_u32(fields: &[Value<'_>], index: usize) -> Option<u32> {
    match fields.get(index)? {
        Value::U32(value) => Some(*value),
        _ => None,
    }
}

fn field_u64(fields: &[Value<'_>], index: usize) -> Option<u64> {
    match fields.get(index)? {
        Value::U64(value) => Some(*value),
        _ => None,
    }
}

fn field_bool(fields: &[Value<'_>], index: usize) -> Option<bool> {
    match fields.get(index)? {
        Value::Bool(value) => Some(*value),
        _ => None,
    }
}

/// Decode one `NTPMessage` structure's fields into an [`NtpSample`].
///
/// Over the field slice rather than a typed tuple, so a systemd that grows
/// the structure keeps decoding (the leading fields are ABI) and a test can
/// hand in a literal `Vec<Value>` with no bus anywhere. `None` when the shape
/// is not the documented one — absent evidence, never an error.
pub fn parse_ntp_message(fields: &[Value<'_>]) -> Option<NtpSample> {
    let origin = i128::from(field_u64(fields, FIELD_ORIGIN)?);
    let receive = i128::from(field_u64(fields, FIELD_RECEIVE)?);
    let transmit = i128::from(field_u64(fields, FIELD_TRANSMIT)?);
    let destination = i128::from(field_u64(fields, FIELD_DESTINATION)?);
    let offset_usec = ((receive - origin) + (transmit - destination)) / 2;
    Some(NtpSample {
        leap: field_u32(fields, FIELD_LEAP)?,
        stratum: field_u32(fields, FIELD_STRATUM)?,
        spike: field_bool(fields, FIELD_SPIKE)?,
        offset_seconds: offset_usec as f64 / 1_000_000.0,
        packet_count: field_u64(fields, FIELD_PACKET_COUNT)?,
    })
}

/// Format timesync1's `ServerAddress` — an address family and raw bytes —
/// the way `timedatectl` would print it.
fn format_server_address(family: i32, bytes: &[u8]) -> Option<String> {
    match (family, bytes.len()) {
        // AF_INET
        (2, 4) => Some(std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string()),
        // AF_INET6
        (10, 16) => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(bytes);
            Some(std::net::Ipv6Addr::from(octets).to_string())
        }
        _ => None,
    }
}

#[async_trait::async_trait]
impl TimeStatusSource for SystemdTimesync {
    async fn observe(&self) -> Result<TimesyncEvidence> {
        let connection = zbus::Connection::system().await?;
        let mut evidence = TimesyncEvidence::default();

        if let Ok(timesync) = zbus::Proxy::new(
            &connection,
            "org.freedesktop.timesync1",
            "/org/freedesktop/timesync1",
            "org.freedesktop.timesync1.Manager",
        )
        .await
        {
            if let Ok(name) = timesync.get_property::<String>("ServerName").await {
                evidence.service_reachable = true;
                if !name.is_empty() {
                    evidence.server_name = Some(name);
                }
            }
            if let Ok((family, bytes)) = timesync
                .get_property::<(i32, Vec<u8>)>("ServerAddress")
                .await
            {
                evidence.service_reachable = true;
                evidence.server_address = format_server_address(family, &bytes);
            }
            if let Ok(message) = timesync
                .get_property::<zbus::zvariant::OwnedValue>("NTPMessage")
                .await
            {
                evidence.service_reachable = true;
                if let Value::Structure(structure) = &Value::from(message) {
                    evidence.sample = parse_ntp_message(structure.fields());
                }
            }
        }

        if let Ok(timedate) = zbus::Proxy::new(
            &connection,
            "org.freedesktop.timedate1",
            "/org/freedesktop/timedate1",
            "org.freedesktop.timedate1",
        )
        .await
            && let Ok(synchronized) = timedate.get_property::<bool>("NTPSynchronized").await
        {
            evidence.ntp_synchronized = Some(synchronized);
        }

        Ok(evidence)
    }
}

/// Where the trusted-clock floor is persisted: timesyncd's saved clock file,
/// whose MTIME is the value.
///
/// `/var/lib/systemd/timesync` is a bind mount whose source is
/// `/mnt/data/state/timesync`, so this path survives a reboot and an A/B update.
pub const SAVED_CLOCK_PATH: &str = "/var/lib/systemd/timesync/clock";

/// Where the kernel reports how long this boot has been running.
const UPTIME_PATH: &str = "/proc/uptime";

/// The two signals the predicate reads, gathered once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockTrust {
    /// The classified state, or `None` when this daemon has no observer at
    /// all (dry-run) — which is not evidence of anything and is treated as
    /// unread, exactly as [`SyncStatus::Unknown`] is.
    pub status: Option<SyncStatus>,
    /// The saved floor's mtime is later than the instant this boot started:
    /// timesyncd is running and writing the floor forward.
    pub floor_advanced: bool,
}

impl ClockTrust {
    /// Whether the device believes its clock, in the exact terms:
    /// *timesyncd reporting `synchronized`, or a floor that has advanced
    /// since boot.*
    #[must_use]
    pub fn believed(&self) -> bool {
        self.status == Some(SyncStatus::Synchronized) || self.floor_advanced
    }

    /// Why the clock may not be believed, or `None` when it may.
    ///
    /// The sentence names BOTH limbs as they were observed, because the
    /// operator reading a `clock-untrusted` deferral has to know which one
    /// to go and fix — a device with no network and a live floor is a
    /// different problem from one whose STATE bind never arrived.
    #[must_use]
    pub fn untrusted_reason(&self) -> Option<String> {
        if self.believed() {
            return None;
        }
        Some(format!(
            "the clock is not one this device believes: time synchronization reports `{}` \
             and the saved floor at {SAVED_CLOCK_PATH} has not advanced since boot. \
             A maintenance window is UTC wall-clock, so an automatic install is deferred \
             until one of the two holds; checks and fetches are unaffected",
            self.status.map_or("unknown", SyncStatus::as_str),
        ))
    }
}

/// Whether the saved floor has moved forward during this boot.
///
/// `false` on an unreadable file or an unreadable uptime: an unread signal is
/// not an advance, and the closed side here is deferring the install.
#[must_use]
pub fn saved_floor_advanced(clock_path: &std::path::Path, uptime_path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(clock_path) else {
        return false;
    };
    let Ok(saved) = metadata.modified() else {
        return false;
    };
    let Some(uptime) = read_uptime(uptime_path) else {
        return false;
    };
    let Some(boot) = std::time::SystemTime::now().checked_sub(uptime) else {
        return false;
    };
    saved > boot
}

/// This boot's age, from the first field of `/proc/uptime`.
fn read_uptime(path: &std::path::Path) -> Option<std::time::Duration> {
    let contents = std::fs::read_to_string(path).ok()?;
    let seconds: f64 = contents.split_whitespace().next()?.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| std::time::Duration::from_secs_f64(seconds))
}

/// The production reading of both limbs, from the host's own files.
#[must_use]
pub fn observed_clock_trust(status: Option<SyncStatus>) -> ClockTrust {
    ClockTrust {
        status,
        floor_advanced: saved_floor_advanced(
            std::path::Path::new(SAVED_CLOCK_PATH),
            std::path::Path::new(UPTIME_PATH),
        ),
    }
}

#[cfg(test)]
mod tests;
