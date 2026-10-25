//! panoptes-resd: the residency manager. It freezes each agent's kiosk while
//! its agent waits, parks long-waiting kiosks into zswap, and thaws them
//! when their desk asks.
//!
//! ```text
//! <root>/<agent>/desk    the agent's panoptes-desk; never frozen
//! <root>/<agent>/kiosk   Cage and Chromium; frozen, reclaimed, weighted here
//! ```
//!
//! Protocol: one JSON object per line on a unix socket.
//!
//! ```text
//! > {"op":"acquire"}               from a desk: thaw or resume its kiosk
//! < {"ok":true,"state":"acting","from":"parked","ms":0.21}
//! > {"op":"release"}               from a desk: freeze its kiosk
//! > {"op":"status"}                anyone: every agent and the host
//! > {"op":"status","since":N}      also the transitions after sequence N
//! > {"op":"acquire","agent":"x"}   root only: name the agent explicitly
//! ```
//!
//! A desk's agent is the cgroup its connecting process runs in, read from
//! the peer's credentials, so one agent cannot freeze another.

mod policy;

use panoptes_cgroup::{Cgroup, Reclaim};
use policy::{Action, Config, Event, Policy, Refusal, State};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const USAGE: &str = "usage: panoptes-resd [--root DIR] [--socket PATH] [--state FILE] \
[--park-after-ms N] [--mem-low-mib N] [--mem-critical-mib N] [--swap-low-mib N]
[--evict-batch N] [--tick-ms N] [--park-jobs N]

  --root DIR         cgroup holding one directory per agent
                     (default /sys/fs/cgroup/panoptes.slice)
  --socket PATH      control socket (default /run/panoptes/resd.sock)
  --state FILE       residency state across restarts (default /run/panoptes/resd.state)
  --park-after-ms N  park an agent after it waits this long (default 30000)
  --mem-low-mib N    park waiting agents early below this MemAvailable (default 4096)
  --mem-critical-mib N  evict the longest-parked agents below this MemAvailable
                     (default 2048)
  --swap-low-mib N   evict them below this SwapFree too (default 4096)
  --evict-batch N    agents evicted per tick (default 4)
  --tick-ms N        discovery and policy period (default 1000)
  --park-jobs N      parks running at once (default 4)";

const FREEZE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct Args {
    root: PathBuf,
    socket: PathBuf,
    state: PathBuf,
    tick: Duration,
    park_jobs: usize,
    cfg: Config,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        root: "/sys/fs/cgroup/panoptes.slice".into(),
        socket: "/run/panoptes/resd.sock".into(),
        state: "/run/panoptes/resd.state".into(),
        tick: Duration::from_millis(1000),
        park_jobs: 4,
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
            "--mem-critical-mib" => a.cfg.mem_critical = num(val()?)? << 20,
            "--swap-low-mib" => a.cfg.swap_low = num(val()?)? << 20,
            "--evict-batch" => a.cfg.evict_batch = num(val()?)?.max(1) as usize,
            "--tick-ms" => a.tick = Duration::from_millis(num(val()?)?),
            "--park-jobs" => a.park_jobs = num(val()?)?.max(1) as usize,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(a)
}

/// The result of an agent's most recent park.
#[derive(Debug, Clone, Copy)]
struct ParkStat {
    before: u64,
    after: u64,
    ms: f64,
    full: bool,
}

#[derive(Debug)]
struct Shared {
    args: Args,
    origin: Instant,
    policy: Mutex<Policy>,
    /// Serializes cgroup writes per agent, so a park and a thaw never
    /// interleave.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    parks: Mutex<HashMap<String, ParkStat>>,
    park_slots: tokio::sync::Semaphore,
    /// Path of `root` relative to the cgroup mount, as /proc/PID/cgroup
    /// prints it.
    root_rel: String,
    /// Recent transitions for `panoptes top`: (next sequence, ring).
    log: Mutex<(u64, VecDeque<Value>)>,
}

/// Transitions kept for status readers.
const LOG_LEN: usize = 1024;

impl Shared {
    fn now(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn kiosk(&self, agent: &str) -> panoptes_cgroup::Result<Cgroup> {
        Cgroup::open(self.args.root.join(agent).join("kiosk"))
    }

    fn lock_for(&self, agent: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .expect("locks")
            .entry(agent.to_owned())
            .or_default()
            .clone()
    }

    /// Run one event through the policy and persist the result.
    fn handle(&self, ev: Event) -> Result<Vec<Action>, Refusal> {
        let now = self.now();
        let mut p = self.policy.lock().expect("policy");
        let out = p.handle(ev, now);
        save_state(&self.args.state, p.agents());
        out
    }

    /// Record one transition, with the time its cgroup writes took.
    fn note(&self, agent: &str, from: &str, to: &str, took_ms: f64, extra: Value) {
        let t = self.now();
        let mut log = self.log.lock().expect("log");
        log.0 += 1;
        let mut e = json!({ "seq": log.0, "t_ms": t, "agent": agent, "from": from, "to": to,
                            "ms": took_ms });
        if let (Some(e), Value::Object(x)) = (e.as_object_mut(), extra) {
            e.extend(x);
        }
        log.1.push_back(e);
        if log.1.len() > LOG_LEN {
            log.1.pop_front();
        }
    }

    fn state_of(&self, agent: &str) -> Option<State> {
        self.policy
            .lock()
            .expect("policy")
            .agents()
            .get(agent)
            .map(|a| a.state)
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 100_000.0).round() / 100.0
}

fn save_state(path: &Path, agents: &BTreeMap<String, policy::Agent>) {
    let map: serde_json::Map<String, Value> = agents
        .iter()
        .map(|(n, a)| (n.clone(), Value::from(a.state.name())))
        .collect();
    let tmp = path.with_extension("tmp");
    let body = Value::Object(map).to_string();
    if std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, path)).is_err() {
        eprintln!("panoptes-resd: cannot write {}", path.display());
    }
}

fn load_state(path: &Path) -> HashMap<String, State> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    map.into_iter()
        .filter_map(|(k, v)| Some((k, State::from_name(v.as_str()?)?)))
        .collect()
}

/// Apply actions for one agent in order, holding its lock. Returns the time
/// the cgroup writes took. On failure the policy is corrected from what the
/// kernel reports.
async fn apply(sh: &Arc<Shared>, agent: &str, actions: Vec<Action>) -> Result<f64, String> {
    if actions.is_empty() {
        return Ok(0.0);
    }
    let lock = sh.lock_for(agent);
    let _held = lock.lock().await;
    let sh2 = sh.clone();
    let name = agent.to_owned();
    let r = tokio::task::spawn_blocking(move || -> Result<f64, String> {
        let start = Instant::now();
        let kiosk = sh2.kiosk(&name).map_err(|e| e.to_string())?;
        for act in actions {
            match act {
                Action::Thaw(_) => {
                    kiosk.thaw(FREEZE_TIMEOUT).map_err(|e| format!("thaw: {e}"))?;
                }
                Action::Freeze(_) => {
                    kiosk.freeze(FREEZE_TIMEOUT).map_err(|e| format!("freeze: {e}"))?;
                }
                Action::Weight(_, w) => {
                    kiosk.set_cpu_weight(w).map_err(|e| format!("cpu.weight: {e}"))?;
                }
                Action::Park(_) => park(&sh2, &name, &kiosk)?,
                Action::Evict(_) => {
                    kiosk.kill_all().map_err(|e| format!("cgroup.kill: {e}"))?;
                    eprintln!("panoptes-resd: evicted {name}");
                    sh2.note(&name, "parked", "evicted", 0.0, json!({}));
                }
            }
        }
        Ok(ms(start.elapsed()))
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Err(e) = &r {
        eprintln!("panoptes-resd: {agent}: {e}");
        if let Ok(ev) = sh.kiosk(agent).and_then(|k| k.events()) {
            let now = sh.now();
            sh.policy.lock().expect("policy").observed(agent, ev.frozen, now);
        }
    }
    r
}

/// Reclaim a frozen kiosk into zswap with writeback off. A kiosk that was
/// thawed after the policy chose it is left alone.
fn park(sh: &Shared, agent: &str, kiosk: &Cgroup) -> Result<(), String> {
    if !kiosk.events().map_err(|e| e.to_string())?.frozen {
        return Ok(());
    }
    kiosk
        .set_zswap_writeback(false)
        .map_err(|e| format!("memory.zswap.writeback: {e}"))?;
    let before = kiosk.memory_current().map_err(|e| e.to_string())?;
    let start = Instant::now();
    let full = match kiosk.reclaim(before).map_err(|e| format!("memory.reclaim: {e}"))? {
        Reclaim::Full => true,
        Reclaim::Partial => false,
    };
    let after = kiosk.memory_current().map_err(|e| e.to_string())?;
    let stat = ParkStat {
        before,
        after,
        ms: ms(start.elapsed()),
        full,
    };
    eprintln!(
        "panoptes-resd: parked {agent}: {} -> {} MiB in {} ms",
        before >> 20,
        after >> 20,
        stat.ms
    );
    sh.parks.lock().expect("parks").insert(agent.to_owned(), stat);
    sh.note(agent, "waiting", "parked", stat.ms, json!({ "before": before, "after": after }));
    Ok(())
}

/// A /proc/meminfo field in bytes; unreadable counts as plenty.
fn meminfo(key: &str) -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(u64::MAX, |kb| kb << 10)
}

fn mem_available() -> u64 {
    meminfo("MemAvailable")
}

fn swap_free() -> u64 {
    meminfo("SwapFree")
}

fn zswap_pool() -> Option<u64> {
    std::fs::read_to_string("/sys/kernel/debug/zswap/pool_total_size")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// One pass of discovery: tell the policy about new, revived, and vanished
/// kiosks.
fn discover(sh: &Shared, saved: &HashMap<String, State>) {
    let mut seen = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&sh.args.root) {
        for ent in dir.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            let Ok(kiosk) = Cgroup::open(ent.path().join("kiosk")) else {
                continue;
            };
            let Ok(ev) = kiosk.events() else { continue };
            seen.push(name.clone());
            let known = sh.state_of(&name);
            let event = match known {
                Some(s) if !ev.populated && s != State::Evicted => Event::Lost { agent: name },
                _ => Event::Found {
                    agent: name,
                    frozen: ev.frozen,
                    populated: ev.populated,
                    saved: saved.get(ent.file_name().to_string_lossy().as_ref()).copied(),
                },
            };
            let _ = sh.handle(event);
        }
    }
    let mut p = sh.policy.lock().expect("policy");
    let gone: Vec<String> = p
        .agents()
        .keys()
        .filter(|n| !seen.contains(n))
        .cloned()
        .collect();
    for n in gone {
        p.forget(&n);
        sh.parks.lock().expect("parks").remove(&n);
    }
}

async fn ticker(sh: Arc<Shared>, saved: HashMap<String, State>) {
    let mut iv = tokio::time::interval(sh.args.tick);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        iv.tick().await;
        discover(&sh, &saved);
        let Ok(actions) = sh.handle(Event::Tick {
            mem_available: mem_available(),
            swap_free: swap_free(),
        }) else {
            continue;
        };
        for act in actions {
            match act {
                Action::Park(agent) => spawn_park(&sh, agent),
                Action::Evict(agent) => {
                    let sh = sh.clone();
                    tokio::spawn(async move {
                        let _ = apply(&sh, &agent, vec![Action::Evict(agent.clone())]).await;
                    });
                }
                _ => {}
            }
        }
    }
}

/// Reclaim a parked agent's kiosk within the park-job limit.
fn spawn_park(sh: &Arc<Shared>, agent: String) {
    let sh = sh.clone();
    tokio::spawn(async move {
        let Ok(_slot) = sh.park_slots.acquire().await else { return };
        // Thawed by an acquire while waiting for a slot: skip.
        if sh.state_of(&agent) != Some(State::Parked) {
            return;
        }
        let _ = apply(&sh, &agent, vec![Action::Park(agent.clone())]).await;
    });
}

/// The agent a connection speaks for: its process's cgroup must be
/// `<root>/<agent>/desk`.
fn peer_agent(sh: &Shared, pid: Option<i32>) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{}/cgroup", pid?)).ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    let rest = path.strip_prefix(&sh.root_rel)?.strip_prefix('/')?;
    let agent = rest.strip_suffix("/desk")?;
    (!agent.is_empty() && !agent.contains('/')).then(|| agent.to_owned())
}

async fn status(sh: &Arc<Shared>, since: u64) -> Value {
    let now = sh.now();
    let agents: Vec<(String, policy::Agent)> = sh
        .policy
        .lock()
        .expect("policy")
        .agents()
        .iter()
        .map(|(n, a)| (n.clone(), *a))
        .collect();
    let parks = sh.parks.lock().expect("parks").clone();
    let sh2 = sh.clone();
    let rows = tokio::task::spawn_blocking(move || {
        agents
            .into_iter()
            .map(|(name, a)| {
                let k = sh2.kiosk(&name).ok();
                let mem = k.as_ref().and_then(|k| k.memory_current().ok());
                let stat = k.as_ref().and_then(|k| k.memory_stat().ok());
                let cpu = k.as_ref().and_then(|k| k.cpu_usage_us().ok());
                let park = parks.get(&name).map(|p| {
                    json!({ "before": p.before, "after": p.after, "ms": p.ms, "full": p.full })
                });
                json!({
                    "agent": name,
                    "state": a.state.name(),
                    "for_ms": now.saturating_sub(a.since),
                    "memory_current": mem,
                    "zswap": stat.as_ref().and_then(|s| s.get("zswap")),
                    "zswapped": stat.as_ref().and_then(|s| s.get("zswapped")),
                    "cpu_usage_us": cpu,
                    "last_park": park,
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let (seq, events) = {
        let log = sh.log.lock().expect("log");
        let ev: Vec<Value> = log
            .1
            .iter()
            .filter(|e| e["seq"].as_u64().unwrap_or(0) > since)
            .cloned()
            .collect();
        (log.0, ev)
    };
    json!({
        "ok": true,
        "now_ms": now,
        "host": { "mem_available": mem_available(), "swap_free": swap_free(),
                    "zswap_pool": zswap_pool() },
        "agents": rows,
        "seq": seq,
        "events": events,
    })
}

async fn request(sh: &Arc<Shared>, line: &str, peer: Option<&str>, root: bool) -> Value {
    let Ok(req) = serde_json::from_str::<Value>(line) else {
        return json!({ "ok": false, "error": "bad json" });
    };
    let op = req["op"].as_str().unwrap_or("");
    if op == "status" {
        return status(sh, req["since"].as_u64().unwrap_or(u64::MAX)).await;
    }
    let named = req["agent"].as_str();
    let agent = match (named, peer, root) {
        (Some(n), _, true) => n.to_owned(),
        (Some(_), _, false) => return json!({ "ok": false, "error": "only root names an agent" }),
        (None, Some(p), _) => p.to_owned(),
        (None, None, _) => return json!({ "ok": false, "error": "not a desk of this root" }),
    };
    let from = sh.state_of(&agent);
    let ev = match op {
        "acquire" => Event::Acquire { agent: agent.clone() },
        "release" => Event::Release { agent: agent.clone() },
        _ => return json!({ "ok": false, "error": format!("unknown op {op}") }),
    };
    // A desk that connects before discovery found its kiosk.
    if from.is_none() {
        discover(sh, &HashMap::new());
    }
    let actions = match sh.handle(ev) {
        Ok(a) => a,
        Err(Refusal::Unknown) => return json!({ "ok": false, "error": "unknown agent" }),
        Err(Refusal::Evicted) => return json!({ "ok": false, "error": "evicted" }),
    };
    match apply(sh, &agent, actions).await {
        Ok(t) => {
            let to = sh.state_of(&agent);
            if let (Some(f), Some(n)) = (from, to)
                && f != n
            {
                sh.note(&agent, f.name(), n.name(), t, json!({}));
            }
            json!({
            "ok": true,
            "state": sh.state_of(&agent).map(State::name),
            "from": from.map(State::name),
            "ms": t,
            })
        }
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

async fn serve(sh: Arc<Shared>, stream: UnixStream) {
    let cred = stream.peer_cred().ok();
    let root = cred.as_ref().is_some_and(|c| c.uid() == 0);
    let peer = peer_agent(&sh, cred.as_ref().and_then(|c| c.pid()));
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut out = request(&sh, &line, peer.as_deref(), root).await.to_string();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            break;
        }
    }
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let root_rel = args
        .root
        .strip_prefix("/sys/fs/cgroup")
        .map(|p| format!("/{}", p.display()))
        .unwrap_or_default();
    let saved = load_state(&args.state);
    let sh = Arc::new(Shared {
        origin: Instant::now(),
        policy: Mutex::new(Policy::new(args.cfg)),
        locks: Mutex::default(),
        parks: Mutex::default(),
        park_slots: tokio::sync::Semaphore::new(args.park_jobs),
        root_rel,
        args,
        log: Mutex::new((0, VecDeque::new())),
    });
    // Recovery: learn every kiosk before serving, without touching any.
    discover(&sh, &saved);
    let counts = sh.policy.lock().expect("policy").agents().values().fold(
        BTreeMap::<&str, usize>::new(),
        |mut m, a| {
            *m.entry(a.state.name()).or_default() += 1;
            m
        },
    );
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    eprintln!("panoptes-resd: recovered {counts:?} at {wall}");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = rt.block_on(async move {
        // The state file records Parked before the reclaim runs, so a crash
        // can leave a parked kiosk unreclaimed. Reclaim is idempotent and
        // park() never thaws, so redo it for every recovered parked agent.
        let parked: Vec<String> = sh
            .policy
            .lock()
            .expect("policy")
            .agents()
            .iter()
            .filter(|(_, a)| a.state == State::Parked)
            .map(|(n, _)| n.clone())
            .collect();
        for agent in parked {
            spawn_park(&sh, agent);
        }
        if let Some(dir) = sh.args.socket.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::remove_file(&sh.args.socket);
        let listener = match UnixListener::bind(&sh.args.socket) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("panoptes-resd: {}: {e}", sh.args.socket.display());
                return 1;
            }
        };
        // Desks run as other users; the peer check, not file modes,
        // decides what a connection may do.
        let _ = std::fs::set_permissions(
            &sh.args.socket,
            std::os::unix::fs::PermissionsExt::from_mode(0o666),
        );
        eprintln!("panoptes-resd: serving on {}", sh.args.socket.display());
        tokio::spawn(ticker(sh.clone(), saved));
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        loop {
            tokio::select! {
                acc = listener.accept() => {
                    if let Ok((s, _)) = acc {
                        tokio::spawn(serve(sh.clone(), s));
                    }
                }
                _ = term.recv() => break,
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        // Exiting leaves every kiosk as it is.
        let _ = std::fs::remove_file(&sh.args.socket);
        0
    });
    std::process::exit(code);
}
