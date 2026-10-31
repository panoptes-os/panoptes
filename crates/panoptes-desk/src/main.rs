//! panoptes-desk: one agent's desktop and its control server.
//!
//! It moves itself into `<agent>/desk`, starts the Cage fork with no client
//! in `<agent>/kiosk`, drives it as a Wayland client, and serves the
//! agent's tools on a unix socket (the internal protocol and MCP) and, with
//! `--mcp-stdio`, MCP on stdin and stdout.

mod desk;
mod keymap;
mod kiosk;
mod protocol;
mod resd;
mod server;
mod wl;

use kiosk::{Config, Kiosk};
use panoptes_cgroup::Cgroup;
use std::path::PathBuf;
use std::sync::Arc;

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
    let kiosk = match kiosk::start(kiosk_cg, &args.config) {
        Ok(k) => Arc::new(k),
        Err(e) => {
            eprintln!("panoptes-desk: {e}");
            std::process::exit(1);
        }
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = rt.block_on(run(args, kiosk.clone()));
    kiosk.shutdown();
    std::process::exit(code);
}

async fn run(args: Args, kiosk: Arc<Kiosk>) -> i32 {
    let wl = match wl::Wl::connect(&kiosk.socket) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("panoptes-desk: {e}");
            return 1;
        }
    };
    let residency = args.resd.clone().map(|sock| {
        resd::Residency::new(
            sock,
            std::time::Duration::from_millis(args.grace_ms),
            kiosk.cgroup.clone(),
        )
    });
    let desk = match desk::Desk::new(wl, kiosk, residency).await {
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
    let socket = args
        .socket
        .unwrap_or_else(|| args.config.run_dir.join("desk.sock"));
    let listen = server::listen(desk.clone(), &socket);
    eprintln!("panoptes-desk: tools on {}", socket.display());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    let stdio = async {
        if args.mcp_stdio {
            server::serve_mcp(desk.clone(), tokio::io::stdin(), tokio::io::stdout()).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let code = tokio::select! {
        r = listen => {
            eprintln!("panoptes-desk: socket: {r:?}");
            1
        }
        _ = stdio => 0,
        _ = tokio::signal::ctrl_c() => 0,
        _ = term.recv() => 0,
    };
    let _ = std::fs::remove_file(&socket);
    code
}
