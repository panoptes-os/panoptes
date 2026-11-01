//! panoptes-desk: one agent's desktop and its control server.
//!
//! It moves itself into `<agent>/desk`, starts the Cage fork with no client
//! in `<agent>/kiosk`, drives it as a Wayland client, and serves the
//! agent's tools on a unix socket (the internal protocol and MCP) and, with
//! `--mcp-stdio`, MCP on stdin and stdout.
//!
//! It is split as docs/design/verification.md says:
//!
//! ```text
//! policy    lease, settle, windows, keymap     no I/O, clock or threads
//! threads   owner (the desk thread), handoff   checked with loom
//! I/O       wl, kiosk, desk's tools            checked by the desk VM test
//!           resd                               tested against a fake resd
//! edge      server: tokio, rmcp, the socket
//! ```
//!
//! Threads, each named:
//!
//! ```text
//! main      supervisor: waits for the edge to end, the compositor to go or
//!           the desk thread to panic
//! edge      tokio current-thread: socket, stdio, signals
//! desk      owner of the desktop; runs calls in order
//! wayland   reads and dispatches the compositor's events
//! reap-PID  waits for one launched program
//! ```

mod desk;
mod handoff;
mod keymap;
mod kiosk;
mod lease;
mod owner;
mod protocol;
mod resd;
mod server;
mod settle;
mod windows;
mod wl;

use kiosk::Config;
use panoptes_cgroup::Cgroup;
use panoptes_sync::{self as sync, mpsc};
use std::path::PathBuf;

const USAGE: &str = "usage: panoptes-desk --run-dir DIR [--cgroup DIR] [--socket PATH] \
[--size WxH] [--mcp-stdio] [--resd SOCK] [--grace-ms N] [--thumb-ms N]

  --run-dir DIR   the agent's directory: runtime, home, app logs, socket
  --cgroup DIR    the agent's cgroup (default: the one this process starts in);
                  the desk moves into DIR/desk and the kiosk starts in DIR/kiosk
  --socket PATH   tool socket (default: RUN_DIR/desk.sock)
  --size WxH      the screen (default 1024x768)
  --mcp-stdio     also serve MCP on stdin/stdout; exit when it closes
  --resd SOCK     acquire the kiosk from panoptes-resd before each call and
                  release it when calls stop, so it is frozen between steps
  --grace-ms N    quiet time before release (default 100)
  --thumb-ms N    after a tool call, at most every N ms, write a small capture
                  to RUN_DIR/thumb.b64 for panoptes top (default off)";

struct Args {
    cgroup: Option<PathBuf>,
    socket: Option<PathBuf>,
    mcp_stdio: bool,
    resd: Option<PathBuf>,
    grace_ms: u64,
    thumb_ms: Option<u64>,
    config: Config,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut run_dir = None;
    let mut cgroup = None;
    let mut socket = None;
    let mut size = (1024, 768);
    let mut mcp_stdio = false;
    let mut resd = None;
    let mut grace_ms = 100;
    let mut thumb_ms = None;
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--run-dir" => run_dir = Some(PathBuf::from(val()?)),
            "--cgroup" => cgroup = Some(PathBuf::from(val()?)),
            "--socket" => socket = Some(PathBuf::from(val()?)),
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--size is WxH")?;
                size = (
                    w.parse().map_err(|_| "--size width")?,
                    h.parse().map_err(|_| "--size height")?,
                );
            }
            "--mcp-stdio" => mcp_stdio = true,
            "--resd" => resd = Some(PathBuf::from(val()?)),
            "--grace-ms" => grace_ms = val()?.parse().map_err(|_| "--grace-ms")?,
            "--thumb-ms" => thumb_ms = Some(val()?.parse().map_err(|_| "--thumb-ms")?),
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    let run_dir = run_dir.ok_or_else(|| format!("--run-dir is required\n{USAGE}"))?;
    Ok(Args {
        cgroup,
        socket,
        mcp_stdio,
        resd,
        grace_ms,
        thumb_ms,
        config: Config {
            run_dir,
            width: size.0,
            height: size.1,
        },
    })
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // Cgroup moves happen before any thread starts.
    let agent = match &args.cgroup {
        Some(p) => Cgroup::open(p),
        None => Cgroup::current(),
    };
    let kiosk_cg = match agent.and_then(|a| kiosk::prepare_cgroups(&a)) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("panoptes-desk: cgroups: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&args.config.run_dir) {
        eprintln!("panoptes-desk: {}: {e}", args.config.run_dir.display());
        std::process::exit(1);
    }
    let (kiosk, cage) = match kiosk::start(kiosk_cg, &args.config) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("panoptes-desk: {e}");
            std::process::exit(1);
        }
    };
    let code = run(args, kiosk);
    cage.shutdown();
    std::process::exit(code);
}

/// Why the desk stops.
#[derive(Debug)]
enum Exit {
    /// The edge ended with this exit code.
    Edge(i32),
    /// The connection to the compositor broke.
    Lost(String),
    /// The desk thread panicked.
    Panicked,
}

/// Held by the desk thread: a panic there ends the process, which can do
/// nothing without that thread.
struct PanicGuard(mpsc::Sender<Exit>);

impl Drop for PanicGuard {
    fn drop(&mut self) {
        // A desk thread that returns does so because the edge ended, and the
        // edge sends its own exit code.
        if std::thread::panicking() {
            let _ = self.0.send(Exit::Panicked);
        }
    }
}

/// Start the threads, then wait for a reason to stop. Returns the exit
/// code.
fn run(args: Args, kiosk: kiosk::Kiosk) -> i32 {
    let (exit_tx, exit_rx) = mpsc::channel();
    let (shared, changes) = handoff::Shared::new();
    let exits = shared.notifier();
    let lost = {
        let exit_tx = exit_tx.clone();
        move |why| {
            let _ = exit_tx.send(Exit::Lost(why));
        }
    };
    let wl = match wl::Wl::connect(&kiosk.socket, shared, lost) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("panoptes-desk: {e}");
            return 1;
        }
    };
    let kiosk_cg = kiosk.cgroup.clone();
    let mut desk = match desk::Desk::new(wl, kiosk, changes, exits) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("panoptes-desk: {e}");
            return 1;
        }
    };
    if let Some(n) = args.thumb_ms {
        desk.set_thumb(
            args.config.run_dir.join("thumb.b64"),
            std::time::Duration::from_millis(n),
        );
    }
    let residency = args.resd.clone().map(|sock| {
        (
            resd::Resd::new(sock, kiosk_cg),
            lease::Lease::new(args.grace_ms),
        )
    });

    let (calls_tx, calls_rx) = mpsc::channel();
    let guard = PanicGuard(exit_tx.clone());
    sync::spawn("desk", move || {
        let _guard = guard;
        owner::run(calls_rx, &mut desk, residency);
    });
    let socket = args
        .socket
        .unwrap_or_else(|| args.config.run_dir.join("desk.sock"));
    let handle = server::Handle::new(calls_tx);
    let mcp_stdio = args.mcp_stdio;
    let edge_socket = socket.clone();
    sync::spawn("edge", move || {
        let code = server::run(handle, edge_socket, mcp_stdio);
        let _ = exit_tx.send(Exit::Edge(code));
    });

    let code = match sync::recv(&exit_rx) {
        Ok(Exit::Edge(code)) => code,
        Ok(Exit::Lost(why)) => {
            eprintln!("panoptes-desk: lost the compositor: {why}");
            1
        }
        Ok(Exit::Panicked) => {
            eprintln!("panoptes-desk: the desk thread panicked");
            1
        }
        Err(_) => 1,
    };
    let _ = std::fs::remove_file(&socket);
    code
}

#[cfg(all(test, not(panoptes_loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_thread_that_panics_tells_main() {
        let (tx, rx) = mpsc::channel();
        let guard = PanicGuard(tx);
        let thread = sync::spawn("panics", move || {
            let _guard = guard;
            panic!("planted");
        });
        assert!(thread.join().is_err());
        assert!(matches!(rx.recv(), Ok(Exit::Panicked)));
    }

    #[test]
    fn a_thread_that_returns_leaves_the_exit_to_the_edge() {
        let (tx, rx) = mpsc::channel();
        let guard = PanicGuard(tx);
        let thread = sync::spawn("returns", move || {
            let _guard = guard;
        });
        thread.join().unwrap();
        assert!(rx.recv().is_err());
    }
}
