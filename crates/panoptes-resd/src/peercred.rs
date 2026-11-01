//! SO_PEERCRED into `libc::ucred`. rustix's `UCred` holds a NonZero pid, but
//! the kernel reports pid 0 for a peer outside this PID namespace.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

pub fn of(stream: &UnixStream) -> Option<libc::ucred> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` outlive the call, and `len` is `cred`'s size.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    (rc == 0).then_some(cred)
}
