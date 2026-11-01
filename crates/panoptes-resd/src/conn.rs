//! The control socket. One thread accepts, and one thread per connection
//! reads its JSON lines, asks the owner, and writes the replies. The I/O
//! layer, at the trust boundary: it checks who may name an agent, counts
//! connections against a cap, and decides nothing else.
//!
//! ```text
//! > {"op":"acquire"}               from a desk: thaw or resume its kiosk
//! < {"ok":true,"state":"acting","from":"parked","ms":0.21}
//! > {"op":"release"}               from a desk: freeze its kiosk
//! > {"op":"status"}                anyone: every agent and the host
//! > {"op":"status","since":N}      also the transitions after sequence N
//! > {"op":"acquire","agent":"x"}   root only: name the agent explicitly
//! < {"ok":false,"error":"too many connections"}   past the cap, then closed
//! ```
//!
//! A desk's agent is the cgroup its connecting process runs in, read from
//! the peer's credentials, so one agent cannot freeze another.
//!
//! A connection has no idle limit. A desk keeps one open for as long as its
//! agent waits. Closing it would make the desk's next acquire thaw the kiosk
//! itself, with resd still counting the kiosk as parked.

use crate::cg;
use crate::owner::{Msg, Op, Snapshot};
use panoptes_cgroup::Cgroup;
use panoptes_sync::{self as sync, mpsc};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The longest request line resd reads.
const MAX_LINE: u64 = 64 << 10;
/// How long a connection waits for the owner's reply: a thaw takes at most
/// 5 s, behind a freeze of 5 s and one reclaim chunk.
const REPLY_LIMIT: Duration = Duration::from_secs(30);
/// The most connections served at once. Each is one thread and two
/// descriptors. A desk keeps one, and a status reader uses one for a moment.
/// 700 desks have run on one host, and about 3,000 kiosks fit it with lean
/// flags (docs/design/kernel-and-inference.md). The default vm.max_map_count
/// ended resd at about 16,000 threads, four times this.
const MAX_CONNS: u64 = 4096;
/// Descriptors left for all but connections: stdio, the listener, the signal
/// pipe, and files open for a moment.
const SPARE_FDS: u64 = 64;
/// What a connection past the cap gets before resd closes it.
const TOO_MANY: &[u8] = b"{\"ok\":false,\"error\":\"too many connections\"}\n";

/// Where the agents live, for peer checks and status.
#[derive(Debug, Clone)]
pub struct Root {
    pub dir: PathBuf,
    /// `dir` relative to the cgroup mount, as /proc/PID/cgroup prints it.
    pub rel: String,
}

/// The connection cap under a descriptor limit of `fds`, `None` for no limit.
/// A connection holds two, the stream and its clone. The cap must come first:
/// past the limit a client gets a closed connection, not a refusal.
pub fn conn_cap(fds: Option<u64>) -> usize {
    let room = fds.map_or(MAX_CONNS, |n| n.saturating_sub(SPARE_FDS) / 2);
    room.clamp(1, MAX_CONNS) as usize
}

/// Accept connections until the process exits, running at most `cap` at once.
pub fn serve(listener: UnixListener, owner: mpsc::Sender<Msg>, root: Root, cap: usize) {
    accept(listener, cap, move |stream| {
        connection(stream, &owner, &root);
    });
}

/// The accept loop, apart from what a connection does.
fn accept<F>(listener: UnixListener, cap: usize, connection: F)
where
    F: Fn(UnixStream) + Clone + Send + 'static,
{
    let mut slots = Slots::new(cap);
    for (n, stream) in listener.incoming().enumerate() {
        let Ok(stream) = stream else { continue };
        let Some(slot) = slots.take() else {
            refuse(stream);
            continue;
        };
        let connection = connection.clone();
        sync::spawn(&format!("conn-{n}"), move || {
            let _slot = slot;
            connection(stream);
        });
    }
}

/// The running connections, counted by the accept thread alone. Each holds a
/// [`Slot`], which reports here when the connection ends.
struct Slots {
    cap: usize,
    live: usize,
    freed: mpsc::Sender<()>,
    ended: mpsc::Receiver<()>,
}

impl Slots {
    fn new(cap: usize) -> Slots {
        let (freed, ended) = mpsc::channel();
        Slots {
            cap,
            live: 0,
            freed,
            ended,
        }
    }

    /// A slot, unless `cap` connections run. A connection ending right now
    /// may count only at the next call: that refuses early, and never runs
    /// too many.
    fn take(&mut self) -> Option<Slot> {
        while self.ended.try_recv().is_ok() {
            self.live -= 1;
        }
        if self.live >= self.cap {
            return None;
        }
        self.live += 1;
        Some(Slot(self.freed.clone()))
    }
}

/// One running connection. Dropping it frees the slot, in a panic too.
struct Slot(mpsc::Sender<()>);

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Write the refusal and close, without waiting for the peer: the accept
/// thread must not block on a client that does not read. The line fits the
/// empty send buffer of a new socket anyway.
fn refuse(mut stream: UnixStream) {
    let _ = stream.set_nonblocking(true);
    let _ = stream.write_all(TOO_MANY);
}

fn connection(stream: UnixStream, owner: &mpsc::Sender<Msg>, root: &Root) {
    let cred = rustix::net::sockopt::socket_peercred(&stream).ok();
    let is_root = cred.is_some_and(|c| c.uid.is_root());
    let peer = cred.and_then(|c| peer_agent(&root.rel, c.pid.as_raw_nonzero().get()));
    let Ok(mut write) = stream.try_clone() else {
        return;
    };
    let mut read = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match (&mut read).take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let too_long = !line.ends_with('\n') && line.len() as u64 == MAX_LINE;
        let out = if too_long {
            json!({ "ok": false, "error": "request line too long" })
        } else {
            answer(owner, line.trim(), peer.as_deref(), is_root, &root.dir)
        };
        if write.write_all(format!("{out}\n").as_bytes()).is_err() || too_long {
            return;
        }
    }
}

fn answer(
    owner: &mpsc::Sender<Msg>,
    line: &str,
    peer: Option<&str>,
    is_root: bool,
    root: &Path,
) -> Value {
    let Ok(req) = serde_json::from_str::<Value>(line) else {
        return json!({ "ok": false, "error": "bad json" });
    };
    let op = req["op"].as_str().unwrap_or("");
    if op == "status" {
        let since = req["since"].as_u64().unwrap_or(u64::MAX);
        let (reply, rx) = mpsc::channel();
        return match ask(owner, Msg::Status { since, reply }, &rx) {
            Ok(s) => status(&s, root),
            Err(e) => json!({ "ok": false, "error": e }),
        };
    }
    let agent = match (req["agent"].as_str(), peer, is_root) {
        (Some(n), _, true) if is_name(n) => n.to_owned(),
        (Some(_), _, true) => return json!({ "ok": false, "error": "bad agent name" }),
        (Some(_), _, false) => return json!({ "ok": false, "error": "only root names an agent" }),
        (None, Some(p), _) => p.to_owned(),
        (None, None, _) => return json!({ "ok": false, "error": "not a desk of this root" }),
    };
    let op = match op {
        "acquire" => Op::Acquire,
        "release" => Op::Release,
        _ => return json!({ "ok": false, "error": format!("unknown op {op}") }),
    };
    let (reply, rx) = mpsc::channel();
    match ask(owner, Msg::Request { agent, op, reply }, &rx) {
        Ok(v) => v,
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

fn ask<T>(owner: &mpsc::Sender<Msg>, msg: Msg, rx: &mpsc::Receiver<T>) -> Result<T, String> {
    owner
        .send(msg)
        .map_err(|_| "resd's owner thread is gone".to_owned())?;
    sync::recv_reply(rx, REPLY_LIMIT, "resd's owner thread")
}

/// One directory name under the root.
fn is_name(n: &str) -> bool {
    !n.is_empty() && n != "." && n != ".." && !n.contains('/')
}

/// The agent a connection speaks for: its process's cgroup must be
/// `<root>/<agent>/desk`.
fn peer_agent(root_rel: &str, pid: i32) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    let rest = path.strip_prefix(root_rel)?.strip_prefix('/')?;
    let agent = rest.strip_suffix("/desk")?;
    is_name(agent).then(|| agent.to_owned())
}

/// The status reply: the owner's snapshot, with each kiosk's memory and
/// CPU read here, off the owner thread.
fn status(s: &Snapshot, root: &Path) -> Value {
    let rows: Vec<Value> = s
        .agents
        .iter()
        .map(|row| {
            let k = Cgroup::open(root.join(&row.name).join("kiosk")).ok();
            let mem = k.as_ref().and_then(|k| k.memory_current().ok());
            let stat = k.as_ref().and_then(|k| k.memory_stat().ok());
            let cpu = k.as_ref().and_then(|k| k.cpu_usage_us().ok());
            let park = row.park.map(
                |p| json!({ "before": p.before, "after": p.after, "ms": p.ms, "full": p.full }),
            );
            json!({
                "agent": row.name,
                "state": row.state.name(),
                "for_ms": row.for_ms,
                "memory_current": mem,
                "zswap": stat.as_ref().and_then(|s| s.get("zswap")),
                "zswapped": stat.as_ref().and_then(|s| s.get("zswapped")),
                "cpu_usage_us": cpu,
                "last_park": park,
            })
        })
        .collect();
    json!({
        "ok": true,
        "now_ms": s.now_ms,
        "host": { "mem_available": cg::meminfo("MemAvailable"),
                  "swap_free": cg::meminfo("SwapFree"),
                  "zswap_pool": cg::zswap_pool() },
        "agents": rows,
        "seq": s.seq,
        "events": s.events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_single_directory_names_name_an_agent() {
        for n in ["a", "agent-7", "x.y"] {
            assert!(is_name(n), "{n}");
        }
        for n in ["", ".", "..", "a/b", "../etc"] {
            assert!(!is_name(n), "{n}");
        }
    }

    #[test]
    fn the_cap_fits_the_descriptor_limit() {
        assert_eq!(conn_cap(None), 4096);
        assert_eq!(conn_cap(Some(65536)), 4096);
        assert_eq!(conn_cap(Some(1024)), 480);
        assert_eq!(conn_cap(Some(10)), 1);
    }
}

/// The cap against real sockets. Nothing here sleeps: a client awaits a reply,
/// and a slot that comes back shows only at a later accept, so
/// [`wait_for_slot`] connects until one is served.
#[cfg(all(test, not(panoptes_loom)))]
mod socket_tests {
    use super::*;
    use std::time::Instant;

    /// How long a failing test waits for a reply or a slot.
    const LIMIT: Duration = Duration::from_secs(10);

    fn listen(name: &str) -> (PathBuf, UnixListener) {
        let dir = std::env::temp_dir().join(format!("panoptes-resd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("resd.sock")).unwrap();
        (dir, listener)
    }

    fn connect(dir: &Path) -> BufReader<UnixStream> {
        let s = UnixStream::connect(dir.join("resd.sock")).unwrap();
        s.set_read_timeout(Some(LIMIT)).unwrap();
        BufReader::new(s)
    }

    /// The next line from `c`.
    fn reply(c: &mut BufReader<UnixStream>) -> String {
        let mut line = String::new();
        c.read_line(&mut line).unwrap();
        line
    }

    /// Send a line and read the reply. A refused connection may be closed
    /// before this writes, so a failed write is no error here.
    fn ask_line(c: &mut BufReader<UnixStream>, line: &str) -> String {
        let _ = writeln!(c.get_mut(), "{line}");
        reply(c)
    }

    fn assert_refused(dir: &Path) {
        assert_eq!(reply(&mut connect(dir)).as_bytes(), TOO_MANY);
    }

    /// Connect until a connection is served, and return it.
    fn wait_for_slot(dir: &Path) -> BufReader<UnixStream> {
        let deadline = Instant::now() + LIMIT;
        loop {
            let mut c = connect(dir);
            if ask_line(&mut c, "x").as_bytes() != TOO_MANY {
                return c;
            }
            assert!(Instant::now() < deadline, "no slot came back in {LIMIT:?}");
        }
    }

    /// `serve` with no owner behind it. Bad JSON gets a reply without one.
    fn start(listener: UnixListener, cap: usize) -> mpsc::Receiver<Msg> {
        let (owner, rx) = mpsc::channel();
        let root = Root {
            dir: "/nonexistent".into(),
            rel: "/nonexistent".into(),
        };
        sync::spawn("accept", move || serve(listener, owner, root, cap));
        rx
    }

    const BAD_JSON: &str = "{\"error\":\"bad json\",\"ok\":false}\n";

    #[test]
    fn past_the_cap_a_client_gets_the_refusal_and_a_close() {
        let (dir, listener) = listen("cap");
        let _owner = start(listener, 2);
        let mut a = connect(&dir);
        assert_eq!(ask_line(&mut a, "x"), BAD_JSON);
        let mut b = connect(&dir);
        assert_eq!(ask_line(&mut b, "x"), BAD_JSON);

        let mut c = connect(&dir);
        let line = reply(&mut c);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ok"], false, "{line}");
        assert_eq!(v["error"], "too many connections", "{line}");
        assert_eq!(reply(&mut c), "", "still open after the refusal");

        // One that never reads holds nothing up: accepts run in order.
        let _silent = connect(&dir);
        assert_refused(&dir);
        // The refusals touched no running connection.
        assert_eq!(ask_line(&mut a, "x"), BAD_JSON);
        assert_eq!(ask_line(&mut b, "x"), BAD_JSON);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_client_that_wrote_first_still_reads_the_refusal() {
        let (dir, listener) = listen("wrote");
        let mut c = connect(&dir);
        writeln!(c.get_mut(), "{{\"op\":\"acquire\"}}").unwrap();
        // Accepting only now, resd closes with the request unread.
        sync::spawn("accept", move || accept(listener, 0, drop));
        assert_eq!(reply(&mut c).as_bytes(), TOO_MANY);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn closing_a_connection_frees_its_slot() {
        let (dir, listener) = listen("close");
        let _owner = start(listener, 1);
        let mut a = connect(&dir);
        assert_eq!(ask_line(&mut a, "x"), BAD_JSON);
        assert_refused(&dir);
        drop(a);
        let mut b = wait_for_slot(&dir);
        assert_eq!(ask_line(&mut b, "x"), BAD_JSON);
        assert_refused(&dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Answers each line, and panics on "panic".
    fn echo_or_panic(stream: UnixStream) {
        let mut write = stream.try_clone().unwrap();
        for line in BufReader::new(stream).lines() {
            assert_ne!(line.unwrap(), "panic", "a planted panic");
            write.write_all(b"served\n").unwrap();
        }
    }

    #[test]
    fn a_connection_that_panics_frees_its_slot() {
        let (dir, listener) = listen("panic");
        sync::spawn("accept", move || accept(listener, 1, echo_or_panic));
        let mut a = connect(&dir);
        assert_eq!(ask_line(&mut a, "x"), "served\n");
        assert_refused(&dir);
        // The panic unwinds the thread, which closes its stream.
        assert_eq!(ask_line(&mut a, "panic"), "");
        let mut b = wait_for_slot(&dir);
        assert_eq!(ask_line(&mut b, "x"), "served\n");
        assert_refused(&dir);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(all(test, panoptes_loom))]
mod loom_models {
    use super::*;
    use panoptes_sync::thread;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

    /// A connection ends on its own thread. A `take` meanwhile may or may not
    /// see it, and the one after that thread ended must: the slot comes back
    /// once, and the cap holds in every order.
    #[test]
    fn a_slot_comes_back_once_and_the_cap_holds() {
        static SAW_IT: AtomicBool = AtomicBool::new(false);
        static MISSED_IT: AtomicBool = AtomicBool::new(false);
        panoptes_sync::model(|| {
            let mut slots = Slots::new(1);
            let slot = slots.take().expect("one is free");
            assert!(slots.take().is_none(), "the cap let a second one in");
            let ending = thread::spawn(move || drop(slot));
            let early = slots.take();
            ending.join().unwrap();
            let late = slots.take();
            assert!(early.is_some() != late.is_some(), "one slot, two or none");
            let seen = if early.is_some() { &SAW_IT } else { &MISSED_IT };
            seen.store(true, Relaxed);
        });
        assert!(
            SAW_IT.load(Relaxed) && MISSED_IT.load(Relaxed),
            "loom tried one order"
        );
    }
}
