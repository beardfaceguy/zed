use anyhow::{Context as _, Result};
use std::process::Stdio;

#[cfg(unix)]
fn trusted_executable(name: &str) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt as _;

    let candidates = [
        std::path::PathBuf::from("/bin").join(name),
        std::path::PathBuf::from("/usr/bin").join(name),
        std::path::PathBuf::from("/run/current-system/sw/bin").join(name),
    ];
    let path = candidates
        .into_iter()
        .find(|path| path.is_file())
        .with_context(|| format!("failed to locate {name}"))?;
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    let metadata = path
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        anyhow::bail!("refusing untrusted executable {}", path.display());
    }
    Ok(path)
}

/// A wrapper around `smol::process::Child` that terminates spawned process
/// trees with their owner: on Linux by using cgroup v2, on other Unix systems
/// by killing the child's process session, and on Windows by using job objects.
///
/// A watchdog holds the read end of a lifeline whose write end is owned by
/// this struct. Closing the write end, including when the OS closes Zed's file
/// descriptors on exit, makes the watchdog kill the dedicated Linux cgroup or
/// Unix process session. On Windows, dropping this struct closes the job
/// object handle and terminates all processes in the job.
/// These OS resources keep descendants covered by those platform mechanisms
/// from outliving Zed even when Rust destructors do not run.
///
/// On non-Linux Unix systems, descendants that deliberately create a new
/// process session are outside the fallback watchdog's scope.
pub struct Child {
    process: smol::process::Child,
    #[cfg(unix)]
    process_tree: unix_process_tree::ProcessTree,
    #[cfg(windows)]
    job: Option<windows_job::JobObject>,
}

impl std::ops::Deref for Child {
    type Target = smol::process::Child;

    fn deref(&self) -> &Self::Target {
        &self.process
    }
}

impl std::ops::DerefMut for Child {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.process
    }
}

impl Child {
    #[cfg(not(windows))]
    pub fn spawn(
        mut command: std::process::Command,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Result<Self> {
        let process_tree = unix_process_tree::ProcessTree::spawn()?;
        process_tree.configure_command(&mut command)?;
        let mut command = smol::process::Command::from(command);
        let process = command
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn command {}",
                    crate::redact::redact_command(&format!("{command:?}"))
                )
            })?;
        Ok(Self {
            process,
            process_tree,
        })
    }

    #[cfg(windows)]
    pub fn spawn(
        command: std::process::Command,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Result<Self> {
        let mut command = smol::process::Command::from(command);
        let process = command
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn command {}",
                    crate::redact::redact_command(&format!("{command:?}"))
                )
            })?;

        // Assign the child to a job object configured to kill the entire
        // process tree when the last job handle is closed, so descendants
        // (e.g. node workers and MCP servers spawned by agent servers) are
        // reaped even if the direct child doesn't clean them up. Any process
        // the child spawns after this assignment is automatically part of the
        // job.
        //
        // There is a small race: descendants the child spawns between the
        // `spawn()` call returning and the assignment below escape the job.
        // Closing it fully would require creating the process suspended
        // (`CREATE_SUSPENDED`), assigning it, then resuming it, which the
        // std/smol process APIs don't support without reimplementing process
        // creation. The window is microseconds, and the children we care
        // about (`npx`, `node`, etc.) take far longer to load their runtime
        // and spawn anything, so in practice nothing escapes.
        let job = windows_job::JobObject::new()
            .and_then(|job| {
                job.assign_process(process.id())?;
                Ok(job)
            })
            .map_err(|error| {
                log::error!("failed to assign spawned process to a job object: {error:#}");
            })
            .ok();

        Ok(Self { process, job })
    }

    /// Consumes the child, draining its stdout/stderr and waiting for it to
    /// exit, then returns the collected output.
    pub async fn output(self) -> Result<std::process::Output> {
        // NOTE: Keep `self` alive across this await, do not destructure it to
        // pull `process` out first. On Windows that drops the job object early,
        // which triggers `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and kills the
        // child before `output()` finishes collecting its stdout/stderr.
        Ok(self.process.output().await?)
    }

    #[cfg(not(windows))]
    pub fn kill(&mut self) -> Result<()> {
        self.process_tree.kill()
    }

    #[cfg(windows)]
    pub fn kill(&mut self) -> Result<()> {
        if let Some(job) = &self.job {
            job.terminate()
        } else {
            self.process.kill()?;
            Ok(())
        }
    }
}

#[cfg(unix)]
mod unix_process_tree {
    use anyhow::Result;

    pub(super) enum ProcessTree {
        #[cfg(target_os = "linux")]
        Cgroup(super::linux_cgroup::Cgroup),
        #[cfg(not(target_os = "linux"))]
        Session(super::unix_process_group::ProcessGroup),
    }

    impl ProcessTree {
        pub(super) fn spawn() -> Result<Self> {
            #[cfg(target_os = "linux")]
            {
                return Ok(Self::Cgroup(super::linux_cgroup::Cgroup::spawn()?));
            }

            #[cfg(not(target_os = "linux"))]
            Ok(Self::Session(
                super::unix_process_group::ProcessGroup::spawn()?,
            ))
        }

        pub(super) fn configure_command(&self, command: &mut std::process::Command) -> Result<()> {
            match self {
                #[cfg(target_os = "linux")]
                Self::Cgroup(cgroup) => cgroup.configure_command(command),
                #[cfg(not(target_os = "linux"))]
                Self::Session(process_group) => process_group.configure_command(command),
            }
        }

        pub(super) fn kill(&mut self) -> Result<()> {
            match self {
                #[cfg(target_os = "linux")]
                Self::Cgroup(cgroup) => cgroup.kill(),
                #[cfg(not(target_os = "linux"))]
                Self::Session(process_group) => process_group.kill(),
            }
        }

        #[cfg(all(test, target_os = "linux"))]
        pub(super) fn cgroup_path(&self) -> Option<&std::path::Path> {
            match self {
                Self::Cgroup(cgroup) => Some(cgroup.path()),
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_cgroup {
    use anyhow::{Context as _, Result};
    use std::{
        io,
        os::{
            fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
            unix::process::CommandExt as _,
        },
        path::{Component, Path, PathBuf},
        process::Stdio,
        sync::atomic::{AtomicU64, Ordering},
    };

    const WATCHDOG_SCRIPT: &str = r#"
while IFS= read -r line; do :; done
while [ -e "$1" ]; do
    if [ -e "$1/cgroup.freeze" ]; then
        printf 0 > "$1/cgroup.freeze" 2>/dev/null || :
    fi
    if ! printf 1 > "$1/cgroup.kill"; then
        "$4" 0.05
        continue
    fi
    "$3" "$1" -depth -mindepth 1 -type d -exec "$2" '{}' ';' 2>/dev/null || :
    "$2" "$1" 2>/dev/null && break
    "$4" 0.05
done
"#;

    static NEXT_CGROUP_ID: AtomicU64 = AtomicU64::new(1);
    static CGROUP_INSTANCE_ID: std::sync::LazyLock<u128> = std::sync::LazyLock::new(|| {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let address = std::ptr::addr_of!(NEXT_CGROUP_ID) as usize as u128;
        timestamp ^ address
    });

    pub(super) struct Cgroup {
        writer: Option<std::os::unix::net::UnixStream>,
        cgroup_procs: OwnedFd,
        _watchdog: smol::process::Child,
        _scope_holder: Option<smol::process::Child>,
        path: PathBuf,
    }

    struct ScopeHolderGuard {
        holder: Option<smol::process::Child>,
        cgroup_path: PathBuf,
    }

    impl Drop for ScopeHolderGuard {
        fn drop(&mut self) {
            let Some(holder) = self.holder.as_mut() else {
                return;
            };
            if self.cgroup_path.exists()
                && let Err(error) = std::fs::write(self.cgroup_path.join("cgroup.kill"), "1")
            {
                log::error!("failed to kill systemd scope after setup error: {error:#}");
            }
            if unsafe { libc::killpg(holder.id() as i32, libc::SIGKILL) } == -1
                && let Err(error) = holder.kill()
            {
                log::error!("failed to stop systemd scope holder after setup error: {error:#}");
            }
        }
    }

    impl Cgroup {
        pub(super) fn spawn() -> Result<Self> {
            let id = NEXT_CGROUP_ID.fetch_add(1, Ordering::Relaxed);
            match Self::spawn_direct(id) {
                Ok(cgroup) => Ok(cgroup),
                Err(direct_error) => Self::spawn_systemd(id).with_context(|| {
                    format!(
                        "direct cgroup setup failed ({direct_error:#}); \
                         systemd transient scope setup also failed"
                    )
                }),
            }
        }

        fn spawn_direct(id: u64) -> Result<Self> {
            let parent = current_cgroup_dir()?;
            if !parent.join("cgroup.kill").is_file() {
                anyhow::bail!("current cgroup does not support cgroup.kill");
            }

            let path = parent.join(cgroup_name(id));
            std::fs::create_dir(&path)
                .with_context(|| format!("failed to create cgroup {}", path.display()))?;

            match Self::spawn_direct_in(path.clone()) {
                Ok(cgroup) => Ok(cgroup),
                Err(error) => {
                    if let Err(remove_error) = std::fs::remove_dir(&path) {
                        log::error!(
                            "failed to remove cgroup {} after setup error: {remove_error:#}",
                            path.display()
                        );
                    }
                    Err(error)
                }
            }
        }

        fn spawn_direct_in(path: PathBuf) -> Result<Self> {
            let cgroup_type = std::fs::read_to_string(path.join("cgroup.type"))
                .context("failed to read cgroup.type")?;
            if cgroup_type.trim() != "domain" {
                anyhow::bail!("unsupported cgroup type: {}", cgroup_type.trim());
            }

            let cgroup_procs = std::fs::OpenOptions::new()
                .write(true)
                .open(path.join("cgroup.procs"))
                .context("failed to open cgroup.procs")?;
            // CLOEXEC keeps the descriptor available to pre_exec, then closes
            // it atomically when the target executable starts.
            let cgroup_procs = duplicate_fd(cgroup_procs.as_raw_fd())
                .context("failed to duplicate cgroup.procs")?;

            let (reader, writer) = std::os::unix::net::UnixStream::pair()
                .context("failed to create cgroup lifeline")?;
            let shell = super::trusted_executable("sh")?;
            let rmdir = super::trusted_executable("rmdir")?;
            let find = super::trusted_executable("find")?;
            let sleep = super::trusted_executable("sleep")?;
            let mut command = std::process::Command::new(shell);
            super::unix_process_group::set_pre_exec_process_group(&mut command, 0);
            command
                .args(["-c", WATCHDOG_SCRIPT, "zed-cgroup-watchdog"])
                .arg(path.as_os_str())
                .arg(rmdir)
                .arg(find)
                .arg(sleep);
            let mut command = smol::process::Command::from(command);
            command
                .stdin(Stdio::from(OwnedFd::from(reader)))
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let watchdog = command.spawn().context("failed to spawn cgroup watchdog")?;

            Ok(Self {
                writer: Some(writer),
                cgroup_procs,
                _watchdog: watchdog,
                _scope_holder: None,
                path,
            })
        }

        fn spawn_systemd(id: u64) -> Result<Self> {
            let current_cgroup = current_cgroup_dir()?;
            let app_slice = current_cgroup
                .ancestors()
                .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "app.slice"))
                .context("current cgroup is not within app.slice")?;
            let parent_unit = current_cgroup
                .file_name()
                .context("current cgroup has no systemd unit name")?
                .to_string_lossy();
            if !parent_unit.ends_with(".service") && !parent_unit.ends_with(".scope") {
                anyhow::bail!("current cgroup is not a systemd unit");
            }

            let unit = cgroup_name(id);
            let path = app_slice.join(format!("{unit}.scope"));
            let systemd_run = super::trusted_executable("systemd-run")?;
            let shell = super::trusted_executable("sh")?;
            let sleep = super::trusted_executable("sleep")?;
            let holder_script = r#"while :; do "$1" 3600; done"#;
            let mut holder_command = std::process::Command::new(systemd_run);
            super::unix_process_group::set_pre_exec_process_group(&mut holder_command, 0);
            holder_command
                .args([
                    "--user",
                    "--scope",
                    "--quiet",
                    "--collect",
                    "--slice=app.slice",
                ])
                .arg(format!("--unit={unit}"))
                .arg(format!("--property=PartOf={parent_unit}"))
                .arg("--")
                .arg(&shell)
                .args(["-c", holder_script, "zed-systemd-scope-holder"])
                .arg(&sleep);
            let mut holder_command = smol::process::Command::from(holder_command);
            let scope_holder = holder_command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("failed to spawn systemd scope holder")?;
            let scope_holder_pid = scope_holder.id();
            let scope_holder_pid = scope_holder_pid.to_string();
            let mut scope_holder = ScopeHolderGuard {
                holder: Some(scope_holder),
                cgroup_path: path.clone(),
            };

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let holder_is_member = std::fs::read_to_string(path.join("cgroup.procs"))
                    .ok()
                    .is_some_and(|members| {
                        members.lines().any(|member| member == scope_holder_pid)
                    });
                if holder_is_member {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!("timed out waiting for transient scope holder migration");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            let cgroup_type = std::fs::read_to_string(path.join("cgroup.type"))
                .context("failed to read transient scope cgroup.type")?;
            if cgroup_type.trim() != "domain" {
                anyhow::bail!(
                    "unsupported transient scope cgroup type: {}",
                    cgroup_type.trim()
                );
            }
            let cgroup_procs = std::fs::OpenOptions::new()
                .write(true)
                .open(path.join("cgroup.procs"))
                .context("failed to open transient scope cgroup.procs")?;
            let cgroup_procs = duplicate_fd(cgroup_procs.as_raw_fd())
                .context("failed to duplicate transient scope cgroup.procs")?;

            let (reader, writer) = std::os::unix::net::UnixStream::pair()
                .context("failed to create transient scope lifeline")?;
            let rmdir = super::trusted_executable("rmdir")?;
            let find = super::trusted_executable("find")?;
            let mut watchdog_command = std::process::Command::new(shell);
            super::unix_process_group::set_pre_exec_process_group(&mut watchdog_command, 0);
            watchdog_command
                .args(["-c", WATCHDOG_SCRIPT, "zed-systemd-cgroup-watchdog"])
                .arg(path.as_os_str())
                .arg(rmdir)
                .arg(find)
                .arg(sleep);
            let mut watchdog_command = smol::process::Command::from(watchdog_command);
            let watchdog = watchdog_command
                .stdin(Stdio::from(OwnedFd::from(reader)))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("failed to spawn transient scope watchdog")?;

            Ok(Self {
                writer: Some(writer),
                cgroup_procs,
                _watchdog: watchdog,
                _scope_holder: scope_holder.holder.take(),
                path,
            })
        }

        #[cfg(test)]
        pub(super) fn spawn_systemd_for_test() -> Result<Self> {
            let id = NEXT_CGROUP_ID.fetch_add(1, Ordering::Relaxed);
            Self::spawn_systemd(id)
        }

        pub(super) fn configure_command(&self, command: &mut std::process::Command) -> Result<()> {
            let cgroup_procs_fd = self.cgroup_procs.as_raw_fd();
            // SAFETY: setsid and write are async-signal-safe.
            unsafe {
                command.pre_exec(move || {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    write_cgroup_procs(cgroup_procs_fd)
                });
            }
            Ok(())
        }

        pub(super) fn kill(&mut self) -> Result<()> {
            let result = self.kill_now();
            self.writer.take();
            result
        }

        fn kill_now(&self) -> Result<()> {
            if !self.path.exists() {
                return Ok(());
            }
            let freeze = self.path.join("cgroup.freeze");
            if freeze.exists() {
                match std::fs::write(&freeze, "0") {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error).context("failed to thaw process cgroup"),
                }
            }
            match std::fs::write(self.path.join("cgroup.kill"), "1") {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).context("failed to kill process cgroup"),
            }
        }

        #[cfg(test)]
        pub(super) fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Cgroup {
        fn drop(&mut self) {
            if self.writer.is_some()
                && let Err(error) = self.kill_now()
            {
                log::error!("failed to kill process cgroup during drop: {error:#}");
            }
        }
    }

    fn current_cgroup_dir() -> Result<PathBuf> {
        let cgroups =
            std::fs::read_to_string("/proc/self/cgroup").context("failed to read self cgroup")?;
        let relative = cgroups
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .context("process is not in a unified cgroup v2 hierarchy")?;
        let relative = Path::new(relative);
        if relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            anyhow::bail!("self cgroup path contains parent traversal");
        }
        Ok(Path::new("/sys/fs/cgroup").join(
            relative
                .strip_prefix("/")
                .context("self cgroup path is not absolute")?,
        ))
    }

    fn cgroup_name(id: u64) -> String {
        format!(
            "zed-process-{}-{:x}-{id}",
            std::process::id(),
            *CGROUP_INSTANCE_ID
        )
    }

    fn duplicate_fd(fd: libc::c_int) -> io::Result<OwnedFd> {
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
        }
    }

    fn write_cgroup_procs(fd: libc::c_int) -> io::Result<()> {
        let bytes = b"0\n";
        loop {
            let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if written == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if written as usize != bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "short write to cgroup.procs",
                ));
            }
            return Ok(());
        }
    }
}

#[cfg(unix)]
#[cfg_attr(all(target_os = "linux", not(test)), allow(dead_code))]
mod unix_process_group {
    use anyhow::{Context as _, Result};
    use std::{
        io,
        os::{
            fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
            unix::process::CommandExt as _,
        },
        process::Stdio,
    };

    const WATCHDOG_SCRIPT: &str = r#"
IFS= read -r root_pid || {
    kill -KILL 0 2>/dev/null || :
    exit
}
ps_path=$1
awk_path=$2
sleep_path=$3

cleanup_session() {
    trap - USR1
    kill "$monitor_pid" 2>/dev/null || :
    while :; do
        pids=$("$ps_path" -axo pid=,sess=,stat= | "$awk_path" -v session="$root_pid" \
            '$2 == session && $3 !~ /^Z/ { print $1 }')
        [ -n "$pids" ] || break
        kill -STOP $pids 2>/dev/null || :
        kill -KILL $pids 2>/dev/null || :
        "$sleep_path" 0.05
    done
    kill -KILL 0 2>/dev/null || :
}

watchdog_pid=$$
(
    while kill -0 "$root_pid" 2>/dev/null; do
        "$sleep_path" 0.1
    done
    kill -USR1 "$watchdog_pid" 2>/dev/null || :
) &
monitor_pid=$!
trap cleanup_session USR1

while IFS= read -r line; do :; done
cleanup_session
"#;

    pub(super) struct ProcessGroup {
        #[cfg(test)]
        pub(super) id: u32,
        writer: Option<std::os::unix::net::UnixStream>,
        _watchdog: smol::process::Child,
    }

    impl ProcessGroup {
        pub(super) fn spawn() -> Result<Self> {
            let shell = super::trusted_executable("sh")?;
            Self::spawn_with_program(shell.as_os_str())
        }

        pub(super) fn spawn_with_program(program: &std::ffi::OsStr) -> Result<Self> {
            let (reader, writer) = std::os::unix::net::UnixStream::pair()
                .context("failed to create process lifeline")?;
            let writer_fd = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
            if writer_fd == -1 {
                return Err(io::Error::last_os_error())
                    .context("failed to duplicate process lifeline");
            }
            drop(writer);
            let writer = unsafe { std::os::unix::net::UnixStream::from_raw_fd(writer_fd) };
            let ps = super::trusted_executable("ps")?;
            let awk = super::trusted_executable("awk")?;
            let sleep = super::trusted_executable("sleep")?;
            let mut command = std::process::Command::new(program);
            set_pre_exec_process_group(&mut command, 0);
            command
                .args(["-c", WATCHDOG_SCRIPT, "zed-process-watchdog"])
                .arg(ps)
                .arg(awk)
                .arg(sleep);
            let mut command = smol::process::Command::from(command);
            command
                .stdin(Stdio::from(OwnedFd::from(reader)))
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let watchdog = command
                .spawn()
                .context("failed to spawn process watchdog")?;
            Ok(Self {
                #[cfg(test)]
                id: watchdog.id(),
                writer: Some(writer),
                _watchdog: watchdog,
            })
        }

        pub(super) fn configure_command(&self, command: &mut std::process::Command) -> Result<()> {
            let writer_fd = self
                .writer
                .as_ref()
                .context("process lifeline closed before target spawn")?
                .as_raw_fd();
            // SAFETY: setsid, getpid, and write are async-signal-safe.
            unsafe {
                command.pre_exec(move || {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    write_pid(writer_fd)
                });
            }
            Ok(())
        }

        pub(super) fn kill(&mut self) -> Result<()> {
            self.writer.take();
            Ok(())
        }
    }

    fn write_pid(writer_fd: libc::c_int) -> io::Result<()> {
        let mut bytes = [0_u8; 32];
        let mut start = bytes.len() - 1;
        bytes[start] = b'\n';
        let mut pid = unsafe { libc::getpid() } as u32;
        loop {
            start -= 1;
            bytes[start] = b'0' + (pid % 10) as u8;
            pid /= 10;
            if pid == 0 {
                break;
            }
        }

        let mut remaining = &bytes[start..];
        while !remaining.is_empty() {
            let written =
                unsafe { libc::write(writer_fd, remaining.as_ptr().cast(), remaining.len()) };
            if written == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            remaining = &remaining[written as usize..];
        }
        Ok(())
    }

    pub(super) fn set_pre_exec_process_group(
        command: &mut std::process::Command,
        process_group_id: u32,
    ) {
        // SAFETY: setpgid is async-signal-safe.
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, process_group_id as i32) == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
}

#[cfg(windows)]
mod windows_job {
    use crate::ResultExt as _;
    use anyhow::{Context as _, Result};
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
        },
    };

    /// A Win32 job object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`:
    /// all processes assigned to the job (and their descendants) are terminated
    /// when the last handle to the job is closed, which happens when this struct
    /// is dropped, or when the OS closes the owning process's handles after it
    /// exits for any reason.
    pub(crate) struct JobObject(HANDLE);

    // SAFETY: Job object handles can be used from any thread.
    unsafe impl Send for JobObject {}
    unsafe impl Sync for JobObject {}

    impl JobObject {
        pub(crate) fn new() -> Result<Self> {
            unsafe {
                let job =
                    Self(CreateJobObjectW(None, None).context("failed to create job object")?);
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .context("failed to set job object limits")?;
                Ok(job)
            }
        }

        pub(crate) fn assign_process(&self, pid: u32) -> Result<()> {
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)
                    .context("failed to open process")?;
                let result = AssignProcessToJobObject(self.0, process)
                    .context("failed to assign process to job object");
                CloseHandle(process).log_err();
                result
            }
        }

        pub(crate) fn terminate(&self) -> Result<()> {
            unsafe { TerminateJobObject(self.0, 1).context("failed to terminate job object") }
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0).log_err();
            }
        }
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn spawn_process_tree(temp_dir: &std::path::Path) -> (Child, u32) {
        let pid_file = temp_dir.join("grandchild_pid");
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("failed to locate util test executable"),
        );
        command
            .args([
                "--exact",
                "process::unix_tests::process_tree_helper",
                "--ignored",
            ])
            .env("ZED_PROCESS_TREE_TEST_PID_FILE", &pid_file);
        let child = Child::spawn(command, Stdio::null(), Stdio::null(), Stdio::null())
            .expect("failed to spawn shell");

        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild_pid = loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for grandchild pid file"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            process_is_alive(grandchild_pid),
            "grandchild should be alive after spawning"
        );
        (child, grandchild_pid)
    }

    fn process_is_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    fn read_pid(path: &std::path::Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(contents) = std::fs::read_to_string(path)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for pid file {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_process_exits(pid: u32, cleanup_group_id: u32, message: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_is_alive(pid) {
            if Instant::now() >= deadline {
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                    libc::killpg(cleanup_group_id as i32, libc::SIGKILL);
                }
                panic!("{message} (pid {pid})");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn spawn_owner(temp_dir: &std::path::Path, mode: &str) -> smol::process::Child {
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("failed to locate util test executable"),
        );
        command
            .args([
                "--exact",
                "process::unix_tests::abrupt_exit_helper",
                "--ignored",
            ])
            .env("ZED_PROCESS_LIFELINE_TEST_DIR", temp_dir)
            .env("ZED_PROCESS_LIFELINE_TEST_MODE", mode);
        let mut command = smol::process::Command::from(command);
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to run owner helper")
    }

    fn read_tree_pids(temp_dir: &std::path::Path) -> (u32, u32) {
        let grandchild_pid = read_pid(&temp_dir.join("grandchild_pid"));
        let session_id = read_pid(&temp_dir.join("session_pid"));
        (grandchild_pid, session_id)
    }

    #[test]
    fn test_drop_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let (child, grandchild_pid) = spawn_process_tree(temp_dir.path());
        let session_id = child.id();

        drop(child);

        assert_process_exits(
            grandchild_pid,
            session_id,
            "grandchild should be terminated after dropping the child",
        );
    }

    #[test]
    fn test_kill_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let (mut child, grandchild_pid) = spawn_process_tree(temp_dir.path());
        let session_id = child.id();

        child.kill().expect("failed to kill process group");

        assert_process_exits(
            grandchild_pid,
            session_id,
            "grandchild should be terminated after killing the child",
        );
    }

    #[test]
    fn test_target_starts_new_session() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let (mut child, grandchild_pid) = spawn_process_tree(temp_dir.path());
        let session_id = child.id();

        assert_eq!(
            unsafe { libc::getpgid(child.id() as i32) },
            session_id as i32
        );
        assert_eq!(
            unsafe { libc::getsid(child.id() as i32) },
            session_id as i32
        );
        assert_eq!(
            unsafe { libc::getsid(grandchild_pid as i32) },
            session_id as i32
        );

        child.kill().expect("failed to kill process session");
        assert_process_exits(
            grandchild_pid,
            session_id,
            "grandchild should exit with its process session",
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_places_process_tree_in_cgroup() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let (mut child, grandchild_pid) = spawn_process_tree(temp_dir.path());
        let Some(cgroup_path) = child.process_tree.cgroup_path().map(ToOwned::to_owned) else {
            return;
        };
        let relative_cgroup = cgroup_path
            .strip_prefix("/sys/fs/cgroup")
            .expect("cgroup path should be below the cgroup v2 mount");
        let expected = format!("0::/{}", relative_cgroup.display());

        for pid in [child.id(), grandchild_pid] {
            let actual = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .expect("failed to read process cgroup");
            assert_eq!(actual.trim(), expected);
        }

        let nested_cgroup = cgroup_path.join("nested").join("child");
        std::fs::create_dir(cgroup_path.join("nested"))
            .expect("failed to create nested test cgroup");
        std::fs::create_dir(&nested_cgroup).expect("failed to create nested child cgroup");

        child.kill().expect("failed to kill cgroup");
        drop(child);
        let deadline = Instant::now() + Duration::from_secs(2);
        while cgroup_path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for cgroup removal: {}",
                cgroup_path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_systemd_scope_contains_process_tree() {
        let Ok(mut cgroup) = linux_cgroup::Cgroup::spawn_systemd_for_test() else {
            return;
        };
        let cgroup_path = cgroup.path().to_path_buf();
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let pid_file = temp_dir.path().join("grandchild_pid");
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("failed to locate util test executable"),
        );
        command
            .args([
                "--exact",
                "process::unix_tests::process_tree_helper",
                "--ignored",
            ])
            .env("ZED_PROCESS_TREE_TEST_PID_FILE", &pid_file);
        cgroup
            .configure_command(&mut command)
            .expect("failed to configure systemd scope target");
        let mut command = smol::process::Command::from(command);
        let process = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn systemd scope target");
        let grandchild_pid = read_pid(&pid_file);

        let relative_cgroup = cgroup_path
            .strip_prefix("/sys/fs/cgroup")
            .expect("scope path should be below the cgroup v2 mount");
        let expected = format!("0::/{}", relative_cgroup.display());
        for pid in [process.id(), grandchild_pid] {
            let actual = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .expect("failed to read process cgroup");
            assert_eq!(actual.trim(), expected);
        }

        cgroup.kill().expect("failed to kill systemd scope");
        drop(cgroup);
        assert_process_exits(
            grandchild_pid,
            process.id(),
            "systemd scope grandchild should exit",
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while cgroup_path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for systemd scope removal: {}",
                cgroup_path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_session_fallback_terminates_process_tree() {
        let mut process_group =
            unix_process_group::ProcessGroup::spawn().expect("failed to spawn fallback watchdog");
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let pid_file = temp_dir.path().join("grandchild_pid");
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("failed to locate util test executable"),
        );
        command
            .args([
                "--exact",
                "process::unix_tests::process_tree_helper",
                "--ignored",
            ])
            .env("ZED_PROCESS_TREE_TEST_PID_FILE", &pid_file);
        process_group
            .configure_command(&mut command)
            .expect("failed to configure fallback target");
        let mut command = smol::process::Command::from(command);
        let process = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn fallback target");
        let grandchild_pid = read_pid(&pid_file);

        process_group.kill().expect("failed to kill fallback tree");
        drop(process_group);
        assert_process_exits(
            grandchild_pid,
            process.id(),
            "session fallback grandchild should exit",
        );
    }

    #[test]
    fn test_watchdog_spawn_failure_prevents_target_spawn() {
        let Err(error) = unix_process_group::ProcessGroup::spawn_with_program(
            std::ffi::OsStr::new("/zed-test-missing-shell"),
        ) else {
            panic!("missing watchdog program should fail");
        };

        assert!(
            error
                .to_string()
                .contains("failed to spawn process watchdog")
        );
    }

    #[test]
    fn test_process_group_setup_failure_prevents_target_spawn() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 0"]);
        unix_process_group::set_pre_exec_process_group(&mut command, u32::MAX);
        let mut command = smol::process::Command::from(command);

        let Err(error) = command.spawn() else {
            panic!("invalid process group should fail");
        };

        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn test_session_setup_failure_prevents_target_spawn() {
        let process_group =
            unix_process_group::ProcessGroup::spawn().expect("failed to spawn process watchdog");
        let watchdog_pid = process_group.id;
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 0"]);
        unix_process_group::set_pre_exec_process_group(&mut command, 0);
        process_group
            .configure_command(&mut command)
            .expect("failed to configure target process");
        let mut command = smol::process::Command::from(command);

        let Err(error) = command.spawn() else {
            panic!("session leader should not be able to call setsid");
        };
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        drop(process_group);
        assert_process_exits(
            watchdog_pid,
            watchdog_pid,
            "watchdog should exit after target setup failure",
        );
    }

    #[test]
    fn test_target_spawn_failure_stops_watchdog() {
        let process_group =
            unix_process_group::ProcessGroup::spawn().expect("failed to spawn process watchdog");
        let process_group_id = process_group.id;
        let mut command = std::process::Command::new("/zed-test-missing-target");
        process_group
            .configure_command(&mut command)
            .expect("failed to configure target process");
        let mut command = smol::process::Command::from(command);

        assert!(command.spawn().is_err(), "missing target should fail");
        drop(process_group);

        assert_process_exits(
            process_group_id,
            process_group_id,
            "watchdog should exit after target spawn failure",
        );
    }

    #[test]
    fn test_output_waits_for_process() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf stdout; printf stderr >&2"]);
        let child = Child::spawn(command, Stdio::null(), Stdio::piped(), Stdio::piped())
            .expect("failed to spawn shell");

        let output = smol::block_on(child.output()).expect("failed to collect process output");

        assert!(output.status.success());
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
    }

    #[test]
    fn test_parent_exit_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let mut owner = spawn_owner(temp_dir.path(), "exit");
        let status = smol::block_on(owner.status()).expect("failed to run abrupt-exit helper");
        assert!(status.success(), "abrupt-exit helper failed: {status}");
        let (grandchild_pid, session_id) = read_tree_pids(temp_dir.path());

        assert_process_exits(
            grandchild_pid,
            session_id,
            "grandchild should be terminated after its owning process exits",
        );
    }

    fn assert_signal_terminates_grandchildren(signal: libc::c_int) {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let mut owner = spawn_owner(temp_dir.path(), "wait");
        let (grandchild_pid, session_id) = read_tree_pids(temp_dir.path());

        assert_eq!(unsafe { libc::kill(owner.id() as i32, signal) }, 0);
        smol::block_on(owner.status()).expect("failed to wait for owner helper");

        assert_process_exits(
            grandchild_pid,
            session_id,
            "grandchild should be terminated after its owner is signaled",
        );
    }

    #[test]
    fn test_sigterm_terminates_grandchildren() {
        assert_signal_terminates_grandchildren(libc::SIGTERM);
    }

    #[test]
    fn test_sigkill_terminates_grandchildren() {
        assert_signal_terminates_grandchildren(libc::SIGKILL);
    }

    #[test]
    fn test_sigabrt_terminates_grandchildren() {
        assert_signal_terminates_grandchildren(libc::SIGABRT);
    }

    #[test]
    #[ignore]
    fn test_parent_exit_stress() {
        for _ in 0..1000 {
            let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
            let mut owner = spawn_owner(temp_dir.path(), "exit");
            let status = smol::block_on(owner.status()).expect("failed to run abrupt-exit helper");
            assert!(status.success(), "abrupt-exit helper failed: {status}");
            let (grandchild_pid, session_id) = read_tree_pids(temp_dir.path());
            assert_process_exits(
                grandchild_pid,
                session_id,
                "grandchild should be terminated during stress test",
            );
        }
    }

    #[test]
    #[ignore]
    fn process_tree_helper() {
        let Some(pid_file) = std::env::var_os("ZED_PROCESS_TREE_TEST_PID_FILE") else {
            return;
        };
        let mut command = std::process::Command::new("sleep");
        command.arg("60");
        unix_process_group::set_pre_exec_process_group(&mut command, 0);
        let mut command = smol::process::Command::from(command);
        let mut grandchild = command.spawn().expect("failed to spawn grandchild");
        std::fs::write(pid_file, grandchild.id().to_string())
            .expect("failed to write grandchild pid");
        smol::block_on(grandchild.status()).expect("failed to wait for grandchild");
    }

    #[test]
    #[ignore]
    fn abrupt_exit_helper() {
        let Some(temp_dir) = std::env::var_os("ZED_PROCESS_LIFELINE_TEST_DIR") else {
            return;
        };
        let temp_dir = std::path::Path::new(&temp_dir);
        let (child, _) = spawn_process_tree(temp_dir);
        std::fs::write(temp_dir.join("session_pid"), child.id().to_string())
            .expect("failed to write process session pid");

        let core_limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_CORE, &core_limit) },
            0,
            "failed to disable core dumps for signal tests"
        );

        match std::env::var("ZED_PROCESS_LIFELINE_TEST_MODE").as_deref() {
            Ok("exit") => unsafe {
                libc::_exit(0);
            },
            Ok("wait") => loop {
                std::thread::park();
            },
            mode => panic!("unexpected helper mode: {mode:?}"),
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Spawns a process tree `powershell -> ping` via `Child::spawn` and
    /// returns the `Child` along with the pid of the grandchild (`ping`).
    fn spawn_process_tree(temp_dir: &std::path::Path) -> (Child, u32) {
        let pid_file = temp_dir.join("grandchild_pid");
        let mut command = std::process::Command::new("powershell.exe");
        command.args(["-NoProfile", "-Command"]).arg(format!(
            "$p = Start-Process -FilePath ping.exe -ArgumentList @('-n','60','127.0.0.1') -PassThru -WindowStyle Hidden; \
             Set-Content -LiteralPath '{}' -Value $p.Id; \
             Wait-Process -Id $p.Id",
            pid_file.display()
        ));
        let child = Child::spawn(command, Stdio::null(), Stdio::null(), Stdio::null())
            .expect("failed to spawn powershell");

        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild_pid = loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for grandchild pid file"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            process_is_alive(grandchild_pid),
            "grandchild should be alive after spawning"
        );
        (child, grandchild_pid)
    }

    fn process_is_alive(pid: u32) -> bool {
        use windows::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };

        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut exit_code = 0u32;
            let alive = GetExitCodeProcess(handle, &mut exit_code).is_ok()
                && exit_code == STILL_ACTIVE.0 as u32;
            CloseHandle(handle).expect("failed to close process handle");
            alive
        }
    }

    fn assert_process_exits(pid: u32, message: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_is_alive(pid) {
            assert!(Instant::now() < deadline, "{message} (pid {pid})");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn test_kill_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut child, grandchild_pid) = spawn_process_tree(temp_dir.path());

        child.kill().expect("failed to kill child");

        assert_process_exits(
            grandchild_pid,
            "grandchild should be terminated after killing the child",
        );
    }

    #[test]
    fn test_drop_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (child, grandchild_pid) = spawn_process_tree(temp_dir.path());

        drop(child);

        assert_process_exits(
            grandchild_pid,
            "grandchild should be terminated after dropping the child",
        );
    }
}
