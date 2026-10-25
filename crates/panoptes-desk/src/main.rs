//! panoptes-desk: one agent's control server.
//!
//! It moves itself into `<agent>/desk`, starts Cage and Chromium in
//! `<agent>/kiosk`, drives the browser over its debugging pipe, and serves
//! the agent's tools on a unix socket (the internal protocol and MCP) and,
//! with `--mcp-stdio`, MCP on stdin and stdout.

mod axtext;
mod desk;
mod keys;
mod kiosk;
mod resd;
mod server;

use kiosk::{CdpMode, Config};
use panoptes_cgroup::Cgroup;
use std::path::PathBuf;

const USAGE: &str = "usage: panoptes-desk --run-dir DIR [--cgroup DIR] [--socket PATH] \
[--size WxH] [--url URL] [--cdp-port N] [--mcp-stdio] [--resd SOCK] [--grace-ms N] \
[--thumb-ms N] [-- CHROMIUM_FLAGS...]

  --run-dir DIR   the agent's directory: runtime, profile, downloads, socket
  --cgroup DIR    the agent's cgroup (default: the one this process starts in);
                  the desk moves into DIR/desk and the kiosk starts in DIR/kiosk
  --socket PATH   tool socket (default: RUN_DIR/desk.sock)
  --size WxH      output mode (default 1280x720)
  --url URL       first page (default about:blank)
  --cdp-port N    reach DevTools over a loopback port instead of the pipe
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
    let mut size = (1280, 720);
    let mut url = "about:blank".to_owned();
    let mut cdp = CdpMode::Pipe;
    let mut mcp_stdio = false;
    let mut resd = None;
    let mut grace_ms = 100;
    let mut thumb_ms = None;
    let mut extra = Vec::new();
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--run-dir" => run_dir = Some(PathBuf::from(val()?)),
            "--cgroup" => cgroup = Some(PathBuf::from(val()?)),
            "--socket" => socket = Some(PathBuf::from(val()?)),
            "--url" => url = val()?,
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--size is WxH")?;
                size = (
                    w.parse().map_err(|_| "--size width")?,
                    h.parse().map_err(|_| "--size height")?,
                );
            }
            "--cdp-port" => cdp = CdpMode::Port(val()?.parse().map_err(|_| "--cdp-port")?),
            "--mcp-stdio" => mcp_stdio = true,
            "--resd" => resd = Some(PathBuf::from(val()?)),
            "--grace-ms" => grace_ms = val()?.parse().map_err(|_| "--grace-ms")?,
            "--thumb-ms" => thumb_ms = Some(val()?.parse().map_err(|_| "--thumb-ms")?),
            "--" => extra.extend(it.by_ref()),
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
            url,
            cdp,
            extra_flags: extra,
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
    // Cgroup moves happen before the runtime starts any threads.
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
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = rt.block_on(run(args, kiosk_cg));
    std::process::exit(code);
}

async fn run(args: Args, kiosk_cg: Cgroup) -> i32 {
    if let Err(e) = std::fs::create_dir_all(&args.config.run_dir) {
        eprintln!("panoptes-desk: {}: {e}", args.config.run_dir.display());
        return 1;
    }
    let kiosk = match kiosk::launch(kiosk_cg, &args.config).await {
        Ok(k) => k,
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
    let desk = match desk::Desk::new(kiosk.cdp.clone(), kiosk.downloads.clone(), residency).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("panoptes-desk: {e}");
            kiosk.shutdown().await;
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
    kiosk.shutdown().await;
    code
}
