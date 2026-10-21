//! `mica-apid --healthcheck`: whether this machine's apid answers `GET /healthz`,
//! for the boot health gate.
//!
//! The probe asks the local socket, which apid serves whatever `access.web`
//! says, so the answer does not depend on where the console listens. Under
//! `access.web.mode` `off` apid does not run by design, and the probe passes
//! without asking.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use micad_settings::WebMode;

/// Bound on each read or write.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Probe apid once; `Ok` only for a 2xx answer to `GET /healthz`, or when the
/// configuration turns apid off.
pub fn probe() -> anyhow::Result<()> {
    if crate::config::Listeners::resolve().mode == WebMode::Off {
        eprintln!("apid is off (access.web.mode)");
        return Ok(());
    }
    let socket = crate::config::local_socket();
    ask(connect(&socket)?, &socket)
}

fn connect(socket: &Path) -> anyhow::Result<UnixStream> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to apid at {}", socket.display()))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    Ok(stream)
}

/// Send `GET /healthz` over `stream` and require a 2xx status line.
fn ask(mut stream: impl Read + Write, socket: &Path) -> anyhow::Result<()> {
    let at = socket.display();
    write!(
        stream,
        "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .with_context(|| format!("request to apid at {at}"))?;
    let mut status = [0_u8; 12];
    stream
        .read_exact(&mut status)
        .with_context(|| format!("read apid's answer from {at}"))?;
    anyhow::ensure!(
        status.starts_with(b"HTTP/1.") && status[9] == b'2',
        "apid at {at} answered /healthz with `{}`",
        String::from_utf8_lossy(&status)
    );
    Ok(())
}
