//! Making a `gpt` board's DATA again for a factory reset, before the root
//! starts, with the verified root's own `mke2fs`.
//!
//! The initramfs carries no e2fsprogs; the root does. Nothing of the root is
//! written and nothing is changed into: its program runs under its own
//! dynamic loader, which finds the root's libraries by an explicit search
//! path. Every link on the way is resolved inside the root, so an absolute
//! link such as `/lib64/ld-linux-x86-64.so.2` names the root's file, not the
//! initramfs's.

use crate::board::DataFormat;
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The dynamic loader and the multiarch triplet of a Debian root, per
/// architecture.
fn loader(arch: &str) -> Result<(&'static [&'static str], &'static str)> {
    Ok(match arch {
        "amd64" => (
            &[
                "lib64/ld-linux-x86-64.so.2",
                "usr/lib64/ld-linux-x86-64.so.2",
                "usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
            ],
            "x86_64-linux-gnu",
        ),
        "arm64" => (
            &[
                "lib/ld-linux-aarch64.so.1",
                "usr/lib/ld-linux-aarch64.so.1",
                "usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
            ],
            "aarch64-linux-gnu",
        ),
        "armhf" => (
            &[
                "lib/ld-linux-armhf.so.3",
                "usr/lib/ld-linux-armhf.so.3",
                "usr/lib/arm-linux-gnueabihf/ld-linux-armhf.so.3",
            ],
            "arm-linux-gnueabihf",
        ),
        _ => bail!("unsupported architecture"),
    })
}

/// `relative`, a path inside the tree at `root`, with every link on the way
/// resolved inside that tree.
pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut pending: Vec<String> = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .rev()
        .map(String::from)
        .collect();
    let mut current = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop() {
        match part.as_str() {
            "." => continue,
            ".." => {
                current.pop();
                continue;
            }
            _ => {}
        }
        let candidate = current.join(&part);
        let full = root.join(&candidate);
        if fs::symlink_metadata(&full)?.file_type().is_symlink() {
            links += 1;
            ensure!(links <= 16, "too many links below {}", root.display());
            let target = fs::read_link(&full)?;
            let target = target.to_str().context("a link that is not UTF-8")?;
            if target.starts_with('/') {
                current = PathBuf::new();
            }
            pending.extend(
                target
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .rev()
                    .map(String::from),
            );
        } else {
            current = candidate;
        }
    }
    Ok(root.join(current))
}

fn first(root: &Path, candidates: &[&str]) -> Result<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| resolve(root, candidate).ok().filter(|path| path.is_file()))
        .with_context(|| format!("no {} in the root", candidates[0]))
}

/// The command that formats `device` as the board's DATA with the `mke2fs`
/// of the root at `root`.
///
/// # Errors
///
/// The root has no loader or no `mke2fs` for `arch`, or `format` or
/// `device` is not one to pass.
pub fn mke2fs(root: &Path, arch: &str, device: &str, format: &DataFormat) -> Result<Command> {
    let (loaders, triplet) = loader(arch)?;
    let loader = first(root, loaders)?;
    let program = first(root, &["usr/sbin/mke2fs", "sbin/mke2fs"])?;
    let mut libraries = Vec::new();
    for directory in [
        format!("usr/lib/{triplet}"),
        format!("lib/{triplet}"),
        "usr/lib".to_owned(),
        "lib".to_owned(),
    ] {
        if let Ok(path) = resolve(root, &directory)
            && path.is_dir()
            && !libraries.contains(&path)
        {
            libraries.push(path);
        }
    }
    ensure!(
        !device.is_empty() && !device.starts_with('-') && !device.contains(char::is_whitespace),
        "invalid DATA device"
    );
    let mut command = Command::new(loader);
    command
        .env_clear()
        .arg("--library-path")
        .arg(std::env::join_paths(&libraries)?)
        .arg(program)
        .args(["-q", "-F", "-t", "ext4", "-b", "4096", "-L", "data", "-O"])
        .arg(&format.features);
    if let Some(bytes) = format.bytes_per_inode {
        command.arg("-i").arg(bytes.to_string());
    }
    command.args(["-E", "root_owner=0:0"]).arg(device);
    let config = root.join("etc/mke2fs.conf");
    if config.is_file() {
        command.env("MKE2FS_CONFIG", config);
    }
    Ok(command)
}

/// Format the block device `device` as the board's DATA with the root at
/// `root`, securely erasing it first when `format` asks; answer what was
/// done.
///
/// # Errors
///
/// `device` is not a block device, or the format fails. A device that cannot
/// erase securely is formatted all the same, and the answer says so.
pub fn format_data(root: &Path, arch: &str, device: &str, format: &DataFormat) -> Result<String> {
    use std::os::unix::fs::FileTypeExt;
    ensure!(
        device.starts_with("/dev/") && fs::metadata(device)?.file_type().is_block_device(),
        "DATA is not a block device"
    );
    let mut done = Vec::new();
    if format.secure_erase {
        use std::io::{Seek, SeekFrom};
        let mut file = fs::OpenOptions::new().write(true).open(device)?;
        let bytes = file.seek(SeekFrom::End(0))?;
        match lifecycle_sys::secure_discard(&file, bytes) {
            Ok(()) => done.push(format!("{bytes} bytes erased securely")),
            Err(error) => done.push(format!("secure erase unsupported: {error}")),
        }
    }
    let output = mke2fs(root, arch, device, format)?
        .output()
        .context("run the root's mke2fs")?;
    ensure!(
        output.status.success(),
        "mke2fs: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    done.push(format!("{device} formatted"));
    Ok(done.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A Debian root's layout: merged /usr, the loader behind an absolute
    /// link and a relative one.
    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        fs::create_dir_all(path.join("usr/lib/x86_64-linux-gnu")).unwrap();
        fs::create_dir_all(path.join("usr/sbin")).unwrap();
        fs::create_dir_all(path.join("etc")).unwrap();
        fs::write(
            path.join("usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"),
            b"",
        )
        .unwrap();
        fs::write(path.join("usr/sbin/mke2fs"), b"").unwrap();
        fs::write(path.join("etc/mke2fs.conf"), b"").unwrap();
        symlink("usr/lib", path.join("lib")).unwrap();
        fs::create_dir(path.join("lib64")).unwrap();
        symlink(
            "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
            path.join("lib64/ld-linux-x86-64.so.2"),
        )
        .unwrap();
        root
    }

    #[test]
    fn links_resolve_inside_the_root() {
        let root = root();
        let path = root.path();
        assert_eq!(
            resolve(path, "lib64/ld-linux-x86-64.so.2").unwrap(),
            path.join("usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2")
        );
        assert_eq!(
            resolve(path, "lib/../lib/x86_64-linux-gnu").unwrap(),
            path.join("usr/lib/x86_64-linux-gnu")
        );
        // Out of the root by `..` is the root.
        assert_eq!(
            resolve(path, "../../usr/sbin/mke2fs").unwrap(),
            path.join("usr/sbin/mke2fs")
        );
        symlink("loop", path.join("loop")).unwrap();
        assert!(resolve(path, "loop").is_err());
    }

    #[test]
    fn the_roots_mke2fs_runs_under_its_own_loader() {
        let root = root();
        let path = root.path();
        let command = mke2fs(path, "amd64", "/dev/vda3", &DataFormat::default_for(true)).unwrap();
        assert_eq!(
            Path::new(command.get_program()),
            path.join("usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2")
        );
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let libraries = format!(
            "{}:{}",
            path.join("usr/lib/x86_64-linux-gnu").display(),
            path.join("usr/lib").display()
        );
        let program = path.join("usr/sbin/mke2fs").display().to_string();
        assert_eq!(
            args,
            [
                "--library-path",
                &libraries,
                &program,
                "-q",
                "-F",
                "-t",
                "ext4",
                "-b",
                "4096",
                "-L",
                "data",
                "-O",
                "project,quota,^orphan_file,^metadata_csum_seed",
                "-i",
                "16384",
                "-E",
                "root_owner=0:0",
                "/dev/vda3",
            ]
        );
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "MKE2FS_CONFIG"
                    && value == Some(path.join("etc/mke2fs.conf").as_os_str()))
        );
        let plain = mke2fs(path, "amd64", "/dev/vda3", &DataFormat::default_for(false)).unwrap();
        assert!(!plain.get_args().any(|arg| arg == "-i"));
        for device in ["", "-n", "/dev/vda 3"] {
            assert!(mke2fs(path, "amd64", device, &DataFormat::default_for(true)).is_err());
        }
        // Another architecture's loader is not there.
        assert!(mke2fs(path, "arm64", "/dev/vda3", &DataFormat::default_for(true)).is_err());
    }

    /// The real thing on a Debian root with e2fsprogs, against a file as the
    /// device: `MICA_TEST_MKE2FS_ROOT=/` in such a container.
    #[test]
    #[ignore = "needs a Debian root with e2fsprogs at MICA_TEST_MKE2FS_ROOT"]
    fn the_roots_mke2fs_makes_the_boards_data() {
        let root = PathBuf::from(std::env::var("MICA_TEST_MKE2FS_ROOT").unwrap());
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => "armhf",
        };
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("data.img");
        fs::File::create(&image).unwrap().set_len(64 << 20).unwrap();
        let output = mke2fs(
            &root,
            arch,
            image.to_str().unwrap(),
            &DataFormat::default_for(true),
        )
        .unwrap()
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let header = Command::new(root.join("usr/sbin/dumpe2fs"))
            .arg("-h")
            .arg(&image)
            .output()
            .unwrap();
        let header = String::from_utf8_lossy(&header.stdout);
        let features = header
            .lines()
            .find_map(|line| line.strip_prefix("Filesystem features:"))
            .unwrap();
        for wanted in ["project", "quota", "extent"] {
            assert!(
                features.split_whitespace().any(|f| f == wanted),
                "{features}"
            );
        }
        for unwanted in ["orphan_file", "metadata_csum_seed"] {
            assert!(
                !features.split_whitespace().any(|f| f == unwanted),
                "{features}"
            );
        }
        assert!(
            header
                .lines()
                .any(|line| line.starts_with("Filesystem volume name:") && line.ends_with("data"))
        );
        assert!(header.lines().any(|line| line.starts_with("Inode size:")));
        println!("{header}");
    }
}
