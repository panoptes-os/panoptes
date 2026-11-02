//! resd's I/O on cgroupfs and procfs: discovery, the host's memory, the
//! state file, and each kiosk's freezes, thaws, weights and reclaims. It
//! decides nothing; the policy does.
//!
//! ```text
//! <root>/<agent>/desk    the agent's panoptes-desk; never frozen
//! <root>/<agent>/kiosk   Cage, Xwayland, and the agent's programs; frozen,
//!                        reclaimed, weighted here
//! ```

use crate::owner::{self, Msg, ParkStat, Seen, Work};
use crate::policy::{Effect, Id, State};
use crate::worker::{self, Chunk};
use panoptes_cgroup::{Cgroup, Reclaim};
use panoptes_sync::{self as sync, mpsc};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a freeze or thaw may take.
const FREEZE_TIMEOUT: Duration = Duration::from_secs(5);
/// How much one `memory.reclaim` write asks for. Work that arrives waits
/// for one chunk at most.
const CHUNK: u64 = 64 << 20;

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 100_000.0).round() / 100.0
}

#[derive(Debug)]
pub struct Host {
    root: PathBuf,
    state: PathBuf,
}

impl Host {
    pub fn new(root: PathBuf, state: PathBuf) -> Host {
        Host { root, state }
    }
}

fn seen(name: String, kiosk: &Path) -> Option<Seen> {
    let cg = Cgroup::open(kiosk).ok()?;
    let events = cg.events().ok()?;
    Some(Seen {
        name,
        frozen: cg.freeze_target().ok()?,
        populated: events.populated,
    })
}

impl owner::Host for Host {
    fn scan(&mut self) -> Vec<Seen> {
        let Ok(dir) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|ent| {
                let name = ent.file_name().into_string().ok()?;
                seen(name, &ent.path().join("kiosk"))
            })
            .collect()
    }

    fn look(&mut self, name: &str) -> Option<Seen> {
        seen(name.to_owned(), &self.root.join(name).join("kiosk"))
    }

    fn mem_available(&mut self) -> u64 {
        meminfo("MemAvailable")
    }

    fn save(&mut self, body: &str) {
        let tmp = self.state.with_extension("tmp");
        if std::fs::write(&tmp, body)
            .and_then(|()| std::fs::rename(&tmp, &self.state))
            .is_err()
        {
            eprintln!("panoptes-resd: cannot write {}", self.state.display());
        }
    }

    fn start(&mut self, id: Id, name: &str, done: mpsc::Sender<Msg>) -> mpsc::Sender<Work> {
        let (tx, rx) = mpsc::channel();
        let kiosk = Kiosk {
            path: self.root.join(name).join("kiosk"),
            park: None,
        };
        sync::spawn(&format!("kiosk-{id}"), move || {
            worker::run(id, kiosk, rx, done);
        });
        tx
    }
}

pub fn load_state(path: &Path) -> HashMap<String, State> {
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

/// A /proc/meminfo field in bytes; unreadable counts as plenty.
pub fn meminfo(key: &str) -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(u64::MAX, |kb| kb << 10)
}

pub fn zswap_pool() -> Option<u64> {
    std::fs::read_to_string("/sys/kernel/debug/zswap/pool_total_size")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// One kiosk cgroup, opened for each write, so a kiosk that vanished
/// fails the write instead of the worker.
#[derive(Debug)]
struct Kiosk {
    path: PathBuf,
    park: Option<Parking>,
}

#[derive(Debug)]
struct Parking {
    start: Instant,
    before: u64,
    asked: u64,
}

impl Kiosk {
    fn cg(&self) -> Result<Cgroup, String> {
        Cgroup::open(&self.path).map_err(|e| e.to_string())
    }
}

impl worker::Kiosk for Kiosk {
    fn apply(&mut self, effect: Effect) -> Result<f64, String> {
        let cg = self.cg()?;
        let start = Instant::now();
        match effect {
            Effect::Thaw => cg
                .thaw(FREEZE_TIMEOUT)
                .map(drop)
                .map_err(|e| format!("thaw: {e}"))?,
            Effect::Freeze => cg
                .freeze(FREEZE_TIMEOUT)
                .map(drop)
                .map_err(|e| format!("freeze: {e}"))?,
            Effect::Weight(w) => cg
                .set_cpu_weight(w)
                .map_err(|e| format!("cpu.weight: {e}"))?,
            Effect::Park => unreachable!("parks go through park_begin"),
        }
        Ok(ms(start.elapsed()))
    }

    fn frozen(&mut self) -> Option<bool> {
        self.cg().ok()?.freeze_target().ok()
    }

    fn park_begin(&mut self) -> Result<bool, String> {
        let cg = self.cg()?;
        // Thawed after the policy chose it: leave it alone.
        if !cg.freeze_target().map_err(|e| e.to_string())? {
            return Ok(false);
        }
        cg.set_zswap_writeback(false)
            .map_err(|e| format!("memory.zswap.writeback: {e}"))?;
        let before = cg.memory_current().map_err(|e| e.to_string())?;
        self.park = Some(Parking {
            start: Instant::now(),
            before,
            asked: 0,
        });
        Ok(true)
    }

    fn reclaim_chunk(&mut self) -> Result<Chunk, String> {
        let cg = self.cg()?;
        let p = self.park.as_mut().expect("park_begin ran");
        let n = p.before.saturating_sub(p.asked).min(CHUNK);
        if n == 0 {
            return Ok(Chunk::Full);
        }
        p.asked += n;
        match cg.reclaim(n).map_err(|e| format!("memory.reclaim: {e}"))? {
            Reclaim::Full if p.asked >= p.before => Ok(Chunk::Full),
            Reclaim::Full => Ok(Chunk::More),
            Reclaim::Partial => Ok(Chunk::Partial),
        }
    }

    fn park_end(&mut self, full: bool) -> Result<ParkStat, String> {
        let p = self.park.take().expect("park_begin ran");
        let after = self.cg()?.memory_current().map_err(|e| e.to_string())?;
        Ok(ParkStat {
            before: p.before,
            after,
            ms: ms(p.start.elapsed()),
            full,
        })
    }
}
