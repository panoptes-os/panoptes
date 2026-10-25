//! cgroup v2 control for Panoptes.
//!
//! A [`Cgroup`] is a directory under `/sys/fs/cgroup`; every operation is a
//! read or write of one of its interface files. panoptes-resd owns the
//! subtree its container was given and parks agents with three of them:
//! `cgroup.freeze`, `memory.zswap.writeback`, and `memory.reclaim`.

mod parse;
mod spawn;

pub use parse::{Events, MemoryStat, Pressure, PressureLine};
pub use spawn::{Child, MAX_CHILD_FD};

use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::inotify;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const ROOT: &str = "/sys/fs/cgroup";

#[derive(Debug)]
pub enum Error {
    /// A read, write, or directory operation on `path` failed.
    Io { path: PathBuf, source: io::Error },
    /// A system call outside the cgroup tree failed.
    Os {
        call: &'static str,
        source: io::Error,
    },
    /// An interface file did not have the documented shape.
    Parse { path: PathBuf },
    /// `cgroup.events` did not reach the wanted state in time.
    Timeout { path: PathBuf, waited: Duration },
    /// The running kernel lacks a feature.
    Unsupported(&'static str),
    InvalidArgument(&'static str),
}

impl Error {
    fn io(path: &Path, source: io::Error) -> Self {
        Error::Io {
            path: path.to_owned(),
            source,
        }
    }

    fn os(call: &'static str, source: io::Error) -> Self {
        Error::Os { call, source }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Os { call, source } => write!(f, "{call}: {source}"),
            Error::Parse { path } => write!(f, "{}: unexpected format", path.display()),
            Error::Timeout { path, waited } => {
                write!(f, "{}: no change after {waited:?}", path.display())
            }
            Error::Unsupported(what) => write!(f, "kernel lacks {what}"),
            Error::InvalidArgument(what) => write!(f, "invalid argument: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } | Error::Os { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What `memory.reclaim` managed. The kernel fails the write with EAGAIN when
/// it reclaimed less than asked, which for parking is a normal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reclaim {
    Full,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cgroup {
    path: PathBuf,
}

impl Cgroup {
    /// Open an existing group by its directory.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let procs = path.join("cgroup.procs");
        std::fs::metadata(&procs).map_err(|e| Error::io(&procs, e))?;
        Ok(Cgroup { path })
    }

    /// The group this process runs in.
    pub fn current() -> Result<Self> {
        let file = Path::new("/proc/self/cgroup");
        let text = std::fs::read_to_string(file).map_err(|e| Error::io(file, e))?;
        let rel = parse::proc_cgroup(&text).ok_or_else(|| Error::Parse {
            path: file.to_owned(),
        })?;
        Cgroup::open(Path::new(ROOT).join(rel.trim_start_matches('/')))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open the child `name`, creating it if it does not exist.
    pub fn child(&self, name: &str) -> Result<Self> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return Err(Error::InvalidArgument("child name"));
        }
        let path = self.path.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::io(&path, e)),
        }
        Cgroup::open(path)
    }

    /// Remove the group. It must have no processes and no children.
    pub fn remove(self) -> Result<()> {
        std::fs::remove_dir(&self.path).map_err(|e| Error::io(&self.path, e))
    }

    pub fn procs(&self) -> Result<Vec<u32>> {
        let text = self.read("cgroup.procs")?;
        text.lines()
            .map(|l| l.parse().map_err(|_| self.parse_error("cgroup.procs")))
            .collect()
    }

    /// Move a process, with all its threads, into this group.
    pub fn attach(&self, pid: u32) -> Result<()> {
        self.write("cgroup.procs", &pid.to_string())
    }

    /// Controllers available to this group's children.
    pub fn controllers(&self) -> Result<Vec<String>> {
        Ok(self
            .read("cgroup.controllers")?
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    }

    /// Enable controllers for this group's children. The group must hold no
    /// processes itself (cgroup v2's no-internal-processes rule).
    pub fn enable_controllers(&self, names: &[&str]) -> Result<()> {
        let line: Vec<String> = names.iter().map(|n| format!("+{n}")).collect();
        self.write("cgroup.subtree_control", &line.join(" "))
    }

    pub fn memory_current(&self) -> Result<u64> {
        self.read_u64("memory.current")
    }

    pub fn memory_stat(&self) -> Result<MemoryStat> {
        parse::memory_stat(&self.read("memory.stat")?).ok_or_else(|| self.parse_error("memory.stat"))
    }

    pub fn memory_pressure(&self) -> Result<Pressure> {
        self.pressure("memory.pressure")
    }

    pub fn cpu_pressure(&self) -> Result<Pressure> {
        self.pressure("cpu.pressure")
    }

    /// Total CPU time used by the group, in µs.
    pub fn cpu_usage_us(&self) -> Result<u64> {
        parse::cpu_usage_us(&self.read("cpu.stat")?).ok_or_else(|| self.parse_error("cpu.stat"))
    }

    pub fn events(&self) -> Result<Events> {
        parse::events(&self.read("cgroup.events")?).ok_or_else(|| self.parse_error("cgroup.events"))
    }

    /// Freeze the group and wait until every task in it has stopped. Returns
    /// the time from the write to the kernel reporting `frozen 1`.
    pub fn freeze(&self, timeout: Duration) -> Result<Duration> {
        self.set_frozen(true, timeout)
    }

    pub fn thaw(&self, timeout: Duration) -> Result<Duration> {
        self.set_frozen(false, timeout)
    }

    fn set_frozen(&self, frozen: bool, timeout: Duration) -> Result<Duration> {
        let start = Instant::now();
        let watch = self.watch_events()?;
        self.write("cgroup.freeze", if frozen { "1" } else { "0" })?;
        self.wait_events(&watch, start, timeout, |e| e.frozen == frozen)?;
        Ok(start.elapsed())
    }

    /// Wait until no process is left in the group or its descendants.
    pub fn wait_empty(&self, timeout: Duration) -> Result<Duration> {
        let start = Instant::now();
        let watch = self.watch_events()?;
        self.wait_events(&watch, start, timeout, |e| !e.populated)?;
        Ok(start.elapsed())
    }

    /// The kernel signals every change to `cgroup.events` as a modify event,
    /// so waiting is an inotify watch plus a re-read, with no polling loop.
    fn watch_events(&self) -> Result<std::os::fd::OwnedFd> {
        let path = self.path.join("cgroup.events");
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)
            .map_err(|e| Error::os("inotify_init1", e.into()))?;
        inotify::add_watch(&fd, &path, inotify::WatchFlags::MODIFY)
            .map_err(|e| Error::io(&path, e.into()))?;
        Ok(fd)
    }

    fn wait_events(
        &self,
        watch: &std::os::fd::OwnedFd,
        start: Instant,
        timeout: Duration,
        done: impl Fn(Events) -> bool,
    ) -> Result<()> {
        let mut buf = [0u8; 1024];
        loop {
            if done(self.events()?) {
                return Ok(());
            }
            let left = timeout
                .checked_sub(start.elapsed())
                .ok_or_else(|| Error::Timeout {
                    path: self.path.join("cgroup.events"),
                    waited: start.elapsed(),
                })?;
            let ts = Timespec {
                tv_sec: left.as_secs() as _,
                tv_nsec: left.subsec_nanos() as _,
            };
            let mut fds = [PollFd::new(watch, PollFlags::IN)];
            match rustix::event::poll(&mut fds, Some(&ts)) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(Error::os("poll", e.into())),
            }
            // Drain the queued events; the next read of cgroup.events is the
            // source of truth.
            while rustix::io::read(watch, &mut buf).is_ok_and(|n| n > 0) {}
        }
    }

    /// Ask the kernel to reclaim `bytes` from the group.
    pub fn reclaim(&self, bytes: u64) -> Result<Reclaim> {
        let path = self.path.join("memory.reclaim");
        match std::fs::write(&path, bytes.to_string()) {
            Ok(()) => Ok(Reclaim::Full),
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => Ok(Reclaim::Partial),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// With writeback off, reclaimed anonymous pages stay compressed in zswap
    /// and never reach the swap device (Linux 6.8 or later).
    pub fn set_zswap_writeback(&self, on: bool) -> Result<()> {
        if !self.path.join("memory.zswap.writeback").exists() {
            return Err(Error::Unsupported("memory.zswap.writeback (Linux 6.8 or later)"));
        }
        self.write("memory.zswap.writeback", if on { "1" } else { "0" })
    }

    pub fn zswap_writeback(&self) -> Result<bool> {
        Ok(self.read_u64("memory.zswap.writeback")? != 0)
    }

    /// Set `cpu.weight`, 1 to 10000; the default is 100.
    pub fn set_cpu_weight(&self, weight: u16) -> Result<()> {
        if !(1..=10_000).contains(&weight) {
            return Err(Error::InvalidArgument("cpu.weight must be 1 to 10000"));
        }
        self.write("cpu.weight", &weight.to_string())
    }

    pub fn cpu_weight(&self) -> Result<u16> {
        let v = self.read_u64("cpu.weight")?;
        u16::try_from(v).map_err(|_| self.parse_error("cpu.weight"))
    }

    /// Start `argv` inside this group, so its first page is charged here.
    /// `env` adds to or overrides this process's environment.
    pub fn spawn<S: AsRef<OsStr>>(&self, argv: &[S], env: &[(&str, &str)]) -> Result<Child> {
        let argv: Vec<&OsStr> = argv.iter().map(AsRef::as_ref).collect();
        let env: Vec<(&OsStr, &OsStr)> = env
            .iter()
            .map(|(k, v)| (OsStr::new(*k), OsStr::new(*v)))
            .collect();
        spawn::spawn_into(&self.path, &argv, &env, &[])
    }

    /// Like [`Cgroup::spawn`], and the child also receives each descriptor in
    /// `fds` at the given number (at most [`MAX_CHILD_FD`], eight entries).
    pub fn spawn_with_fds<S: AsRef<OsStr>>(
        &self,
        argv: &[S],
        env: &[(&str, &str)],
        fds: &[(BorrowedFd<'_>, RawFd)],
    ) -> Result<Child> {
        let argv: Vec<&OsStr> = argv.iter().map(AsRef::as_ref).collect();
        let env: Vec<(&OsStr, &OsStr)> = env
            .iter()
            .map(|(k, v)| (OsStr::new(*k), OsStr::new(*v)))
            .collect();
        let fds: Vec<(RawFd, RawFd)> = fds.iter().map(|(f, to)| (f.as_raw_fd(), *to)).collect();
        spawn::spawn_into(&self.path, &argv, &env, &fds)
    }

    /// SIGKILL every process in the group and its descendants
    /// (`cgroup.kill`, Linux 5.14 or later). Works on a frozen group.
    pub fn kill_all(&self) -> Result<()> {
        if !self.path.join("cgroup.kill").exists() {
            return Err(Error::Unsupported("cgroup.kill (Linux 5.14 or later)"));
        }
        self.write("cgroup.kill", "1")
    }

    fn read(&self, file: &str) -> Result<String> {
        let path = self.path.join(file);
        std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))
    }

    fn read_u64(&self, file: &str) -> Result<u64> {
        self.read(file)?
            .trim()
            .parse()
            .map_err(|_| self.parse_error(file))
    }

    fn write(&self, file: &str, value: &str) -> Result<()> {
        let path = self.path.join(file);
        std::fs::write(&path, value).map_err(|e| Error::io(&path, e))
    }

    fn pressure(&self, file: &str) -> Result<Pressure> {
        parse::pressure(&self.read(file)?).ok_or_else(|| self.parse_error(file))
    }

    fn parse_error(&self, file: &str) -> Error {
        Error::Parse {
            path: self.path.join(file),
        }
    }
}
