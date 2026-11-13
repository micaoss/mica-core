//! Media mounts on a systemd root, made by PID 1.
//!
//! micad runs in a mount namespace of its own (`PrivateTmp=` and the
//! `Protect*=` settings of its unit), which systemd makes a slave of the
//! host's: a mount micad made itself would stay in that namespace, and
//! neither the host nor apid would see the stick. So each mount is a
//! transient `.mount` unit PID 1 starts in the host's namespace, from where
//! it propagates into every service's, micad's and apid's included:
//! - the unit is named after its place, `run-mica-media-<id>.mount`, so micad
//!   only ever starts and stops units under [`super::MEDIA_ROOT`];
//! - `What=` the device, `Type=` the driver and `Options=` the flags and data
//!   [`super::mount::options`] gives, read-only and `nosuid,nodev,noexec`;
//! - `BindsTo=` and `After=` the device's unit, so a pulled stick takes its
//!   mount with it;
//! - `CollectMode=inactive-or-failed`, so nothing is left once it stops.
//!
//! Each call waits for its job. A mount job that does not end `done` is the
//! mount's failure; an unmount job that does not is `EBUSY`, which is what a
//! mount still in use refuses with. PID 1 never detaches a mount: the mount
//! of a device that is gone is stopped by the device's own unit, and one
//! still in use stays until nothing holds it.

use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::sync::OnceCell;
use zbus::export::futures_core::Stream;
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath, Variant};

use super::mount::{Mounter, options};

const DESTINATION: &str = "org.freedesktop.systemd1";
const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
/// How long a mount or unmount job may take.
const JOB_TIMEOUT: Duration = Duration::from_secs(30);

/// The mounts PID 1 makes.
#[derive(Default)]
pub struct SystemdMounter {
    connection: OnceCell<Connection>,
}

/// `path` as systemd escapes it into a unit name: `/` between components
/// becomes `-`, and every byte but ASCII letters, digits, `:`, `_` and a
/// `.` that does not start the name is `\xNN`, as `systemd-escape -p` does.
#[must_use]
pub fn escape_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text.trim_matches('/');
    let mut out = String::new();
    for (index, byte) in trimmed.bytes().enumerate() {
        match byte {
            b'/' => out.push('-'),
            b'.' if index > 0 => out.push('.'),
            b if b.is_ascii_alphanumeric() || b == b':' || b == b'_' => out.push(char::from(b)),
            b => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// The mount unit for `target`.
#[must_use]
pub fn unit_name(target: &Path) -> String {
    format!("{}.mount", escape_path(target))
}

/// The properties of the transient unit mounting `device` with `filesystem`
/// at `target`.
///
/// # Errors
///
/// `Unsupported` for a filesystem MICA does not mount.
pub fn properties(
    device: &Path,
    target: &Path,
    filesystem: &str,
) -> io::Result<Vec<(&'static str, Variant<'static>)>> {
    let (driver, data) = options(filesystem).ok_or(io::ErrorKind::Unsupported)?;
    let data = data.to_str().map_err(io::Error::other)?;
    let device_unit = format!("{}.device", escape_path(device));
    Ok(vec![
        ("Description", Variant::from("MICA media volume")),
        ("What", Variant::from(device.to_string_lossy().into_owned())),
        (
            "Where",
            Variant::from(target.to_string_lossy().into_owned()),
        ),
        ("Type", Variant::from(driver)),
        (
            "Options",
            Variant::from(format!("ro,nosuid,nodev,noexec,{data}")),
        ),
        ("BindsTo", Variant::from(vec![device_unit.clone()])),
        ("After", Variant::from(vec![device_unit])),
        ("CollectMode", Variant::from("inactive-or-failed")),
    ])
}

/// What a job's result means for the call that queued it.
///
/// # Errors
///
/// Any result but `done`: `EBUSY` for an unmount, the result for a mount.
pub fn job_outcome(result: &str, unmount: bool) -> io::Result<()> {
    match (result, unmount) {
        ("done", _) => Ok(()),
        (_, true) => Err(io::Error::from_raw_os_error(
            rustix::io::Errno::BUSY.raw_os_error(),
        )),
        (other, false) => Err(io::Error::other(format!("the mount job ended {other}"))),
    }
}

impl SystemdMounter {
    /// The system bus, subscribed to the manager's signals once: PID 1 only
    /// sends `JobRemoved` to subscribed clients, and refuses a second
    /// subscription from the same one.
    async fn connection(&self) -> zbus::Result<&Connection> {
        self.connection
            .get_or_try_init(|| async {
                let connection = Connection::system().await?;
                connection
                    .call_method(Some(DESTINATION), PATH, Some(MANAGER), "Subscribe", &())
                    .await?;
                Ok(connection)
            })
            .await
    }

    /// Call the manager's `method`, which queues a job, and answer the job's
    /// result once it is removed.
    async fn job<B>(&self, method: &str, body: &B) -> io::Result<String>
    where
        B: zbus::export::serde::ser::Serialize + zbus::DynamicType,
    {
        let connection = self.connection().await.map_err(io::Error::other)?;
        // The rule is in place before the call, so the job cannot end unseen.
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(DESTINATION)
            .interface(MANAGER)
            .member("JobRemoved")
            .build()
            .map_err(io::Error::other)?;
        let stream = MessageStream::for_match_rule(rule, connection, None)
            .await
            .map_err(io::Error::other)?;
        let reply = connection
            .call_method(Some(DESTINATION), PATH, Some(MANAGER), method, body)
            .await
            .map_err(|err| match &err {
                zbus::Error::MethodError(name, _, _)
                    if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit" =>
                {
                    io::Error::from(io::ErrorKind::NotFound)
                }
                _ => io::Error::other(err),
            })?;
        let job: OwnedObjectPath = reply.body().deserialize().map_err(io::Error::other)?;
        let mut stream = std::pin::pin!(stream);
        let wait = async {
            while let Some(message) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
            {
                let Ok(message) = message else { continue };
                // JobRemoved(u id, o job, s unit, s result)
                if let Ok((_, path, _, result)) =
                    message
                        .body()
                        .deserialize::<(u32, OwnedObjectPath, String, String)>()
                    && path == job
                {
                    return Ok(result);
                }
            }
            Err(io::Error::other("the systemd bus closed"))
        };
        tokio::time::timeout(JOB_TIMEOUT, wait)
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }
}

impl Mounter for SystemdMounter {
    fn mount(&self, device: &Path, target: &Path, filesystem: &str) -> io::Result<()> {
        let properties = properties(device, target, filesystem)?;
        let aux: Vec<(String, Vec<(String, Variant<'static>)>)> = Vec::new();
        let body = (unit_name(target), "fail", properties, aux);
        let result =
            tokio::runtime::Handle::current().block_on(self.job("StartTransientUnit", &body))?;
        job_outcome(&result, false)
    }

    fn unmount(&self, target: &Path, _detach: bool) -> io::Result<()> {
        // A unit already stopped and collected, as a pulled stick's is,
        // answers no such unit: there is nothing left to undo.
        let body = (unit_name(target), "replace");
        match tokio::runtime::Handle::current().block_on(self.job("StopUnit", &body)) {
            Ok(result) => job_outcome(&result, true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn available(&self, target: &Path) -> Option<u64> {
        super::HostMounter.available(target)
    }
}

#[cfg(test)]
mod tests;
