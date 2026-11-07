//! Running the verified root's own programs before the root starts: its
//! `mke2fs` for a factory reset (`format`) and its `depmod` for the driver
//! extensions' modules ([`module_index`]).
//!
//! The initramfs carries neither e2fsprogs nor kmod; the root does. Nothing
//! of the root is written and nothing is changed into: its program runs
//! under its own dynamic loader, which finds the root's libraries by an
//! explicit search path. Every link on the way is resolved inside the root,
//! so an absolute link such as `/lib64/ld-linux-x86-64.so.2` names the
//! root's file, not the initramfs's.

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

/// A command that runs the first of `programs` found in the root at `root`
/// under the root's own loader and libraries, as `name` (its `argv[0]`, for
/// a multi-call program such as kmod). No environment is passed.
///
/// # Errors
///
/// The root has no loader for `arch` or none of `programs`.
pub fn root_command(root: &Path, arch: &str, programs: &[&str], name: &str) -> Result<Command> {
    let (loaders, triplet) = loader(arch)?;
    let loader = first(root, loaders)?;
    let program = first(root, programs)?;
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
    let mut command = Command::new(loader);
    command
        .env_clear()
        .arg("--library-path")
        .arg(std::env::join_paths(&libraries)?)
        .arg("--argv0")
        .arg(name)
        .arg(program);
    Ok(command)
}

/// Index the modules composed at `crate::extensions::MODULE_STAGING` for the
/// kernel release `release` with the `depmod` of the root at `root`, into
/// `crate::extensions::MODULE_INDEX_LAYER`. A module under `updates/` comes
/// before the support image's own of the same name, as depmod's search order
/// `updates built-in` has it.
///
/// # Errors
///
/// An invalid release, no `depmod` in the root, or one that fails.
pub fn module_index(root: &Path, arch: &str, release: &str) -> Result<()> {
    ensure!(
        !release.is_empty()
            && release.len() <= 64
            && release
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)),
        "invalid kernel release"
    );
    let index = Path::new(crate::extensions::MODULE_INDEX);
    for tree in ["base", "out"] {
        fs::create_dir_all(index.join(tree).join("usr/lib/modules"))?;
        // Whichever of the two the root's kmod was built to read.
        let lib = index.join(tree).join("lib");
        if lib.symlink_metadata().is_err() {
            std::os::unix::fs::symlink("usr/lib", &lib)?;
        }
    }
    fs::create_dir_all(index.join("out/usr/lib/modules").join(release))?;
    let config = index.join("depmod.conf");
    fs::write(&config, "search updates built-in\n")?;
    let output = root_command(root, arch, &["usr/sbin/depmod", "sbin/depmod"], "depmod")?
        .arg("-b")
        .arg(index.join("base"))
        .arg("-o")
        .arg(index.join("out"))
        .arg("-C")
        .arg(&config)
        .arg(release)
        .output()
        .context("run the root's depmod")?;
    ensure!(
        output.status.success(),
        "depmod: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A Debian root's layout: merged /usr, the loader behind an absolute
    /// link and a relative one, depmod a link to kmod.
    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        fs::create_dir_all(path.join("usr/lib/x86_64-linux-gnu")).unwrap();
        fs::create_dir_all(path.join("usr/sbin")).unwrap();
        fs::create_dir_all(path.join("usr/bin")).unwrap();
        fs::write(
            path.join("usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"),
            b"",
        )
        .unwrap();
        fs::write(path.join("usr/sbin/mke2fs"), b"").unwrap();
        fs::write(path.join("usr/bin/kmod"), b"").unwrap();
        symlink("../bin/kmod", path.join("usr/sbin/depmod")).unwrap();
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
    fn a_multi_call_program_runs_as_its_name() {
        let root = root();
        let path = root.path();
        let command = root_command(path, "amd64", &["usr/sbin/depmod"], "depmod").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[2..4], ["--argv0", "depmod"]);
        assert_eq!(args[4], path.join("usr/bin/kmod").display().to_string());
        assert!(root_command(path, "amd64", &["usr/sbin/none"], "none").is_err());
        assert!(root_command(path, "riscv64", &["usr/sbin/depmod"], "depmod").is_err());
    }
}
