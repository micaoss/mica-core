//! Reading an app bundle: a gzip tar, checked member by member while it is
//! unpacked, and its manifest.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use super::manifest::{self, Manifest, Problem};

/// The largest archive accepted.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// The most it may unpack to.
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
/// The most members it may hold.
const MAX_ENTRIES: usize = 20_000;
/// The largest manifest.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// The manifest's name, at the top.
pub const MANIFEST: &str = "mica-app.json";
/// The top-level directories a bundle may carry.
const TOP: [&str; 3] = ["files", "units", "hooks"];

/// A bundle unpacked and checked.
#[derive(Debug)]
pub struct Unpacked {
    pub manifest: Manifest,
    pub sha256: String,
}

/// Why a bundle is refused: every rule it breaks, or the reason it could not
/// be read.
#[derive(Debug)]
pub enum Refusal {
    Invalid(Vec<Problem>),
    Io(anyhow::Error),
}

impl From<Problem> for Refusal {
    fn from(problem: Problem) -> Self {
        Self::Invalid(vec![problem])
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(problems) => {
                let lines: Vec<String> = problems
                    .iter()
                    .map(|p| format!("{}: {}", p.rule, p.detail))
                    .collect();
                write!(f, "the bundle is refused: {}", lines.join("; "))
            }
            Self::Io(err) => write!(f, "{err:#}"),
        }
    }
}

fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Refusal {
    let context = context.into();
    move |err| Refusal::Io(anyhow::Error::new(err).context(context))
}

/// The sha256 of the file at `path`, in hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut context = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    Ok(hex::encode(context.finish().as_ref()))
}

/// Unpack `archive` into `dest`, which must not exist, checking every member,
/// then read and check its manifest for a device running `init` on `arch`.
/// What was unpacked stays for the caller to keep or remove.
pub fn unpack(
    archive: &Path,
    dest: &Path,
    init: micad_settings::Init,
    arch: &str,
) -> Result<Unpacked, Refusal> {
    let size = fs::metadata(archive).map_err(io("read the bundle"))?.len();
    if size > MAX_ARCHIVE_BYTES {
        return Err(Problem::new(
            "too_large",
            format!("the bundle is above {} MiB", MAX_ARCHIVE_BYTES >> 20),
        )
        .into());
    }
    let sha256 = sha256_file(archive).map_err(io("hash the bundle"))?;
    fs::DirBuilder::new()
        .mode(0o755)
        .create(dest)
        .map_err(io(format!("create {}", dest.display())))?;
    let file = File::open(archive).map_err(io("open the bundle"))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut problems = Vec::new();
    let mut entries = 0_usize;
    let mut unpacked = 0_u64;
    for entry in tar.entries().map_err(io("read the bundle"))? {
        let mut entry = entry.map_err(io("read the bundle"))?;
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(
                Problem::new("too_many", format!("more than {MAX_ENTRIES} members")).into(),
            );
        }
        let raw = entry
            .path()
            .map_err(io("read a member's path"))?
            .into_owned();
        let Some(relative) = member_path(&raw) else {
            problems.push(Problem::new(
                "path_outside",
                format!("{} leaves the bundle", raw.display()),
            ));
            continue;
        };
        let header = entry.header();
        let kind = header.entry_type();
        let mode = header.mode().map_err(io("read a member's mode"))?;
        if mode & 0o6000 != 0 {
            problems.push(Problem::new(
                "setuid",
                format!("{} is setuid or setgid", relative.display()),
            ));
            continue;
        }
        let top = relative
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or_default()
            .to_string();
        let at_top = relative.components().count() == 1;
        let allowed = if at_top {
            (top == MANIFEST && kind.is_file()) || (TOP.contains(&top.as_str()) && kind.is_dir())
        } else {
            TOP.contains(&top.as_str())
        };
        if !allowed {
            problems.push(Problem::new(
                "path_outside",
                format!(
                    "{} is outside mica-app.json, files/, units/ and hooks/",
                    relative.display()
                ),
            ));
            continue;
        }
        if through_link(dest, &relative) {
            problems.push(Problem::new(
                "link_outside",
                format!("{} is under a link", relative.display()),
            ));
            continue;
        }
        let target = dest.join(&relative);
        if let Some(parent) = target.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(parent)
                .map_err(io(format!("create {}", parent.display())))?;
        }
        if kind.is_dir() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(&target)
                .map_err(io(format!("create {}", target.display())))?;
        } else if kind.is_file() {
            unpacked += entry.size();
            if unpacked > MAX_UNPACKED_BYTES {
                return Err(Problem::new(
                    "too_large",
                    format!(
                        "the bundle unpacks to more than {} MiB",
                        MAX_UNPACKED_BYTES >> 20
                    ),
                )
                .into());
            }
            if top == MANIFEST && entry.size() > MAX_MANIFEST_BYTES {
                problems.push(Problem::new(
                    "manifest",
                    format!("mica-app.json is above {} KiB", MAX_MANIFEST_BYTES >> 10),
                ));
                continue;
            }
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&target)
                .map_err(|err| match err.kind() {
                    io::ErrorKind::AlreadyExists => Refusal::from(Problem::new(
                        "manifest",
                        format!("{} is in the bundle twice", relative.display()),
                    )),
                    _ => io(format!("create {}", target.display()))(err),
                })?;
            io::copy(&mut entry, &mut out).map_err(io(format!("write {}", target.display())))?;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o755))
                .map_err(io(format!("chmod {}", target.display())))?;
        } else if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(io("read a link"))?
                .map(|link| link.into_owned())
                .unwrap_or_default();
            if !link_stays_inside(&relative, &link) {
                problems.push(Problem::new(
                    "link_outside",
                    format!("{} points outside the bundle", relative.display()),
                ));
                continue;
            }
            std::os::unix::fs::symlink(&link, &target)
                .map_err(io(format!("link {}", target.display())))?;
        } else {
            problems.push(Problem::new(
                "special_file",
                format!(
                    "{} is not a file, a directory or a symbolic link",
                    relative.display()
                ),
            ));
        }
    }
    if !problems.is_empty() {
        return Err(Refusal::Invalid(problems));
    }
    let manifest = read_manifest(dest)?;
    let problems = manifest::check(&manifest, dest, init, arch);
    if !problems.is_empty() {
        return Err(Refusal::Invalid(problems));
    }
    Ok(Unpacked { manifest, sha256 })
}

/// What the bundle at `archive` unpacks to, by its members' headers.
pub fn unpacked_size(archive: &Path) -> Result<u64, Refusal> {
    let file = File::open(archive).map_err(io("open the bundle"))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut total = 0_u64;
    for entry in tar.entries().map_err(io("read the bundle"))? {
        total = total.saturating_add(entry.map_err(io("read the bundle"))?.size());
        if total > MAX_UNPACKED_BYTES {
            break;
        }
    }
    Ok(total)
}

/// The manifest of the bundle at `archive`, read without unpacking it: the
/// members before it are skipped, the rest is not read.
pub fn peek_manifest(archive: &Path) -> Result<Manifest, Refusal> {
    let size = fs::metadata(archive).map_err(io("read the bundle"))?.len();
    if size > MAX_ARCHIVE_BYTES {
        return Err(Problem::new(
            "too_large",
            format!("the bundle is above {} MiB", MAX_ARCHIVE_BYTES >> 20),
        )
        .into());
    }
    let file = File::open(archive).map_err(io("open the bundle"))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let entries = tar
        .entries()
        .map_err(|err| Refusal::from(Problem::new("manifest", format!("not a gzip tar: {err}"))))?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ENTRIES {
            break;
        }
        let entry = entry.map_err(|err| {
            Refusal::from(Problem::new("manifest", format!("not a gzip tar: {err}")))
        })?;
        let is_manifest = entry
            .path()
            .ok()
            .and_then(|path| member_path(&path))
            .is_some_and(|path| path == Path::new(MANIFEST));
        if !is_manifest {
            continue;
        }
        let mut text = Vec::new();
        entry
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut text)
            .map_err(io("read mica-app.json"))?;
        if text.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(Problem::new(
                "manifest",
                format!("mica-app.json is above {} KiB", MAX_MANIFEST_BYTES >> 10),
            )
            .into());
        }
        return serde_json::from_slice(&text)
            .map_err(|err| Problem::new("manifest", format!("mica-app.json: {err}")).into());
    }
    Err(Problem::new("manifest", "the bundle has no mica-app.json").into())
}

/// The manifest of the bundle unpacked at `dir`.
pub fn read_manifest(dir: &Path) -> Result<Manifest, Refusal> {
    let path = dir.join(MANIFEST);
    let text = match fs::read(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(Problem::new("manifest", "the bundle has no mica-app.json").into());
        }
        Err(err) => return Err(io("read mica-app.json")(err)),
    };
    serde_json::from_slice(&text)
        .map_err(|err| Problem::new("manifest", format!("mica-app.json: {err}")).into())
}

/// `raw` as a path inside the bundle: relative, no `..`, `.` dropped.
fn member_path(raw: &Path) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Whether a directory above `relative` in `dest` is a link: a member is never
/// written through one.
fn through_link(dest: &Path, relative: &Path) -> bool {
    let mut path = dest.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return false;
        }
        path.push(part);
        if path
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return true;
        }
    }
    false
}

/// Whether a link at `member` to `link` resolves inside the bundle.
fn link_stays_inside(member: &Path, link: &Path) -> bool {
    if link.is_absolute() || link.as_os_str().is_empty() {
        return false;
    }
    let mut depth = member.components().count() - 1;
    for component in link.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use micad_settings::Init;

    /// One member of a test bundle.
    pub(crate) enum Member<'a> {
        File(&'a str, &'a [u8], u32),
        Dir(&'a str),
        Link(&'a str, &'a str),
        Fifo(&'a str),
    }

    /// A gzip tar of `members` at `path`.
    pub(crate) fn write_bundle(path: &Path, members: &[Member<'_>]) {
        let file = File::create(path).unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::fast(),
        ));
        for member in members {
            let mut header = tar::Header::new_gnu();
            match member {
                Member::File(name, data, mode) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(data.len() as u64);
                    header.set_mode(*mode);
                    header.set_cksum();
                    builder.append_data(&mut header, name, *data).unwrap();
                }
                Member::Dir(name) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_mode(0o755);
                    header.set_cksum();
                    builder.append_data(&mut header, name, io::empty()).unwrap();
                }
                Member::Link(name, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    header.set_mode(0o777);
                    builder.append_link(&mut header, name, target).unwrap();
                }
                Member::Fifo(name) => {
                    header.set_entry_type(tar::EntryType::Fifo);
                    header.set_size(0);
                    header.set_mode(0o644);
                    header.set_cksum();
                    builder.append_data(&mut header, name, io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    /// The manifest of a one-unit app `name` at `version`.
    pub(crate) fn manifest_json(name: &str, version: &str) -> String {
        serde_json::json!({
            "schemaVersion": 1,
            "name": name,
            "version": version,
            "arch": ["all"],
            "units": [{ "systemd": format!("{name}.service"), "openrc": name, "enable": true }],
            "privileges": { "user": "root" },
        })
        .to_string()
    }

    /// A well-formed bundle of app `name` at `version` at `path`.
    pub(crate) fn good_bundle(path: &Path, name: &str, version: &str) {
        let manifest = manifest_json(name, version);
        let unit = format!(
            "[Service]\nExecStart=/mica/apps/{name}/current/files/run\n\n[Install]\nWantedBy=multi-user.target\n"
        );
        let script = "#!/sbin/openrc-run\ncommand=/bin/true\n";
        write_bundle(
            path,
            &[
                Member::File("mica-app.json", manifest.as_bytes(), 0o644),
                Member::File("files/run", b"#!/bin/sh\nexec sleep 1000\n", 0o755),
                Member::Link("files/bin", "run"),
                Member::File(
                    &format!("units/systemd/{name}.service"),
                    unit.as_bytes(),
                    0o644,
                ),
                Member::File(&format!("units/openrc/{name}"), script.as_bytes(), 0o755),
            ],
        );
    }

    #[test]
    fn a_good_bundle_unpacks_with_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        good_bundle(&archive, "demo", "1.0");
        let dest = dir.path().join("out");
        let unpacked = unpack(&archive, &dest, Init::Systemd, "arm64").unwrap();
        assert_eq!(unpacked.manifest.name, "demo");
        assert_eq!(unpacked.sha256, sha256_file(&archive).unwrap());
        assert_eq!(
            fs::metadata(dest.join("files/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(dest.join("files/bin")).unwrap(),
            Path::new("run")
        );
    }

    /// `docs/apps/tailscale`, laid out as its `pack.sh` lays it out, with
    /// stand-ins for Tailscale's binaries.
    #[test]
    fn the_tailscale_example_is_a_good_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("tailscale.tar.gz");
        write_bundle(
            &archive,
            &[
                Member::File(
                    "mica-app.json",
                    include_bytes!("../../../../docs/apps/tailscale/mica-app.json"),
                    0o644,
                ),
                Member::Dir("files"),
                Member::File("files/tailscale", b"", 0o755),
                Member::File("files/tailscaled", b"", 0o755),
                Member::File(
                    "hooks/first-run",
                    include_bytes!("../../../../docs/apps/tailscale/first-run"),
                    0o755,
                ),
            ],
        );
        for init in [Init::Systemd, Init::Openrc] {
            let dest = dir.path().join(init.as_str());
            let unpacked = unpack(&archive, &dest, init, "arm64")
                .unwrap_or_else(|err| panic!("docs/apps/tailscale under {}: {err}", init.as_str()));
            assert_eq!(unpacked.manifest.name, "tailscale");
        }
    }

    #[test]
    fn every_member_outside_the_rules_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("bad.tar.gz");
        let manifest = manifest_json("demo", "1.0");
        write_bundle(
            &archive,
            &[
                Member::File("mica-app.json", manifest.as_bytes(), 0o644),
                Member::File("etc/passwd", b"x", 0o644),
                Member::File("files/su", b"x", 0o4755),
                Member::Link("files/out", "../../etc/shadow"),
                Member::Link("files/abs", "/etc/shadow"),
                Member::Fifo("files/pipe"),
                Member::Dir("files/sub"),
                Member::Link("files/up", ".."),
                Member::File("files/up/units/x", b"x", 0o644),
            ],
        );
        let Err(Refusal::Invalid(problems)) =
            unpack(&archive, &dir.path().join("out"), Init::Systemd, "arm64")
        else {
            panic!("refused as invalid")
        };
        let rules: Vec<&str> = problems.iter().map(|p| p.rule).collect();
        assert_eq!(
            rules,
            [
                "path_outside",
                "setuid",
                "link_outside",
                "link_outside",
                "special_file",
                "link_outside"
            ]
        );
    }

    #[test]
    fn links_are_followed_on_paper() {
        assert!(link_stays_inside(Path::new("files/a/b"), Path::new("../c")));
        assert!(link_stays_inside(
            Path::new("files/b"),
            Path::new("../units/x")
        ));
        assert!(!link_stays_inside(
            Path::new("files/b"),
            Path::new("../../x")
        ));
        assert!(!link_stays_inside(Path::new("files/b"), Path::new("/x")));
        assert_eq!(
            member_path(Path::new("./files/x")),
            Some(PathBuf::from("files/x"))
        );
        assert_eq!(member_path(Path::new("../x")), None);
    }
}
