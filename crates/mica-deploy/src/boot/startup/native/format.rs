//! Making a `gpt` board's DATA again for a factory reset, before the root
//! starts, with the verified root's own `mke2fs` (`root_program`).

use super::root_program::root_command;
use crate::board::DataFormat;
use anyhow::{Context, Result, ensure};
use std::{fs, path::Path, process::Command};

/// The command that formats `device` as the board's DATA with the `mke2fs`
/// of the root at `root`.
///
/// # Errors
///
/// The root has no loader or no `mke2fs` for `arch`, or `format` or
/// `device` is not one to pass.
pub fn mke2fs(root: &Path, arch: &str, device: &str, format: &DataFormat) -> Result<Command> {
    ensure!(
        !device.is_empty() && !device.starts_with('-') && !device.contains(char::is_whitespace),
        "invalid DATA device"
    );
    let mut command = root_command(root, arch, &["usr/sbin/mke2fs", "sbin/mke2fs"], "mke2fs")?;
    command
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
                "--argv0",
                "mke2fs",
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
        let root = std::path::PathBuf::from(std::env::var("MICA_TEST_MKE2FS_ROOT").unwrap());
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
