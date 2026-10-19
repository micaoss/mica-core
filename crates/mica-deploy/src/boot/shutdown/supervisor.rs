//! The watchdog supervisor: the shutdown's time budget and its keepalives.

use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::*;

/// The watchdog a board's policy names, as `watchdog<N>` under `class`
/// (`/sys/class/watchdog` on a device).
///
/// `None` is `watchdog0`, what the device has always armed. An identity is
/// matched exactly against each watchdog's `identity`, and exactly one must
/// match: which watchdog guards the boot is the board's declaration, never the
/// kernel's probe order.
pub fn resolve_watchdog(class: &Path, identity: Option<&str>) -> Result<String> {
    let Some(identity) = identity else {
        return Ok("watchdog0".to_owned());
    };
    let mut found = Vec::new();
    for entry in fs::read_dir(class)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("watchdog")
            && read_text(class.join(name).join("identity"), 64)
                .is_ok_and(|text| text.trim_end_matches('\n') == identity)
        {
            found.push(name.to_owned());
        }
    }
    ensure!(
        found.len() == 1,
        "the declared watchdog is absent or ambiguous"
    );
    Ok(found.remove(0))
}

/// The only watchdog owner. All files are close-on-exec; children cannot feed it.
pub struct Supervisor {
    pub(super) started: Instant,
    pub(super) watchdog: Option<File>,
    pub(super) timeout: u64,
    pub(super) last_kick: u64,
    pub(super) armed: bool,
    pub(super) poisoned: bool,
    pub(super) budget: Option<Budget>,
}
impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}
impl Supervisor {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            watchdog: None,
            timeout: 0,
            last_kick: 0,
            armed: false,
            poisoned: false,
            budget: None,
        }
    }
    pub fn now_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    /// Arm `name` (`watchdog<N>`, from [`resolve_watchdog`]).
    pub fn arm(&mut self, name: &str) -> Result<()> {
        ensure!(self.watchdog.is_none(), "watchdog already owned");
        ensure!(
            name.strip_prefix("watchdog").is_some_and(|n| !n.is_empty()
                && n.len() <= 3
                && n.bytes().all(|b| b.is_ascii_digit())),
            "invalid watchdog name"
        );
        let class = Path::new("/sys/class/watchdog").join(name);
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(
                rustix::fs::OFlags::CLOEXEC.bits() as i32
                    | rustix::fs::OFlags::NONBLOCK.bits() as i32
                    | rustix::fs::OFlags::NOFOLLOW.bits() as i32,
            )
            .open(Path::new("/dev").join(name))
            .context("required watchdog unavailable")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.file_type().is_char_device(),
            "watchdog is not a character device"
        );
        let device = Device::parse(read_text(class.join("dev"), 64)?.trim())?;
        ensure!(
            Device::from_raw(metadata.rdev()) == device,
            "watchdog device identity mismatch"
        );
        self.watchdog = Some(file);
        let fd = self.watchdog.as_ref().context("watchdog missing")?;
        lifecycle_sys::watchdog_support(fd)?;
        self.timeout = lifecycle_sys::watchdog_timeout(fd)?;
        Budget::new(self.now_ms(), self.timeout)?;
        ensure!(
            read_text(class.join("nowayout"), 64)?.trim() == "1",
            "watchdog NOWAYOUT is not enforced"
        );
        lifecycle_sys::watchdog_keepalive(fd)?;
        self.armed = true;
        self.last_kick = self.now_ms();
        Ok(())
    }
    pub(super) fn kick(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }
        let now = self.now_ms();
        if self.budget.is_some_and(|b| now >= b.deadline_ms) {
            bail!("watchdog feeding deadline exhausted");
        }
        if now.saturating_sub(self.last_kick) >= 1000 {
            lifecycle_sys::watchdog_keepalive(self.watchdog.as_ref().context("watchdog missing")?)?;
            self.last_kick = now;
        }
        Ok(())
    }
    pub fn begin_shutdown(&mut self) -> Result<Budget> {
        ensure!(self.armed, "shutdown watchdog was not verified armed");
        if let Some(budget) = self.budget {
            return Ok(budget);
        }
        let actual =
            lifecycle_sys::watchdog_timeout(self.watchdog.as_ref().context("watchdog missing")?)?;
        self.timeout = actual;
        let budget = Budget::new(self.now_ms(), actual)?;
        self.budget = Some(budget);
        Ok(budget)
    }
    pub fn limit_shutdown(&mut self, limit_ms: Option<u64>) -> Result<Budget> {
        let mut budget = self.begin_shutdown()?;
        if let Some(limit) = limit_ms {
            ensure!(limit >= 3000, "inadequate systemd shutdown timeout");
            budget.deadline_ms = budget.deadline_ms.min(self.now_ms().saturating_add(limit));
            budget.cleanup_deadline_ms = budget.cleanup_deadline_ms.min(budget.deadline_ms - 2000);
            self.budget = Some(budget);
        }
        Ok(budget)
    }
    pub fn observe(&mut self, executable: &'static str) -> Result<Snapshot> {
        let deadline = if let Some(b) = self.budget {
            b.operation_deadline(self.now_ms())?
        } else {
            self.now_ms().saturating_add(5000)
        };
        SystemIo {
            supervisor: self,
            executable,
        }
        .scan(deadline)
    }
    pub fn prepare_process(&self) -> Result<()> {
        use std::os::fd::AsRawFd;
        std::env::set_current_dir("/")?;
        let console = OpenOptions::new()
            .read(true)
            .write(true)
            .open(devfs()?.join("console"))?;
        ensure!(
            console.metadata()?.file_type().is_char_device(),
            "console is not a character device"
        );
        rustix::stdio::dup2_stdin(&console)?;
        rustix::stdio::dup2_stdout(&console)?;
        rustix::stdio::dup2_stderr(&console)?;
        drop(console);
        let proc = procfs()?;
        let root_device = fs::metadata("/")?.dev();
        ensure!(
            fs::metadata(proc.join("self/exe"))?.dev() == root_device,
            "executable pins persistent storage"
        );
        for line in read_text(proc.join("self/maps"), 1024 * 1024)?.lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            ensure!(parts.len() >= 5, "invalid executable mappings");
            let (major, minor) = parts[3].split_once(':').context("invalid mapped device")?;
            let device = Device {
                major: u32::from_str_radix(major, 16)?,
                minor: u32::from_str_radix(minor, 16)?,
            };
            ensure!(
                parts[4] == "0" || device == Device::from_raw(root_device),
                "library or mapping pins persistent storage"
            );
        }
        let names = list(&proc.join("self/fd"), 1024)?;
        for path in names {
            let number = path
                .file_name()
                .and_then(|s| s.to_str())
                .context("invalid descriptor")?
                .parse::<i32>()?;
            if number <= 2
                || self
                    .watchdog
                    .as_ref()
                    .is_some_and(|fd| fd.as_raw_fd() == number)
            {
                continue;
            }
            ensure!(!path.exists(), "unexpected inherited descriptor {number}");
        }
        Ok(())
    }
    pub fn run_startup(&mut self, command: Command) -> Result<String> {
        let program = command
            .get_program()
            .to_str()
            .context("invalid startup command")?;
        ensure!(
            program == "/init"
                && command
                    .get_args()
                    .next()
                    .is_some_and(|arg| arg == "--startup-worker"),
            "unapproved startup executable"
        );
        let deadline = if let Some(budget) = self.budget {
            budget.operation_deadline(self.now_ms())?
        } else {
            self.now_ms().saturating_add(30000)
        };
        let output = self.run(command, deadline, 16384)?;
        Ok(String::from_utf8(output)?.trim().into())
    }
    pub(super) fn run(
        &mut self,
        mut command: Command,
        deadline: u64,
        limit: usize,
    ) -> Result<Vec<u8>> {
        ensure!(!self.poisoned, "unreaped or failed supervised work remains");
        ensure!(
            self.now_ms().saturating_add(1000) < deadline,
            "insufficient child deadline margin"
        );
        let mut child = command
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .context("start supervised child")?;
        let pid = rustix::process::Pid::from_raw(child.id() as i32).context("invalid child PID")?;
        let mut stdout = child.stdout.take().context("missing child stdout")?;
        let mut stderr = child.stderr.take().context("missing child stderr")?;
        let setup = rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK)
            .and_then(|()| rustix::fs::fcntl_setfl(&stderr, rustix::fs::OFlags::NONBLOCK));
        let readable = setup.is_ok();
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut failure = setup.err().map(anyhow::Error::from);
        let mut status = None;
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut term_at = None;
        let mut killed = false;
        loop {
            if let Err(error) = self.kick() {
                self.poisoned = true;
                failure.get_or_insert(error);
            }
            if readable {
                for (pipe, data, cap, eof) in [
                    (
                        &mut stdout as &mut dyn Read,
                        &mut output,
                        limit,
                        &mut stdout_eof,
                    ),
                    (
                        &mut stderr as &mut dyn Read,
                        &mut errors,
                        16384,
                        &mut stderr_eof,
                    ),
                ] {
                    if !*eof {
                        match drain(pipe, data, cap) {
                            Ok(done) => *eof = done,
                            Err(error) => {
                                failure.get_or_insert(error);
                            }
                        }
                    }
                }
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(value) => status = value,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        failure.get_or_insert(error.into());
                    }
                }
            }
            let now = self.now_ms();
            if status.is_some() && stdout_eof && stderr_eof {
                self.reap_adopted(deadline)?;
                if let Some(error) = failure {
                    return Err(error);
                }
                ensure!(
                    status.is_some_and(|s| s.success()),
                    "supervised child failed: {status:?}: {}",
                    String::from_utf8_lossy(&errors)
                );
                return Ok(output);
            }
            if failure.is_some() || now >= deadline.saturating_sub(1000) {
                failure.get_or_insert_with(|| anyhow::anyhow!("supervised operation timed out"));
                let start = *term_at.get_or_insert(now);
                if !killed {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
                }
                if now >= start.saturating_add(250) || now >= deadline.saturating_sub(500) {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                    killed = true;
                }
                if status.is_some() && (killed || !readable) {
                    self.reap_adopted(deadline)?;
                    return Err(failure.context("missing supervision failure")?);
                }
            }
            if now >= deadline {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                self.poisoned = true;
                bail!("unreaped supervised child at absolute deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    pub(super) fn reap_adopted(&mut self, deadline: u64) -> Result<()> {
        if std::process::id() != 1 {
            return Ok(());
        }
        for _ in 0..4096 {
            ensure!(self.now_ms() < deadline, "reap deadline exhausted");
            self.kick()?;
            match rustix::process::waitpid(None, rustix::process::WaitOptions::NOHANG) {
                Ok(Some(_)) => {}
                Ok(None) | Err(rustix::io::Errno::CHILD) => return Ok(()),
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => {
                    self.poisoned = true;
                    return Err(error.into());
                }
            }
        }
        self.poisoned = true;
        bail!("excessive adopted children")
    }
    pub fn failure(mut self, error: &anyhow::Error) -> ! {
        let _ = diagnostic(&format!(
            "MICA_SHUTDOWN stage=storage-not-released error={:?}",
            format!("{error:#}")
        ));
        if let Some(budget) = self.budget {
            let until = self.now_ms().saturating_add(2000).min(budget.deadline_ms);
            while self.now_ms() < until {
                if self.kick().is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = diagnostic(&format!(
            "MICA_SHUTDOWN stage=failed watchdogArmed={}",
            self.armed
        ));
        // PID 1 must stay alive without feeding. Only a verified armed watchdog
        // can provide the emergency reset; this is never graceful completion.
        loop {
            thread::park();
        }
    }
    pub fn terminal(&mut self, action: Action, _released: Released) -> Result<()> {
        ensure!(!self.poisoned, "unreaped work prevents terminal action");
        let budget = self.budget.context("missing lifecycle deadline")?;
        ensure!(
            self.now_ms() < budget.cleanup_deadline_ms,
            "terminal deadline exhausted"
        );
        diagnostic(&format!(
            "MICA_SHUTDOWN stage=action-requested action={}",
            action.as_str()
        ))?;
        let command = match action {
            Action::Reboot => rustix::system::RebootCommand::Restart,
            Action::Poweroff => rustix::system::RebootCommand::PowerOff,
            Action::Halt => rustix::system::RebootCommand::Halt,
        };
        let result = rustix::system::reboot(command);
        bail!("terminal action returned: {result:?}")
    }
}

pub(super) fn drain(pipe: &mut dyn Read, data: &mut Vec<u8>, limit: usize) -> Result<bool> {
    let mut buffer = [0; 4096];
    // Bound work per tick even if a writer continuously fills the pipe.
    for _ in 0..16 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                ensure!(data.len() + n <= limit, "excessive supervised output");
                data.extend_from_slice(&buffer[..n]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

pub(super) fn read_text(path: impl AsRef<Path>, limit: u64) -> Result<String> {
    let mut value = String::new();
    File::open(path.as_ref())
        .with_context(|| format!("open {}", path.as_ref().display()))?
        .take(limit + 1)
        .read_to_string(&mut value)?;
    ensure!(value.len() as u64 <= limit, "excessive kernel state");
    Ok(value)
}
