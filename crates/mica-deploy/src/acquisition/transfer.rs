//! Downloading over HTTPS: TLS, pacing, redirects and chunked bodies.

use anyhow::{Context, Result, ensure};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

/// Connection establishment bound, per resolved address.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Whole-transfer bound, checked before every socket read and write.
pub(super) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1800);
/// A transfer averaging under this many bytes per second over one window is
/// abandoned; a socket silent for a whole window is abandoned at once.
pub(super) const LOW_SPEED_BYTES: u64 = 1024;
pub(super) const LOW_SPEED_WINDOW: Duration = Duration::from_secs(30);
pub(super) const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
pub(super) const MAX_RESPONSE_HEAD: usize = 16384;

/// Redirects followed before a transfer is refused.
pub(super) const MAX_REDIRECTS: usize = 10;

/// One HTTP/1.1 GET into `path`, http or https only. Redirects are followed, up
/// to [`MAX_REDIRECTS`]: sources may sit behind a CDN, and every catalog and
/// object is authenticated by signature and digest rather than by where it was
/// served from. With `resume` a non-empty file continues through `Range` and
/// only `206` from exactly that offset is accepted; otherwise only `200` is.
pub(super) fn download(url: &str, path: &Path, limit: u64, resume: bool) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        ensure!(
            path.symlink_metadata()?.is_file(),
            "invalid download target"
        );
    }
    let offset = if resume && path.symlink_metadata().is_ok() {
        path.metadata()?.len()
    } else {
        0
    };
    ensure!(offset <= limit, "transfer exceeded byte bound");
    let started = Instant::now();
    let mut url = url::Url::parse(url)?;
    for _ in 0..=MAX_REDIRECTS {
        match transfer_once(&url, path, offset, limit, started)? {
            None => return Ok(()),
            Some(location) => {
                url = url
                    .join(&location)
                    .context("invalid transfer redirect location")?
            }
        }
    }
    anyhow::bail!("component transfer redirected more than {MAX_REDIRECTS} times")
}

/// One request to `url`: `None` once the body is in `path`, or the location a
/// redirect names.
pub(super) fn transfer_once(
    url: &url::Url,
    path: &Path,
    offset: u64,
    limit: u64,
    started: Instant,
) -> Result<Option<String>> {
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url.username().is_empty()
            && url.password().is_none(),
        "unsupported transfer URL"
    );
    let host = url.host().context("transfer URL has no host")?;
    let address = url
        .socket_addrs(|| None)?
        .into_iter()
        .find_map(|address| TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok())
        .context("component transfer could not connect")?;
    address.set_read_timeout(Some(LOW_SPEED_WINDOW))?;
    address.set_write_timeout(Some(LOW_SPEED_WINDOW))?;
    let socket = Paced {
        stream: address,
        started,
        window: (Instant::now(), 0),
    };
    let mut request = format!(
        "GET {}{} HTTP/1.1\r\nHost: {}{}\r\nUser-Agent: mica-deploy\r\nAccept: */*\r\n",
        url.path(),
        url.query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default(),
        host,
        url.port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default(),
    );
    if offset > 0 {
        request.push_str(&format!("Range: bytes={offset}-\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    let transfer = Transfer {
        request: &request,
        path,
        offset,
        limit,
    };
    if url.scheme() == "https" {
        let name = match host {
            url::Host::Domain(domain) => ServerName::try_from(domain.to_owned())?,
            url::Host::Ipv4(ip) => ServerName::from(std::net::IpAddr::V4(ip)),
            url::Host::Ipv6(ip) => ServerName::from(std::net::IpAddr::V6(ip)),
        };
        let connection = rustls::ClientConnection::new(tls_config()?, name)?;
        transfer.run(rustls::StreamOwned::new(connection, socket))
    } else {
        transfer.run(socket)
    }
}

pub(super) fn tls_config() -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    let certificates = CertificateDer::pem_file_iter(CA_BUNDLE)
        .with_context(|| format!("read {CA_BUNDLE}"))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parse {CA_BUNDLE}"))?;
    let (added, _) = roots.add_parsable_certificates(certificates);
    ensure!(added > 0, "no usable certificate authority in {CA_BUNDLE}");
    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth(),
    ))
}

/// The socket beneath TLS and HTTP framing, so the transfer bounds hold for the
/// response head, chunk framing and body alike: the whole-transfer deadline is
/// checked before every read and write, and a window averaging under
/// [`LOW_SPEED_BYTES`] per second abandons the connection.
pub(super) struct Paced {
    pub(super) stream: TcpStream,
    pub(super) started: Instant,
    pub(super) window: (Instant, u64),
}

impl Paced {
    pub(super) fn deadline(&self) -> std::io::Result<()> {
        if self.started.elapsed() < TRANSFER_TIMEOUT {
            Ok(())
        } else {
            Err(std::io::Error::other("component transfer timed out"))
        }
    }
}

impl Read for Paced {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.deadline()?;
        let count = self.stream.read(buffer)?;
        self.window.1 += count as u64;
        if count > 0 && self.window.0.elapsed() >= LOW_SPEED_WINDOW {
            if self.window.1 < LOW_SPEED_BYTES * LOW_SPEED_WINDOW.as_secs() {
                return Err(std::io::Error::other("component transfer too slow"));
            }
            self.window = (Instant::now(), 0);
        }
        Ok(count)
    }
}

impl Write for Paced {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.deadline()?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

pub(super) struct Transfer<'a> {
    pub(super) request: &'a str,
    pub(super) path: &'a Path,
    pub(super) offset: u64,
    pub(super) limit: u64,
}

impl Transfer<'_> {
    pub(super) fn run(&self, mut stream: impl Read + Write) -> Result<Option<String>> {
        stream.write_all(self.request.as_bytes())?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            ensure!(head.len() < MAX_RESPONSE_HEAD, "excessive response head");
            let mut byte = [0];
            reader
                .read_exact(&mut byte)
                .context("component transfer interrupted")?;
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).context("invalid response head")?;
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.strip_prefix("HTTP/1."))
            .and_then(|line| line.get(2..5))
            .context("invalid response status line")?;
        let mut length = None;
        let mut chunked = false;
        let mut range = None;
        let mut location = None;
        for line in lines.filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').context("invalid response header")?;
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = Some(value.parse::<u64>()?),
                "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
                "content-range" => range = Some(value.to_owned()),
                "location" => location = Some(value.to_owned()),
                _ => {}
            }
        }
        if ["301", "302", "303", "307", "308"].contains(&status) {
            return location
                .map(Some)
                .with_context(|| format!("transfer redirect {status} without a location"));
        }
        if self.offset > 0 {
            ensure!(
                status == "206"
                    && range
                        .is_some_and(|range| range.starts_with(&format!("bytes {}-", self.offset))),
                "unexpected transfer status {status}: the server did not resume"
            );
        } else {
            ensure!(status == "200", "unexpected transfer status {status}");
        }
        if !chunked && let Some(length) = length {
            ensure!(
                self.offset
                    .checked_add(length)
                    .is_some_and(|end| end <= self.limit),
                "transfer exceeded byte bound"
            );
        }
        let mut output = if self.offset > 0 {
            OpenOptions::new().append(true).open(self.path)?
        } else {
            File::create(self.path)?
        };
        let mut body: Box<dyn Read + '_> = if chunked {
            Box::new(Chunked {
                inner: &mut reader,
                remaining: 0,
                done: false,
            })
        } else if let Some(length) = length {
            Box::new((&mut reader).take(length))
        } else {
            Box::new(&mut reader)
        };
        let mut written = self.offset;
        let mut received = 0_u64;
        let mut buffer = vec![0; 65536];
        loop {
            let count = match body.read(&mut buffer) {
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    anyhow::bail!("component transfer stalled")
                }
                Err(error) => return Err(error).context("component transfer interrupted"),
            };
            if count == 0 {
                break;
            }
            written += count as u64;
            received += count as u64;
            ensure!(written <= self.limit, "transfer exceeded byte bound");
            output.write_all(&buffer[..count])?;
        }
        if !chunked && let Some(length) = length {
            ensure!(received == length, "component transfer interrupted");
        }
        Ok(None)
    }
}

/// `Transfer-Encoding: chunked`, trailers discarded.
pub(super) struct Chunked<R> {
    pub(super) inner: R,
    pub(super) remaining: u64,
    pub(super) done: bool,
}

impl<R: BufRead> Chunked<R> {
    pub(super) fn line(&mut self) -> std::io::Result<String> {
        let mut line = Vec::new();
        (&mut self.inner).take(1024).read_until(b'\n', &mut line)?;
        let line = String::from_utf8(line)
            .ok()
            .and_then(|line| line.strip_suffix("\r\n").map(str::to_owned))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "chunk framing"))?;
        Ok(line)
    }
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let line = self.line()?;
            let size = line.split(';').next().unwrap_or_default().trim();
            self.remaining = u64::from_str_radix(size, 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "chunk size"))?;
            if self.remaining == 0 {
                while !self.line()?.is_empty() {}
                self.done = true;
                return Ok(0);
            }
        }
        let limit = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let count = self.inner.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.remaining -= count as u64;
        if self.remaining == 0 && !self.line()?.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunk framing",
            ));
        }
        Ok(count)
    }
}
