//! micad's readiness for the service manager.
//!
//! systemd runs micad as `Type=notify`: it is started once `READY=1` arrives
//! on `$NOTIFY_SOCKET`, sent after the startup reconcile and the interface is
//! served. OpenRC's script asks the served interface itself, and a process
//! without the variable (OpenRC, the tests) sends nothing.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixDatagram;

/// Send `READY=1` when a service manager asked for it; a failure is logged.
pub(crate) fn notify() {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    if let Err(err) = send(&socket, b"READY=1") {
        tracing::warn!(error = %err, "telling the service manager micad is ready failed");
    }
}

/// One datagram to `socket`, a path or, after `@`, an abstract name.
fn send(socket: &OsStr, message: &[u8]) -> std::io::Result<()> {
    let sender = UnixDatagram::unbound()?;
    match socket.as_bytes().strip_prefix(b"@") {
        Some(name) => {
            use std::os::linux::net::SocketAddrExt;
            let address = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
            sender.send_to_addr(message, &address)?;
        }
        None => {
            sender.send_to(message, socket)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_reaches_a_path_socket_and_an_abstract_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notify");
        let receiver = UnixDatagram::bind(&path).expect("bind");
        send(path.as_os_str(), b"READY=1").expect("send");
        let mut buffer = [0u8; 16];
        let read = receiver.recv(&mut buffer).expect("recv");
        assert_eq!(&buffer[..read], b"READY=1");

        use std::os::linux::net::SocketAddrExt;
        let name = format!("micad-ready-test-{}", std::process::id());
        let address =
            std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).expect("name");
        let receiver = UnixDatagram::bind_addr(&address).expect("bind abstract");
        send(OsStr::new(&format!("@{name}")), b"READY=1").expect("send abstract");
        let read = receiver.recv(&mut buffer).expect("recv");
        assert_eq!(&buffer[..read], b"READY=1");
    }
}
