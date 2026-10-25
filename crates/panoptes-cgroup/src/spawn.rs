//! Start a process directly inside a cgroup with `clone3(CLONE_INTO_CGROUP)`.
//!
//! A process moved into a group after it starts leaves the memory it touched
//! first charged to its old group (E11), so kiosks must start where they
//! will be parked.

use crate::{Error, Result};
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const CLONE_PIDFD: u64 = 0x1000;
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;

/// `struct clone_args`, through `cgroup` (CLONE_ARGS_SIZE_VER2, 88 bytes).
#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

/// A process started by [`crate::Cgroup::spawn`]. Dropping it kills and
/// reaps the process unless [`Child::wait`] already did.
#[derive(Debug)]
pub struct Child {
    pid: u32,
    _pidfd: OwnedFd,
    reaped: bool,
}

impl Child {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn kill(&self) -> Result<()> {
        // SAFETY: kill(2) on a pid this process created and has not reaped,
        // so the pid cannot have been reused.
        let rc = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
        if rc != 0 {
            return Err(Error::os("kill", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Wait for the process to exit and return its raw wait status.
    pub fn wait(mut self) -> Result<i32> {
        let status = self.reap()?;
        Ok(status)
    }

    fn reap(&mut self) -> Result<i32> {
        let mut status = 0;
        loop {
            // SAFETY: waitpid(2) on our own child; `status` is a valid
            // out-pointer for the duration of the call.
            let rc = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, 0) };
            if rc >= 0 {
                self.reaped = true;
                return Ok(status);
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINTR) {
                return Err(Error::os("waitpid", err));
            }
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.kill();
            let _ = self.reap();
        }
    }
}

/// Resolve `program` the way execvp would, before forking, so the child only
/// calls execve.
fn resolve(program: &OsStr) -> Result<PathBuf> {
    let p = Path::new(program);
    if program.as_bytes().contains(&b'/') {
        return Ok(p.to_owned());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(p))
        .find(|c| c.is_file())
        .ok_or_else(|| Error::os("resolve", std::io::ErrorKind::NotFound.into()))
}

fn cstring(s: &OsStr) -> Result<CString> {
    CString::new(s.as_bytes())
        .map_err(|_| Error::os("argument", std::io::ErrorKind::InvalidInput.into()))
}

/// Highest descriptor `spawn_into` can hand to a child.
pub const MAX_CHILD_FD: RawFd = 63;

/// `fds` maps a descriptor of this process to the number the child sees,
/// such as Chromium's `--remote-debugging-pipe` ends on 3 and 4.
pub(crate) fn spawn_into(
    cgroup: &Path,
    argv: &[&OsStr],
    env: &[(&OsStr, &OsStr)],
    fds: &[(RawFd, RawFd)],
) -> Result<Child> {
    if fds.iter().any(|&(_, to)| !(0..=MAX_CHILD_FD).contains(&to)) {
        return Err(Error::InvalidArgument("child fd above MAX_CHILD_FD"));
    }
    let program = argv
        .first()
        .ok_or_else(|| Error::os("spawn", std::io::ErrorKind::InvalidInput.into()))?;
    let path = cstring(resolve(program)?.as_os_str())?;
    let args: Vec<CString> = argv.iter().map(|a| cstring(a)).collect::<Result<_>>()?;
    let mut vars: Vec<CString> = std::env::vars_os()
        .filter(|(k, _)| !env.iter().any(|(ek, _)| ek == k))
        .map(|(k, v)| [k.as_bytes(), b"=", v.as_bytes()].concat())
        .chain(env.iter().map(|(k, v)| [k.as_bytes(), b"=", v.as_bytes()].concat()))
        .map(|kv| CString::new(kv).map_err(|_| Error::os("env", std::io::ErrorKind::InvalidInput.into())))
        .collect::<Result<_>>()?;
    vars.sort();
    let mut arg_ptrs: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    arg_ptrs.push(std::ptr::null());
    let mut env_ptrs: Vec<*const libc::c_char> = vars.iter().map(|v| v.as_ptr()).collect();
    env_ptrs.push(std::ptr::null());

    let dir = File::open(cgroup).map_err(|e| Error::io(cgroup, e))?;
    let mut pidfd: libc::c_int = -1;
    let clone_args = CloneArgs {
        flags: CLONE_PIDFD | CLONE_INTO_CGROUP,
        pidfd: &raw mut pidfd as u64,
        exit_signal: libc::SIGCHLD as u64,
        cgroup: dir.as_raw_fd() as u64,
        ..CloneArgs::default()
    };

    // SAFETY: clone3 without a new stack behaves like fork(2): the child
    // runs on a copy of this thread's stack. Everything the child touches
    // (path, arg and env pointer arrays) was allocated before the call, and
    // the child only calls fcntl, dup2, execve, and _exit, all
    // async-signal-safe.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &raw const clone_args,
            size_of::<CloneArgs>(),
        )
    };
    if ret == 0 {
        // SAFETY: in the child; see above. fcntl and dup2 are
        // async-signal-safe. Each source is first copied above every target,
        // so a source that already sits on another entry's target survives,
        // and dup2 then clears close-on-exec on the target only.
        unsafe {
            let mut moved = [-1 as RawFd; 8];
            if fds.len() > moved.len() {
                libc::_exit(126);
            }
            for (i, &(from, _)) in fds.iter().enumerate() {
                moved[i] = libc::fcntl(from, libc::F_DUPFD_CLOEXEC, MAX_CHILD_FD + 1);
                if moved[i] < 0 {
                    libc::_exit(126);
                }
            }
            for (i, &(_, to)) in fds.iter().enumerate() {
                if libc::dup2(moved[i], to) < 0 {
                    libc::_exit(126);
                }
            }
            libc::execve(path.as_ptr(), arg_ptrs.as_ptr(), env_ptrs.as_ptr());
            libc::_exit(127);
        }
    }
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        return Err(match err.raw_os_error() {
            Some(libc::ENOSYS) => Error::Unsupported("clone3 (Linux 5.3 or later)"),
            // Kernels before 5.7 reject the nonzero `cgroup` field.
            Some(libc::E2BIG) => {
                Error::Unsupported("CLONE_INTO_CGROUP (Linux 5.7 or later, cgroup v2)")
            }
            _ => Error::io(cgroup, err),
        });
    }
    // SAFETY: CLONE_PIDFD succeeded, so the kernel stored a new, owned,
    // close-on-exec descriptor in `pidfd`.
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
    Ok(Child {
        pid: ret as u32,
        _pidfd: pidfd,
        reaped: false,
    })
}
