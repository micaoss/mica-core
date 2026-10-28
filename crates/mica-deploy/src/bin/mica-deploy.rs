//! Device entry point for signed component installation and boot confirmation.
#![forbid(unsafe_code)]
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    acquisition::Acquisition,
    board::{BoardFacts, Storage},
    boot::BootKind,
    components::BootIdentity,
    core_set::{self, CoreBoot},
    defaults::{Defaults, GENERAL_CHANNEL},
    deployments::{
        BootBackend, BootReceipt, DeploymentStore, SetTarget, SharedDataFailure, Target,
        read_bounded,
    },
    set_state,
    ubi::SysUbi,
    update_set::SetKind,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "\
usage: mica-deploy <command> [<argument>...] [--<option> <value>...]

Updating by hand, with or without micad:
  update [--source <url>] [--channel <channel>] [--reboot]
                  check, download and install the newest core set, else
                  the newest system
  rollback [--reboot]
                  return to the retained previous system
  import <archive>
                  stage an offline update archive
  status          the boot, the deployments and the sets
  help            this text

The steps, as micad runs them:
  probe | discard | gc | confirm | booted | fail-boot | reject <id>
  check [--source <url>]
  fetch [--source <url>]
  set-check --kind <kind> [--source <url>] [--channel <channel>] [--deployment <descriptor>]
  set-fetch --kind <kind> [--source <url>] [--channel <channel>] [--deployment <descriptor>]
  install <descriptor> --objects <dir> [--core-set <set>]
  set-install <set> --objects <dir>
  firmware-readback [<manifest>] [--record]

Every command but update and help answers in JSON.

  --source <url>    the update root; source.url of /mica/config/updates.json,
                    else the image's update.source
  --channel <name>  the channel of a set; the one updates.json or the image
                    names for its kind, else general
  --max-bytes <n>   the download budget; source.maxBytes of updates.json,
                    else 500000000
  --reboot          reboot once done, instead of saying a reboot runs it
";

/// `mica-deploy <action> [<argument>...] [--<option> <value>...]`, each option
/// also as `--<option>=<value>`, and `--max-bytes` also before the action.
struct Cli {
    /// The workspace budget in bytes; the device's when not given.
    max_bytes: Option<u64>,
    command: Action,
}
enum Action {
    Help,
    /// Check, fetch and install the newest the channel offers.
    Update {
        source: Option<String>,
        channel: Option<String>,
        reboot: bool,
    },
    Status,
    Probe,
    Discard,
    Booted,
    FailBoot,
    FirmwareReadback {
        manifest: PathBuf,
        record: bool,
    },
    Gc,
    Confirm,
    Reject {
        id: String,
    },
    Rollback {
        reboot: bool,
    },
    Check {
        source: Option<String>,
    },
    Fetch {
        source: Option<String>,
    },
    Import {
        archive: PathBuf,
    },
    /// Read the catalog's line of a kind for a channel.
    SetCheck {
        kind: SetKind,
        source: Option<String>,
        channel: Option<String>,
        /// Select on this deployment's root rather than the running one.
        deployment: Option<PathBuf>,
    },
    /// Fetch the set the catalog offers a channel.
    SetFetch {
        kind: SetKind,
        source: Option<String>,
        channel: Option<String>,
        /// Select on this deployment's root rather than the running one.
        deployment: Option<PathBuf>,
    },
    Install {
        descriptor: PathBuf,
        objects: PathBuf,
        /// The core channel's latest set, fetched for this deployment.
        core_set: Option<PathBuf>,
    },
    /// Publish a signed set beside the current one of its kind and start its
    /// trial. The kind is the one the set was signed as.
    SetInstall {
        set: PathBuf,
        objects: PathBuf,
    },
}

/// An action's arguments: its options by name, then its positional
/// arguments in order. Each is taken once, and what is left over is refused.
struct Words {
    options: Vec<(String, Option<String>)>,
    positional: std::collections::VecDeque<String>,
}

impl Words {
    /// `flags` are the options that take no value.
    fn read(args: impl Iterator<Item = String>, flags: &[&str]) -> Result<Self> {
        let mut args = args.peekable();
        let mut words = Self {
            options: Vec::new(),
            positional: std::collections::VecDeque::new(),
        };
        while let Some(arg) = args.next() {
            let Some(option) = arg.strip_prefix("--") else {
                words.positional.push_back(arg);
                continue;
            };
            let (name, inline) = match option.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (option, None),
            };
            ensure!(
                !words.options.iter().any(|(seen, _)| seen == name),
                "--{name} is given twice"
            );
            let value = if flags.contains(&name) {
                ensure!(inline.is_none(), "--{name} takes no value");
                None
            } else {
                Some(match inline {
                    Some(value) => value,
                    None => args
                        .next()
                        .with_context(|| format!("--{name} needs a value"))?,
                })
            };
            words.options.push((name.to_owned(), value));
        }
        Ok(words)
    }

    fn option(&mut self, name: &str) -> Option<String> {
        let at = self.options.iter().position(|(seen, _)| seen == name)?;
        self.options.remove(at).1
    }

    fn required(&mut self, name: &str) -> Result<String> {
        self.option(name)
            .with_context(|| format!("--{name} is required"))
    }

    fn flag(&mut self, name: &str) -> bool {
        let at = self.options.iter().position(|(seen, _)| seen == name);
        at.map(|at| self.options.remove(at)).is_some()
    }

    fn argument(&mut self, what: &str) -> Result<String> {
        self.positional
            .pop_front()
            .with_context(|| format!("<{what}> is required"))
    }

    fn done(self) -> Result<()> {
        if let Some((name, _)) = self.options.first() {
            anyhow::bail!("unexpected option --{name}");
        }
        if let Some(arg) = self.positional.front() {
            anyhow::bail!("unexpected argument {arg:?}");
        }
        Ok(())
    }
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let mut max_bytes = None;
        let name = loop {
            let arg = args
                .next()
                .context("a command is required; mica-deploy help lists them")?;
            if arg == "-h" || arg == "--help" {
                break "help".to_owned();
            }
            let Some(option) = arg.strip_prefix("--") else {
                break arg;
            };
            let value = match option.split_once('=') {
                Some(("max-bytes", value)) => value.to_owned(),
                None if option == "max-bytes" => {
                    args.next().context("--max-bytes needs a value")?
                }
                _ => anyhow::bail!("unexpected option --{option}"),
            };
            ensure!(max_bytes.is_none(), "--max-bytes is given twice");
            max_bytes = Some(value.parse().context("invalid --max-bytes")?);
        };
        let mut words = Words::read(args, &["record", "reboot"])?;
        if let Some(value) = words.option("max-bytes") {
            ensure!(max_bytes.is_none(), "--max-bytes is given twice");
            max_bytes = Some(value.parse().context("invalid --max-bytes")?);
        }
        let kind = |words: &mut Words| -> Result<SetKind> { Ok(words.required("kind")?.parse()?) };
        let command = match name.as_str() {
            "help" => Action::Help,
            "update" => Action::Update {
                source: words.option("source"),
                channel: words.option("channel"),
                reboot: words.flag("reboot"),
            },
            "status" => Action::Status,
            "probe" => Action::Probe,
            "discard" => Action::Discard,
            "booted" => Action::Booted,
            "fail-boot" => Action::FailBoot,
            "firmware-readback" => Action::FirmwareReadback {
                record: words.flag("record"),
                manifest: words
                    .positional
                    .pop_front()
                    .unwrap_or_else(|| "/mnt/data/meta/firmware.json".to_owned())
                    .into(),
            },
            "gc" => Action::Gc,
            "confirm" => Action::Confirm,
            "reject" => Action::Reject {
                id: words.argument("id")?,
            },
            "rollback" => Action::Rollback {
                reboot: words.flag("reboot"),
            },
            "check" => Action::Check {
                source: words.option("source"),
            },
            "fetch" => Action::Fetch {
                source: words.option("source"),
            },
            "import" => Action::Import {
                archive: words.argument("archive")?.into(),
            },
            "set-check" => Action::SetCheck {
                kind: kind(&mut words)?,
                source: words.option("source"),
                channel: words.option("channel"),
                deployment: words.option("deployment").map(PathBuf::from),
            },
            "set-fetch" => Action::SetFetch {
                kind: kind(&mut words)?,
                source: words.option("source"),
                channel: words.option("channel"),
                deployment: words.option("deployment").map(PathBuf::from),
            },
            "install" => Action::Install {
                descriptor: words.argument("descriptor")?.into(),
                objects: words.required("objects")?.into(),
                core_set: words.option("core-set").map(PathBuf::from),
            },
            "set-install" => Action::SetInstall {
                set: words.argument("set")?.into(),
                objects: words.required("objects")?.into(),
            },
            _ => anyhow::bail!("unknown command {name:?}; mica-deploy help lists them"),
        };
        words.done()?;
        Ok(Self { max_bytes, command })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Policy {
    identity: BootIdentity,
    board: BoardFacts,
    public_keys: Vec<String>,
    /// SYSTEM's and DATA's GPT partition UUIDs; a `ubi` board has neither.
    #[serde(default)]
    system_part_uuid: String,
    #[serde(default)]
    data_part_uuid: String,
}

fn receipt() -> Result<BootReceipt> {
    Ok(serde_json::from_slice(&read_bounded(
        Path::new("/run/mica/boot.json"),
        4096,
    )?)?)
}
fn command(program: &str, args: &[&str]) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            let output = child.wait_with_output()?;
            ensure!(status.success(), "{program} failed: {status}");
            ensure!(output.stdout.len() <= 4096, "excessive command output");
            return Ok(String::from_utf8(output.stdout)?.trim().to_owned());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("command timed out: {program}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn remount(path: &str, mode: &str) -> Result<()> {
    command("/bin/mount", &["-o", &format!("remount,{mode}"), path])?;
    Ok(())
}
fn mount_device(text: &str, path: &str, filesystem: &str, writable: bool) -> Result<String> {
    Ok(mount_record(text, path, filesystem, writable)?.0)
}
/// The device number and source of the one mount at `path`, which must be of
/// `filesystem`'s root and, if `writable`, read-write.
fn mount_record(
    text: &str,
    path: &str,
    filesystem: &str,
    writable: bool,
) -> Result<(String, String)> {
    let found = text
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() >= 10 && fields[4] == path).then_some(fields)
        })
        .collect::<Vec<_>>();
    ensure!(found.len() == 1, "shared storage unavailable: {path}");
    let fields = &found[0];
    ensure!(
        fields[3] == "/",
        "shared storage is a subdirectory bind: {path}"
    );
    let separator = fields
        .iter()
        .position(|field| *field == "-")
        .context("invalid mount record")?;
    ensure!(
        fields.get(separator + 1) == Some(&filesystem),
        "wrong filesystem: {path}"
    );
    if writable {
        ensure!(
            fields[5].split(',').any(|option| option == "rw"),
            "shared storage is read-only: {path}"
        );
    }
    let source = fields.get(separator + 2).context("invalid mount record")?;
    Ok((fields[2].to_string(), (*source).to_string()))
}
/// A UBIFS volume has no block device: its mount is bound to the volume by
/// its source, `ubiN:<volume>`.
fn ubifs_mount(text: &str, path: &str, source: &str, writable: bool) -> Result<()> {
    ensure!(
        mount_record(text, path, "ubifs", writable)?.1 == source,
        "mounted volume differs from authenticated boot policy: {path}"
    );
    Ok(())
}
fn require_mount(path: &str, filesystem: &str, writable: bool, number: u32) -> Result<PathBuf> {
    let device = mount_device(
        &std::fs::read_to_string("/proc/self/mountinfo")?,
        path,
        filesystem,
        writable,
    )?;
    let (major, minor) = device.split_once(':').context("invalid mount device")?;
    ensure!(
        !major.is_empty()
            && !minor.is_empty()
            && major
                .bytes()
                .chain(minor.bytes())
                .all(|c| c.is_ascii_digit()),
        "invalid mount device"
    );
    let physical = std::fs::canonicalize(format!("/sys/dev/block/{device}"))?;
    ensure!(
        std::fs::read_to_string(physical.join("partition"))?.trim() == number.to_string(),
        "wrong partition: {path}"
    );
    Ok(physical)
}

/// The product the running, dm-verity authenticated root was built as.
fn product() -> Result<String> {
    let text = read_bounded(Path::new(mica_deploy::components::PRODUCT_FILE), 4096)
        .with_context(|| format!("read {}", mica_deploy::components::PRODUCT_FILE))?;
    Ok(mica_deploy::components::device_product(
        &String::from_utf8(text)?,
    )?)
}

/// The features of the running root's product file.
fn features() -> Result<Vec<String>> {
    let text = read_bounded(Path::new(mica_deploy::components::PRODUCT_FILE), 4096)
        .with_context(|| format!("read {}", mica_deploy::components::PRODUCT_FILE))?;
    Ok(core_set::device_features(&String::from_utf8(text)?)?)
}

/// What the runkit composed this boot.
fn core_boot() -> Result<CoreBoot> {
    Ok(serde_json::from_slice(&read_bounded(
        Path::new(core_set::BOOT_FILE),
        4096,
    )?)?)
}

/// The running deployment, which the runkit authenticated and left in `/run`:
/// its root is what a set is selected on.
fn running_deployment(keys: &[[u8; 32]]) -> Result<mica_deploy::components::Deployment> {
    let envelope = read_bounded(Path::new("/run/mica/deployment.json"), 24576)?;
    Ok(mica_deploy::components::authenticate_deployment(
        &envelope, keys,
    )?)
}

/// The deployment whose root a set is selected on: the one `descriptor`
/// names, or the running one.
fn set_root(
    keys: &[[u8; 32]],
    descriptor: Option<&Path>,
) -> Result<mica_deploy::components::Deployment> {
    match descriptor {
        Some(path) => Ok(mica_deploy::components::authenticate_deployment(
            &read_bounded(path, 24576)?,
            keys,
        )?),
        None => running_deployment(keys),
    }
}

fn policy() -> Result<(Policy, Vec<[u8; 32]>)> {
    let policy: Policy = serde_json::from_slice(&read_bounded(
        Path::new("/run/mica/boot-policy.json"),
        4096,
    )?)?;
    match policy.board.storage {
        Storage::Gpt => ensure!(
            !policy.system_part_uuid.is_empty(),
            "missing SYSTEM binding"
        ),
        Storage::Ubi => ensure!(
            policy.system_part_uuid.is_empty() && policy.data_part_uuid.is_empty(),
            "partition binding on UBI storage"
        ),
    }
    policy.board.validate(&policy.identity.arch)?;
    let keys = policy
        .public_keys
        .iter()
        .map(|key| {
            let bytes = STANDARD.decode(key)?;
            ensure!(STANDARD.encode(&bytes) == *key, "noncanonical metadata key");
            bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid metadata key"))
        })
        .collect::<Result<_>>()?;
    Ok((policy, keys))
}

fn main() -> Result<()> {
    let result = execute();
    if let Err(error) = &result
        && error.is::<SharedDataFailure>()
    {
        std::fs::write("/run/mica/shared-data-failure", format!("{error:#}"))?;
    }
    result
}

fn execute() -> Result<()> {
    let cli = Cli::parse(
        std::env::args_os()
            .skip(1)
            .map(|arg| {
                arg.into_string()
                    .map_err(|_| anyhow::anyhow!("an argument is not UTF-8"))
            })
            .collect::<Result<Vec<_>>>()?,
    )?;
    if matches!(cli.command, Action::Help) {
        print!("{USAGE}");
        return Ok(());
    }
    let receipt = receipt().context("authenticated boot receipt is unavailable")?;
    let (policy, keys) = policy()?;
    let backend = policy.board.boot;
    ensure!(
        receipt.backend == backend,
        "boot receipt backend differs from signed policy"
    );
    let boot = match &policy.board.ubi {
        Some(ubi) => {
            let ubi = SysUbi::attached(&ubi.mtd)?;
            let table = std::fs::read_to_string("/proc/self/mountinfo")?;
            let number = ubi.number();
            ubifs_mount(&table, "/mnt/system", &format!("ubi{number}:system"), false)?;
            ubifs_mount(&table, "/mnt/data", &format!("ubi{number}:data"), true)
                .context(SharedDataFailure)?;
            BootBackend::Ubi {
                ubi: std::sync::Arc::new(ubi),
            }
        }
        None => gpt_storage(&policy)?,
    };
    let store = DeploymentStore::new("/mnt/system".into(), boot, "/mnt/data/meta".into())
        .with_reserves(policy.board.reserves)
        .with_boot_trust(policy.board.boot_trust);
    if matches!(cli.command, Action::Booted) {
        ensure!(
            receipt.content_verified,
            "running content is not authenticated"
        );
        mica_deploy::deployments::valid_id(&receipt.deployment_id)?;
        println!("{}", receipt.deployment_id);
        return Ok(());
    }
    if matches!(cli.command, Action::Status) {
        let features = features()?;
        let running = running_deployment(&keys)?;
        let sets = store.sets_status(
            &keys,
            &SetTarget {
                arch: &policy.identity.arch,
                features: &features,
                root: running.rootfs.facts(),
            },
            core_boot()?,
        )?;
        println!(
            "{}",
            json!({"boot":receipt,"state":store.effective_state()?,"deployments":store.describe(&keys)?,"sets":sets})
        );
        return Ok(());
    }
    ensure!(
        receipt.content_verified,
        "running content is not authenticated"
    );
    let _lock = store.lock()?;
    if let Action::FirmwareReadback { manifest, record } = &cli.command {
        let envelope = read_bounded(manifest, 6500)?;
        let firmware = mica_deploy::firmware::authenticate_firmware(&envelope, &keys)?;
        mica_deploy::firmware::admit_firmware(&firmware, &policy.identity, &policy.board)?;
        mica_deploy::firmware::verify_installed(&firmware, &store.boot)?;
        if *record {
            mica_deploy::deployments::atomic_write(
                Path::new("/mnt/data/meta/firmware.json"),
                &envelope,
            )?;
        }
        println!(
            "{}",
            json!({"firmwareId": firmware.id, "generation": firmware.generation, "readbackVerified": true, "receiptRecorded": record})
        );
        return Ok(());
    }
    if let Action::Update {
        source,
        channel,
        reboot,
    } = cli.command
    {
        require_workspace(policy.board.storage)?;
        let defaults = Defaults::read()?;
        let source = source.or(defaults.source.clone()).context(NO_SOURCE)?;
        let channel =
            channel.unwrap_or_else(|| defaults.channel(SetKind::Core.as_str()).to_owned());
        let (product, features) = (product()?, features()?);
        let acquisition = Acquisition {
            root: "/mica/updates".into(),
            store: &store,
            keys: &keys,
            board: &policy.identity.board,
            arch: &policy.identity.arch,
            product: &product,
            max_bytes: Some(cli.max_bytes.unwrap_or(defaults.max_bytes)),
        };
        acquisition.probe()?;
        let running = running_deployment(&keys)?;
        let on = |root| SetTarget {
            arch: &policy.identity.arch,
            features: &features,
            root,
        };
        // The core line first, as micad checks it: the smaller and more frequent
        // update. A core line this device cannot read never blocks the system
        // update that would mend it.
        let core = match acquisition.set_check(
            SetKind::Core,
            &source,
            &channel,
            &on(running.rootfs.facts()),
        ) {
            Ok(catalog) => catalog.selected,
            Err(error) => {
                eprintln!(
                    "mica-deploy: the core line could not be checked ({error:#}); checking the system"
                );
                None
            }
        };
        if let Some(selected) = core {
            let set = acquisition.set_fetch(selected)?;
            writable(backend, true, || {
                store.install_set(
                    &read_bounded(&set.path, set_state::MAX_ENVELOPE_BYTES)?,
                    &keys,
                    &on(running.rootfs.facts()),
                    &set.objects,
                )
            })?;
            return finish(
                &format!("installed core set {} ({})", set.version, set.channel),
                reboot,
            );
        }
        let Some(selected) = acquisition.check(&source)?.selected else {
            println!("up to date");
            return Ok(());
        };
        let system = acquisition.fetch(selected)?;
        // On `general`, the system takes the channel's latest set for its new
        // root with it; without one, its image's set serves.
        let core_set = if channel == GENERAL_CHANNEL {
            (|| -> Result<Option<Vec<u8>>> {
                let root = set_root(&keys, Some(&system.path))?;
                let catalog = acquisition.set_check(
                    SetKind::Core,
                    &source,
                    &channel,
                    &SetTarget {
                        arch: &policy.identity.arch,
                        features: &features,
                        root: root.rootfs.facts(),
                    },
                )?;
                let Some(selected) = catalog.selected else {
                    return Ok(None);
                };
                let set = acquisition.set_fetch(selected)?;
                Ok(Some(read_bounded(&set.path, set_state::MAX_ENVELOPE_BYTES)?))
            })()
            .unwrap_or_else(|error| {
                eprintln!(
                    "mica-deploy: the core set for the new system was not fetched ({error:#}); its image's set serves"
                );
                None
            })
        } else {
            None
        };
        writable(backend, true, || {
            store.install(
                &read_bounded(&system.path, 24576)?,
                &keys,
                &Target {
                    board: &policy.identity.board,
                    arch: &policy.identity.arch,
                    product: &product,
                    features: &features,
                },
                &system.objects,
                &receipt,
                core_set.as_deref(),
            )
        })?;
        return finish(&format!("installed system {}", system.version), reboot);
    }
    if matches!(
        cli.command,
        Action::Probe
            | Action::Discard
            | Action::Check { .. }
            | Action::Fetch { .. }
            | Action::Import { .. }
            | Action::SetCheck { .. }
            | Action::SetFetch { .. }
    ) {
        require_workspace(policy.board.storage)?;
        let product = product()?;
        let max_bytes = match cli.command {
            Action::Discard => cli.max_bytes,
            _ => Some(or_device(cli.max_bytes, |device| Ok(device.max_bytes))?),
        };
        let acquisition = Acquisition {
            root: "/mica/updates".into(),
            store: &store,
            keys: &keys,
            board: &policy.identity.board,
            arch: &policy.identity.arch,
            product: &product,
            max_bytes,
        };
        let source = |given| or_device(given, |device| device.source.clone().context(NO_SOURCE));
        let channel = |given, kind: SetKind| {
            or_device(given, |device| Ok(device.channel(kind.as_str()).to_owned()))
        };
        let result = match cli.command {
            Action::Probe => acquisition.probe()?,
            Action::Discard => json!({"removedFiles":acquisition.discard()?}),
            Action::Check { source: given } => {
                let catalog = acquisition.check(&source(given)?)?;
                json!({"revision":catalog.checkpoint.revision,"selected":catalog.selected})
            }
            Action::Fetch { source: given } => {
                let catalog = acquisition.check(&source(given)?)?;
                match catalog.selected {
                    Some(selected) => json!(acquisition.fetch(selected)?),
                    None => serde_json::Value::Null,
                }
            }
            Action::Import { archive } => {
                ensure!(
                    archive.symlink_metadata()?.is_file(),
                    "archive is not a regular file"
                );
                json!(acquisition.import_any(&mut std::fs::File::open(archive)?)?)
            }
            Action::SetCheck {
                kind,
                source: given,
                channel: named,
                deployment,
            } => {
                let (features, root) = (features()?, set_root(&keys, deployment.as_deref())?);
                let catalog = acquisition.set_check(
                    kind,
                    &source(given)?,
                    &channel(named, kind)?,
                    &SetTarget {
                        arch: &policy.identity.arch,
                        features: &features,
                        root: root.rootfs.facts(),
                    },
                )?;
                json!({"revision":catalog.checkpoint.revision,"selected":catalog.selected,
                    "waitsForSystem":catalog.waits_for_system})
            }
            Action::SetFetch {
                kind,
                source: given,
                channel: named,
                deployment,
            } => {
                let (features, root) = (features()?, set_root(&keys, deployment.as_deref())?);
                let catalog = acquisition.set_check(
                    kind,
                    &source(given)?,
                    &channel(named, kind)?,
                    &SetTarget {
                        arch: &policy.identity.arch,
                        features: &features,
                        root: root.rootfs.facts(),
                    },
                )?;
                match catalog.selected {
                    Some(selected) => json!(acquisition.set_fetch(selected)?),
                    None => serde_json::Value::Null,
                }
            }
            _ => unreachable!(),
        };
        println!("{result}");
        return Ok(());
    }
    let booted_core = core_boot()?;
    // Confirming a boot settles the set on trial, which writes SYSTEM.
    let install = matches!(
        cli.command,
        Action::Install { .. } | Action::Gc | Action::SetInstall { .. }
    ) || (matches!(cli.command, Action::Confirm)
        && store.sets_need_settling(&receipt.deployment_id, &booted_core)?);
    let reboot = matches!(cli.command, Action::Rollback { reboot: true });
    let result = writable(backend, install, || match cli.command {
        Action::Confirm => {
            let state = store.confirm(&receipt)?;
            // After the deployment: the health gate's verdict is on the boot
            // as a whole, and a deployment that could not be confirmed
            // confirms nothing composed over it.
            // The answer is the deployment state; `status` reports the sets.
            store.settle_sets(&receipt.deployment_id, &booted_core, &keys)?;
            Ok(json!(state))
        }
        // A boot that failed with a set alone on trial is the set's failure,
        // and its trial has already paid for it: the deployment is kept.
        Action::FailBoot if booted_core.pending && !booted_core.paired => {
            Ok(json!({"retired": false, "setTrial": "core"}))
        }
        Action::FailBoot => store
            .fail_boot(&receipt)
            .map(|retired| json!({"retired": retired})),
        Action::Reject { id } => store.reject(&id).map(|value| json!(value)),
        Action::Rollback { .. } => store.rollback(&receipt).map(|value| json!(value)),
        Action::Install {
            descriptor,
            objects,
            core_set,
        } => store
            .install(
                &read_bounded(&descriptor, 24576)?,
                &keys,
                &Target {
                    board: &policy.identity.board,
                    arch: &policy.identity.arch,
                    product: &product()?,
                    features: &features()?,
                },
                &objects,
                &receipt,
                core_set
                    .map(|path| read_bounded(&path, set_state::MAX_ENVELOPE_BYTES))
                    .transpose()?
                    .as_deref(),
            )
            .map(|value| json!(value)),
        Action::Gc => store
            .collect(&receipt, &keys)
            .map(|count| json!({"removedFiles":count})),
        Action::SetInstall { set, objects } => store
            .install_set(
                &read_bounded(&set, set_state::MAX_ENVELOPE_BYTES)?,
                &keys,
                &SetTarget {
                    arch: &policy.identity.arch,
                    features: &features()?,
                    root: running_deployment(&keys)?.rootfs.facts(),
                },
                &objects,
            )
            .map(|value| json!(value)),
        Action::Help
        | Action::Update { .. }
        | Action::Status
        | Action::Probe
        | Action::Discard
        | Action::Booted
        | Action::FirmwareReadback { .. }
        | Action::Check { .. }
        | Action::Fetch { .. }
        | Action::Import { .. }
        | Action::SetCheck { .. }
        | Action::SetFetch { .. } => unreachable!(),
    })?;
    println!("{}", serde_json::to_string(&result)?);
    if reboot {
        command("reboot", &[])?;
    }
    Ok(())
}

/// Why a check has nowhere to look.
const NO_SOURCE: &str =
    "no update source: pass --source <url>, or set source.url in /mica/config/updates.json";

/// `given`, or what the device's configuration says.
fn or_device<T>(given: Option<T>, pick: impl FnOnce(&Defaults) -> Result<T>) -> Result<T> {
    match given {
        Some(value) => Ok(value),
        None => pick(&Defaults::read()?),
    }
}

/// Run `change` with SYSTEM writable when `system` says and, on a UEFI board,
/// the ESP, and both read-only again after, whatever `change` returned.
fn writable<T>(backend: BootKind, system: bool, change: impl FnOnce() -> Result<T>) -> Result<T> {
    if system {
        remount("/mnt/system", "rw")?;
    }
    if backend == BootKind::Uefi
        && let Err(error) = remount("/boot", "rw")
    {
        if system {
            remount("/mnt/system", "ro")?;
        }
        return Err(error);
    }
    let result = change();
    let esp_sync = if backend == BootKind::Uefi {
        remount("/boot", "ro")
    } else {
        Ok(())
    };
    let system_sync = if system {
        remount("/mnt/system", "ro")
    } else {
        Ok(())
    };
    esp_sync?;
    system_sync?;
    result
}

/// Say what an update installed, which runs from the next boot, and reboot
/// when asked to.
fn finish(what: &str, reboot: bool) -> Result<()> {
    if !reboot {
        println!("{what}; reboot to run it (or run again with --reboot)");
        return Ok(());
    }
    println!("{what}; rebooting");
    command("reboot", &[])?;
    Ok(())
}

/// The boot medium of a `gpt` board, with SYSTEM and DATA mounted from the
/// partitions its policy binds.
fn gpt_storage(policy: &Policy) -> Result<BootBackend> {
    let partitions = policy.board.gpt_partitions()?;
    let system = require_mount("/mnt/system", "ext4", false, partitions.system)?;
    let boot = match policy.board.boot {
        BootKind::Uefi => {
            let esp = require_mount("/boot", "vfat", false, partitions.boot)?;
            ensure!(
                system.parent() == esp.parent(),
                "ESP and SYSTEM are on different disks"
            );
            BootBackend::Uefi {
                esp: "/boot".into(),
            }
        }
        BootKind::UbootFit => BootBackend::Fit {
            layout: policy
                .board
                .records
                .context("FIT boot records absent from the boot policy")?,
            firmware: mica_deploy::deployments::boot_partition(&system, &policy.board)?,
        },
    };
    let data_check = (|| -> Result<()> {
        let data = require_mount("/mnt/data", "ext4", true, partitions.data)?;
        ensure!(
            system.parent() == data.parent(),
            "DATA and SYSTEM are on different disks"
        );
        let node = data
            .file_name()
            .context("missing DATA device")?
            .to_str()
            .context("invalid DATA device")?;
        ensure!(
            command(
                "/sbin/blkid",
                &["-s", "PARTUUID", "-o", "value", &format!("/dev/{node}")]
            )? == policy.data_part_uuid.to_ascii_lowercase(),
            "DATA differs from authenticated boot policy"
        );
        Ok(())
    })();
    data_check.context(SharedDataFailure)?;
    let node = system
        .file_name()
        .context("missing SYSTEM device")?
        .to_str()
        .context("invalid SYSTEM device")?;
    ensure!(
        command(
            "/sbin/blkid",
            &["-s", "PARTUUID", "-o", "value", &format!("/dev/{node}")]
        )? == policy.system_part_uuid.to_ascii_lowercase(),
        "mounted SYSTEM differs from authenticated boot policy"
    );
    Ok(boot)
}

fn require_workspace(storage: Storage) -> Result<()> {
    let table = std::fs::read_to_string("/proc/self/mountinfo")?;
    let filesystem = match storage {
        Storage::Gpt => "ext4",
        Storage::Ubi => "ubifs",
    };
    let data = mount_device(&table, "/mnt/data", filesystem, true)?;
    let mut namespace = None;
    for line in table.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        if fields[4] == "/mica" {
            ensure!(
                namespace.is_none()
                    && fields[2] == data
                    && fields[3] == "/mica"
                    && fields[5].split(',').any(|option| option == "rw"),
                "invalid DATA namespace binding"
            );
            namespace = Some(());
        }
        ensure!(
            fields[4] != "/mica/updates" && !fields[4].starts_with("/mica/updates/"),
            "unexpected mount in update workspace"
        );
    }
    ensure!(namespace.is_some(), "DATA namespace is not mounted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Action, Cli, SetKind, mount_device, ubifs_mount};
    use std::path::Path;

    fn parse(line: &str) -> anyhow::Result<Cli> {
        Cli::parse(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn the_command_lines_its_callers_run_parse() {
        assert!(matches!(parse("status").unwrap().command, Action::Status));
        assert!(matches!(
            parse("fail-boot").unwrap().command,
            Action::FailBoot
        ));
        let cli = parse("--max-bytes 536870912 import /run/mica/import/update.micaupd").unwrap();
        assert_eq!(cli.max_bytes, Some(536_870_912));
        assert!(
            matches!(cli.command, Action::Import { archive } if archive == Path::new("/run/mica/import/update.micaupd"))
        );
        assert_eq!(parse("--max-bytes=7 probe").unwrap().max_bytes, Some(7));
        assert!(matches!(
            parse("reject abc").unwrap().command,
            Action::Reject { id } if id == "abc"
        ));
        assert!(matches!(
            parse("firmware-readback").unwrap().command,
            Action::FirmwareReadback { manifest, record: false }
                if manifest == Path::new("/mnt/data/meta/firmware.json")
        ));
        assert!(matches!(
            parse("firmware-readback /run/mica/firmware-receipt.json --record").unwrap().command,
            Action::FirmwareReadback { manifest, record: true }
                if manifest == Path::new("/run/mica/firmware-receipt.json")
        ));
        assert!(matches!(
            parse("--max-bytes 1 fetch --source=https://res.micaos.dev/update/").unwrap().command,
            Action::Fetch { source: Some(source) } if source == "https://res.micaos.dev/update/"
        ));
        assert!(matches!(
            parse("--max-bytes 1 set-fetch --kind core --source s --channel general --deployment d.json")
                .unwrap()
                .command,
            Action::SetFetch { kind: SetKind::Core, source: Some(source), channel: Some(channel), deployment: Some(d) }
                if source == "s" && channel == "general" && d == Path::new("d.json")
        ));
        assert!(matches!(
            parse("--max-bytes 1 set-check --channel general --kind core --source s")
                .unwrap()
                .command,
            Action::SetCheck {
                deployment: None,
                ..
            }
        ));
        assert!(matches!(
            parse("install /v/id.json --objects /v/objects --core-set /v/set.json").unwrap().command,
            Action::Install { descriptor, objects, core_set: Some(set) }
                if descriptor == Path::new("/v/id.json")
                    && objects == Path::new("/v/objects")
                    && set == Path::new("/v/set.json")
        ));
        assert!(matches!(
            parse("set-install /v/set.json --objects /v/o").unwrap().command,
            Action::SetInstall { set, objects }
                if set == Path::new("/v/set.json") && objects == Path::new("/v/o")
        ));
    }

    #[test]
    fn what_an_operator_types_parses() {
        for line in ["help", "-h", "--help"] {
            assert!(
                matches!(parse(line).unwrap().command, Action::Help),
                "{line}"
            );
        }
        assert!(matches!(
            parse("update").unwrap().command,
            Action::Update {
                source: None,
                channel: None,
                reboot: false
            }
        ));
        let cli = parse("update --reboot --source https://u/ --channel lts --max-bytes 9").unwrap();
        assert_eq!(cli.max_bytes, Some(9));
        assert!(matches!(
            cli.command,
            Action::Update { source: Some(s), channel: Some(c), reboot: true } if s == "https://u/" && c == "lts"
        ));
        assert!(matches!(
            parse("rollback").unwrap().command,
            Action::Rollback { reboot: false }
        ));
        assert!(matches!(
            parse("rollback --reboot").unwrap().command,
            Action::Rollback { reboot: true }
        ));
        // Source, channel and budget come from the device when not given.
        assert!(matches!(
            parse("check").unwrap().command,
            Action::Check { source: None }
        ));
        assert!(matches!(
            parse("set-check --kind core").unwrap().command,
            Action::SetCheck {
                source: None,
                channel: None,
                ..
            }
        ));
        assert_eq!(parse("probe").unwrap().max_bytes, None);
        assert_eq!(parse("probe --max-bytes=3").unwrap().max_bytes, Some(3));
    }

    #[test]
    fn a_malformed_command_line_is_refused() {
        for line in [
            "",
            "frobnicate",
            "status extra",
            "--max-bytes",
            "--max-bytes x probe",
            "--max-bytes 1 --max-bytes 2 probe",
            "--other 1 probe",
            "reject",
            "check --source",
            "check --source a --source b",
            "check --source a --channel b",
            "set-check --kind ui --source s --channel c",
            "set-check",
            "install /v/id.json",
            "firmware-readback --record=yes",
            "--max-bytes 1 probe --max-bytes 1",
            "update extra",
            "update --reboot=yes",
            "update --kind core",
            "rollback --source s",
            "status --reboot",
            "help me",
        ] {
            assert!(parse(line).is_err(), "{line:?} parsed");
        }
    }

    #[test]
    fn storage_requires_one_complete_physical_mount() {
        let mount = "35 22 253:2 / /mnt/system ro,noatime - ext4 /dev/vda2 ro\n";
        assert_eq!(
            mount_device(mount, "/mnt/system", "ext4", false).unwrap(),
            "253:2"
        );
        assert!(mount_device(mount, "/mnt/system", "ext4", true).is_err());
        assert!(mount_device(mount, "/mnt/system", "vfat", false).is_err());
        assert!(mount_device(&mount.repeat(2), "/mnt/system", "ext4", false).is_err());
        assert!(
            mount_device(
                &mount.replace(" / /mnt", " /roots /mnt"),
                "/mnt/system",
                "ext4",
                false
            )
            .is_err()
        );
    }

    #[test]
    fn a_ubifs_mount_is_bound_by_its_volume() {
        let mount = "36 22 0:21 / /mnt/data rw,noatime - ubifs ubi0:data rw\n";
        ubifs_mount(mount, "/mnt/data", "ubi0:data", true).unwrap();
        assert!(ubifs_mount(mount, "/mnt/data", "ubi1:data", true).is_err());
        assert!(ubifs_mount(mount, "/mnt/data", "ubi0:system", false).is_err());
        let ext4 = mount.replace("ubifs", "ext4");
        assert!(ubifs_mount(&ext4, "/mnt/data", "ubi0:data", true).is_err());
        let read_only = mount.replace(" rw,noatime ", " ro,noatime ");
        assert!(ubifs_mount(&read_only, "/mnt/data", "ubi0:data", true).is_err());
        ubifs_mount(&read_only, "/mnt/data", "ubi0:data", false).unwrap();
    }
}
