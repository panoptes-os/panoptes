//! panoptes-resd: the residency manager. It freezes each agent's kiosk while
//! its agent waits, parks long-waiting kiosks into zswap, and thaws them
//! when their desk asks. The protocol is in [`conn`]; the cgroup layout in
//! [`cg`].
//!
//! The layers and their threads (docs/design/verification.md):
//!
//! ```text
//! policy    policy.rs   residency decisions               exhaustive test
//! threads   owner.rs    the owner: policy, names, log     loom
//!           worker.rs   kiosk-N: one agent's cgroup work  loom
//! I/O       cg.rs       cgroupfs, procfs, the state file
//!           conn.rs     accept, conn-N: the socket
//! main.rs   tick: a Tick each period; signals: Stop on SIGTERM or SIGINT
//! ```

mod cg;
mod conn;
mod owner;
mod policy;
mod worker;

use owner::Msg;
use panoptes_sync::{self as sync, mpsc};
use policy::{Config, Policy};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "usage: panoptes-resd [--root DIR] [--socket PATH] [--state FILE] \
[--park-after-ms N] [--mem-low-mib N] [--tick-ms N] [--park-jobs N]

  --root DIR         cgroup holding one directory per agent
                     (default /sys/fs/cgroup/panoptes.slice)
  --socket PATH      control socket (default /run/panoptes/resd.sock)
  --state FILE       residency state across restarts (default /run/panoptes/resd.state)
  --park-after-ms N  park an agent after it waits this long (default 30000)
  --mem-low-mib N    park waiting agents early below this MemAvailable (default 4096)
  --tick-ms N        discovery and policy period (default 1000)
  --park-jobs N      parks running at once (default 4)";

#[derive(Debug, Clone)]
struct Args {
    root: PathBuf,
    socket: PathBuf,
    state: PathBuf,
    tick: Duration,
    cfg: Config,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        root: "/sys/fs/cgroup/panoptes.slice".into(),
        socket: "/run/panoptes/resd.sock".into(),
        state: "/run/panoptes/resd.state".into(),
        tick: Duration::from_millis(1000),
        cfg: Config::default(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{k} needs a value"));
        let num = |v: String| v.parse::<u64>().map_err(|_| format!("{k}: not a number"));
        match k.as_str() {
            "--root" => a.root = val()?.into(),
            "--socket" => a.socket = val()?.into(),
            "--state" => a.state = val()?.into(),
            "--park-after-ms" => a.cfg.park_after = num(val()?)?,
            "--mem-low-mib" => a.cfg.mem_low = num(val()?)? << 20,
            "--tick-ms" => a.tick = Duration::from_millis(num(val()?)?.max(1)),
            "--park-jobs" => {
                a.cfg.park_jobs =
                    u32::try_from(num(val()?)?.max(1)).map_err(|_| format!("{k}: too large"))?;
            }
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(a)
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let root = conn::Root {
        rel: args
            .root
            .strip_prefix("/sys/fs/cgroup")
            .map(|p| format!("/{}", p.display()))
            .unwrap_or_default(),
        dir: args.root.clone(),
    };
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])
    .expect("signal handlers");

    // The owner writes the state file as soon as it has recovered.
    for file in [&args.socket, &args.state] {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let (tx, rx) = mpsc::channel();
    let saved = cg::load_state(&args.state);
    let host = cg::Host::new(args.root.clone(), args.state.clone());
    let me = tx.clone();
    let owner = sync::spawn("owner", move || {
        drop(owner::run(host, Policy::new(args.cfg), saved, rx, me));
    });

    let _ = std::fs::remove_file(&args.socket);
    let listener = match UnixListener::bind(&args.socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("panoptes-resd: {}: {e}", args.socket.display());
            std::process::exit(1);
        }
    };
    // Desks run as other users; the peer check, not file modes, decides
    // what a connection may do.
    let _ = std::fs::set_permissions(
        &args.socket,
        std::os::unix::fs::PermissionsExt::from_mode(0o666),
    );
    let cap = conn::conn_cap(rustix::process::getrlimit(rustix::process::Resource::Nofile).current);
    eprintln!(
        "panoptes-resd: serving on {}, at most {cap} connections",
        args.socket.display()
    );

    let ticks = tx.clone();
    sync::spawn("tick", move || {
        loop {
            std::thread::sleep(args.tick);
            if ticks.send(Msg::Tick).is_err() {
                return;
            }
        }
    });
    let stop = tx.clone();
    sync::spawn("signals", move || {
        if signals.forever().next().is_some() {
            let _ = stop.send(Msg::Stop);
        }
    });
    sync::spawn("accept", move || conn::serve(listener, tx, root, cap));

    let _ = owner.join();
    // Exiting leaves every kiosk as it is.
    let _ = std::fs::remove_file(&args.socket);
    std::process::exit(0);
}
