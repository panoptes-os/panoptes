//! The desk's client of panoptes-resd: the I/O layer of the lease. The desk
//! thread calls it when [`crate::lease::Lease`] says to acquire or release.
//!
//! One JSON line each way per request, on a connection kept between calls:
//!
//! ```text
//! > {"op":"acquire"}      < {"ok":true,"from":"parked",...}
//! > {"op":"release"}      < {"ok":true,...}
//! ```
//!
//! A refusal fails the call that asked, and the next call connects again:
//! resd closes a connection it refuses for being full. When resd cannot be
//! reached or does not answer in time, the desk thaws the kiosk itself, so
//! the agent keeps working without residency.

use crate::owner::{Acquired, Residency};
use panoptes_cgroup::Cgroup;
use serde_json::Value;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long one request may take, thawing a parked kiosk included.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the desk's own thaw may take when resd is out of reach.
const THAW_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct Resd {
    socket: PathBuf,
    kiosk: Cgroup,
    conn: Option<BufReader<UnixStream>>,
    io_timeout: Duration,
}

/// Why a request got no `ok`.
#[derive(Debug)]
enum Failed {
    /// resd answered no; the reason is its own.
    Refused(String),
    /// No answer: resd is down, hung, or closed the connection.
    Unreachable(String),
}

impl Resd {
    pub fn new(socket: PathBuf, kiosk: Cgroup) -> Resd {
        Resd {
            socket,
            kiosk,
            conn: None,
            io_timeout: IO_TIMEOUT,
        }
    }

    fn send(&mut self, op: &str) -> Result<Value, Failed> {
        let r = self.round_trip(op);
        if r.is_err() {
            // The stream may hold half a reply, or resd may have closed it
            // after refusing; start over next time.
            self.conn = None;
        }
        r
    }

    fn round_trip(&mut self, op: &str) -> Result<Value, Failed> {
        let unreachable = |e: std::io::Error| Failed::Unreachable(e.to_string());
        if self.conn.is_none() {
            let s = UnixStream::connect(&self.socket)
                .map_err(|e| Failed::Unreachable(format!("{}: {e}", self.socket.display())))?;
            s.set_read_timeout(Some(self.io_timeout))
                .map_err(unreachable)?;
            s.set_write_timeout(Some(self.io_timeout))
                .map_err(unreachable)?;
            self.conn = Some(BufReader::new(s));
        }
        let conn = self.conn.as_mut().expect("connected above");
        let sent = conn
            .get_mut()
            .write_all(format!("{{\"op\":\"{op}\"}}\n").as_bytes());
        // A full resd writes its refusal and closes without reading, which
        // can come before this write. The refusal is still here to read.
        if let Err(e) = sent
            && e.kind() != ErrorKind::BrokenPipe
        {
            return Err(unreachable(e));
        }
        let mut line = String::new();
        match conn.read_line(&mut line) {
            Ok(0) => return Err(Failed::Unreachable("resd closed the connection".into())),
            Ok(_) => {}
            Err(e) => return Err(unreachable(e)),
        }
        let v: Value = serde_json::from_str(&line)
            .map_err(|e| Failed::Unreachable(format!("bad reply: {e}")))?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v)
        } else {
            let why = v["error"].as_str().unwrap_or("refused");
            Err(Failed::Refused(why.to_owned()))
        }
    }
}

impl Residency for Resd {
    fn acquire(&mut self) -> Result<Acquired, String> {
        let start = Instant::now();
        let woke = match self.send("acquire") {
            Ok(v) => matches!(v["from"].as_str(), Some("waiting" | "parked")),
            Err(Failed::Refused(why)) => {
                return Err(format!("panoptes-resd refused the kiosk: {why}"));
            }
            Err(Failed::Unreachable(e)) => {
                eprintln!("panoptes-desk: resd acquire: {e}; thawing the kiosk itself");
                let frozen = self.kiosk.events().map(|ev| ev.frozen).unwrap_or(false);
                if frozen {
                    let _ = self.kiosk.thaw(THAW_TIMEOUT);
                }
                frozen
            }
        };
        let ms = (start.elapsed().as_secs_f64() * 100_000.0).round() / 100.0;
        if woke {
            eprintln!("panoptes-desk: woke kiosk in {ms} ms");
        }
        Ok(Acquired { ms, woke })
    }

    fn release(&mut self) {
        match self.send("release") {
            Ok(_) => {}
            Err(Failed::Refused(why)) => eprintln!("panoptes-desk: resd refused release: {why}"),
            Err(Failed::Unreachable(e)) => eprintln!("panoptes-desk: resd release: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::mpsc;

    /// A kiosk directory that `Cgroup::open` accepts, and a socket path
    /// beside it.
    fn dir(name: &str) -> (PathBuf, Cgroup) {
        let dir = std::env::temp_dir().join(format!("panoptes-desk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cgroup.procs"), "").unwrap();
        let kiosk = Cgroup::open(&dir).unwrap();
        (dir, kiosk)
    }

    /// A stand-in panoptes-resd on `socket` that answers each op with
    /// `answer(op)`, or not at all for `None`, and reports the ops it saw.
    fn fake(socket: &Path, answer: fn(&str) -> Option<&'static str>) -> mpsc::Receiver<String> {
        let listener = UnixListener::bind(socket).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut w = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { return };
                let v: Value = serde_json::from_str(&line).unwrap();
                let op = v["op"].as_str().unwrap().to_owned();
                let reply = answer(&op);
                tx.send(op).unwrap();
                match reply {
                    Some(r) => w.write_all(format!("{r}\n").as_bytes()).unwrap(),
                    None => std::thread::sleep(Duration::from_secs(60)),
                }
            }
        });
        rx
    }

    #[test]
    fn acquire_reports_a_wake_and_release_follows_on_one_connection() {
        let (dir, kiosk) = dir("ops");
        let socket = dir.join("resd.sock");
        let ops = fake(&socket, |op| {
            Some(match op {
                "acquire" => r#"{"ok":true,"from":"parked"}"#,
                _ => r#"{"ok":true}"#,
            })
        });
        let mut resd = Resd::new(socket, kiosk);
        assert!(resd.acquire().unwrap().woke);
        resd.release();
        assert_eq!(ops.recv().unwrap(), "acquire");
        assert_eq!(ops.recv().unwrap(), "release");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refusal_fails_the_call() {
        let (dir, kiosk) = dir("refuse");
        let socket = dir.join("resd.sock");
        let _ops = fake(&socket, |_| Some(r#"{"ok":false,"error":"unknown agent"}"#));
        let e = Resd::new(socket, kiosk).acquire().unwrap_err();
        assert!(e.contains("refused") && e.contains("unknown agent"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `dir` as a frozen kiosk. A thaw would rewrite its `cgroup.freeze`.
    fn freeze(dir: &Path) {
        std::fs::write(dir.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        std::fs::write(dir.join("cgroup.freeze"), "1\n").unwrap();
    }

    fn assert_frozen(dir: &Path) {
        let asks = std::fs::read_to_string(dir.join("cgroup.freeze")).unwrap();
        assert_eq!(asks, "1\n", "the desk thawed the kiosk itself");
    }

    const FULL: &[u8] = b"{\"ok\":false,\"error\":\"too many connections\"}\n";

    #[test]
    fn a_full_resd_fails_each_call_and_the_kiosk_stays_frozen() {
        let (dir, kiosk) = dir("full");
        freeze(&dir);
        let socket = dir.join("resd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                stream.unwrap().write_all(FULL).unwrap();
            }
        });
        let mut resd = Resd::new(socket, kiosk);
        for call in 0..3 {
            let e = resd.acquire().expect_err("no refusal: thawed it instead");
            assert!(e.contains("too many connections"), "call {call}: {e}");
        }
        assert_frozen(&dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refusal_that_beat_the_write_is_still_a_refusal() {
        let (dir, kiosk) = dir("beat");
        freeze(&dir);
        let (mut resd_end, desk_end) = UnixStream::pair().unwrap();
        resd_end.write_all(FULL).unwrap();
        drop(resd_end);
        let mut resd = Resd::new(dir.join("no.sock"), kiosk);
        resd.conn = Some(BufReader::new(desk_end));
        let e = resd.acquire().expect_err("no refusal: thawed it instead");
        assert!(e.contains("too many connections"), "{e}");
        assert!(resd.conn.is_none(), "kept a closed connection");
        assert_frozen(&dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_resd_the_desk_carries_on() {
        let (dir, kiosk) = dir("absent");
        let got = Resd::new(dir.join("no.sock"), kiosk).acquire().unwrap();
        // The fake kiosk reports no freeze, so there was nothing to thaw.
        assert!(!got.woke);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_silent_resd_times_out_and_the_next_call_reconnects() {
        let (dir, kiosk) = dir("silent");
        let socket = dir.join("resd.sock");
        let _ops = fake(&socket, |_| None);
        let mut resd = Resd::new(socket, kiosk);
        resd.io_timeout = Duration::from_millis(100);
        let start = Instant::now();
        assert!(!resd.acquire().unwrap().woke);
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(resd.conn.is_none(), "kept a connection with half a reply");
        let _ = std::fs::remove_dir_all(dir);
    }
}
