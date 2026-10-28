//! Where `panoptes top` gets its numbers: a live host, or a recording.
//!
//! ```text
//! live:    resd status (socket) + /proc + nvidia-smi + the loop's run dir
//! record:  live, and every frame appended to a JSONL file
//! replay:  frames read back from that file at their recorded pace
//! ```

use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// One poll's worth of input.
#[derive(Debug, Default, Clone)]
pub struct Frame {
    /// Milliseconds since the source started.
    pub t_ms: u64,
    /// resd's `status` reply, or null when resd is unreachable.
    pub status: Value,
    pub mem_total: u64,
    pub mem_available: u64,
    /// Host CPU busy fraction since the previous frame.
    pub cpu: f64,
    /// GPU utilization percent and memory used in MiB, when known.
    pub gpu: Option<(f64, f64)>,
    /// New lines of the loop's trace.jsonl.
    pub trace: Vec<Value>,
    /// results.jsonl, once the loop writes it.
    pub results: Option<Vec<Value>>,
    /// Invoices assigned to the run.
    pub assigned: usize,
    /// Changed thumbnails: (agent index, base64 JPEG).
    pub thumbs: Vec<(usize, String)>,
}

impl Frame {
    fn to_json(&self) -> Value {
        json!({
            "t_ms": self.t_ms, "status": self.status, "mem_total": self.mem_total,
            "mem_available": self.mem_available, "cpu": self.cpu,
            "gpu": self.gpu.map(|(u, m)| json!([u, m])), "trace": self.trace,
            "results": self.results, "assigned": self.assigned,
            "thumbs": self.thumbs.iter().map(|(i, b)| json!([i, b])).collect::<Vec<_>>(),
        })
    }

    fn from_json(v: &Value) -> Frame {
        Frame {
            t_ms: v["t_ms"].as_u64().unwrap_or(0),
            status: v["status"].clone(),
            mem_total: v["mem_total"].as_u64().unwrap_or(0),
            mem_available: v["mem_available"].as_u64().unwrap_or(0),
            cpu: v["cpu"].as_f64().unwrap_or(0.0),
            gpu: v["gpu"]
                .as_array()
                .and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))),
            trace: v["trace"].as_array().cloned().unwrap_or_default(),
            results: v["results"].as_array().cloned(),
            assigned: v["assigned"].as_u64().unwrap_or(0) as usize,
            thumbs: v["thumbs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| Some((t[0].as_u64()? as usize, t[1].as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug)]
pub struct Live {
    resd: PathBuf,
    run: Option<PathBuf>,
    started: Instant,
    since: u64,
    cpu_prev: Option<(u64, u64)>,
    gpu: Option<(f64, f64)>,
    gpu_at: Option<Instant>,
    trace: Tail,
    results: Tail,
    /// Per agent: the thumbnail file's last modification time.
    thumb_seen: Vec<Option<SystemTime>>,
    record: Option<File>,
    /// Read every agent's thumbnail (recording), or only the wanted one.
    all_thumbs: bool,
}

impl Live {
    pub fn new(resd: PathBuf, run: Option<PathBuf>, record: Option<File>) -> Live {
        let all_thumbs = record.is_some();
        Live {
            resd,
            run,
            started: Instant::now(),
            since: 0,
            cpu_prev: None,
            gpu: None,
            gpu_at: None,
            trace: Tail::default(),
            results: Tail::default(),
            thumb_seen: Vec::new(),
            record,
            all_thumbs,
        }
    }

    /// Poll everything once. `want` is the agent whose thumbnail the view
    /// needs.
    pub fn poll(&mut self, want: Option<usize>, agents: usize) -> Frame {
        let status = resd_status(&self.resd, self.since).unwrap_or(Value::Null);
        if let Some(s) = status["seq"].as_u64() {
            self.since = s;
        }
        let (mem_total, mem_available) = meminfo();
        let cpu = self.cpu();
        if self
            .gpu_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.gpu = gpu();
            self.gpu_at = Some(Instant::now());
        }
        let mut f = Frame {
            t_ms: self.started.elapsed().as_millis() as u64,
            status,
            mem_total,
            mem_available,
            cpu,
            gpu: self.gpu,
            ..Frame::default()
        };
        if let Some(run) = self.run.clone() {
            f.trace = self.trace.read(&run.join("trace.jsonl"));
            // The loop appends one line as each agent finishes.
            let r = self.results.read(&run.join("results.jsonl"));
            f.results = (!r.is_empty()).then_some(r);
            f.assigned = std::fs::read_to_string(run.join("assigned.txt"))
                .map(|s| s.lines().filter(|l| !l.is_empty()).count())
                .unwrap_or(0);
            let n = agents.max(f.assigned);
            self.thumb_seen.resize(n, None);
            for i in 0..n {
                if !self.all_thumbs && Some(i) != want {
                    continue;
                }
                let p = run.join(format!("agent-{i}")).join("thumb.b64");
                let Ok(m) = std::fs::metadata(&p).and_then(|m| m.modified()) else {
                    continue;
                };
                if self.thumb_seen[i] == Some(m) {
                    continue;
                }
                if let Ok(b) = std::fs::read_to_string(&p) {
                    self.thumb_seen[i] = Some(m);
                    f.thumbs.push((i, b));
                }
            }
        }
        if let Some(rec) = &mut self.record {
            let _ = writeln!(rec, "{}", f.to_json());
        }
        f
    }

    fn cpu(&mut self) -> f64 {
        let Some((busy, total)) = proc_stat() else {
            return 0.0;
        };
        let out = match self.cpu_prev {
            Some((b0, t0)) if total > t0 => (busy - b0) as f64 / (total - t0) as f64,
            _ => 0.0,
        };
        self.cpu_prev = Some((busy, total));
        out
    }
}

#[derive(Debug)]
pub struct Replay {
    frames: Vec<Frame>,
    next: usize,
    started: Instant,
    speed: f64,
}

impl Replay {
    pub fn open(path: &Path, speed: f64) -> Result<Replay, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let frames: Vec<Frame> = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
            .map(|v| Frame::from_json(&v))
            .collect();
        if frames.is_empty() {
            return Err(format!("{}: no frames", path.display()));
        }
        Ok(Replay {
            frames,
            next: 0,
            started: Instant::now(),
            speed,
        })
    }

    /// Every frame due by now, merged into one; loops at the end.
    pub fn poll(&mut self) -> Option<Frame> {
        if self.next >= self.frames.len() {
            self.next = 0;
            self.started = Instant::now();
        }
        let base = self.frames[0].t_ms;
        let now = (self.started.elapsed().as_millis() as f64 * self.speed) as u64 + base;
        let mut out: Option<Frame> = None;
        while self.next < self.frames.len() && self.frames[self.next].t_ms <= now {
            let f = self.frames[self.next].clone();
            self.next += 1;
            out = Some(match out {
                None => f,
                Some(mut acc) => {
                    acc.trace.extend(f.trace);
                    acc.thumbs.extend(f.thumbs);
                    let mut events = acc.status["events"].as_array().cloned().unwrap_or_default();
                    events.extend(f.status["events"].as_array().cloned().unwrap_or_default());
                    let results = match (acc.results, f.results) {
                        (Some(mut a), Some(b)) => {
                            a.extend(b);
                            Some(a)
                        }
                        (a, b) => a.or(b),
                    };
                    Frame {
                        trace: acc.trace,
                        thumbs: acc.thumbs,
                        results,
                        status: {
                            let mut s = f.status;
                            if s.is_object() {
                                s["events"] = Value::Array(events);
                            }
                            s
                        },
                        ..f
                    }
                }
            });
        }
        out
    }
}

fn resd_status(sock: &Path, since: u64) -> Option<Value> {
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(500))).ok()?;
    writeln!(s, "{}", json!({ "op": "status", "since": since })).ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

fn meminfo() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map_or(0, |kb| kb << 10)
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

/// (busy, total) jiffies from the first line of /proc/stat.
fn proc_stat() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<u64> = text
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    let total: u64 = v.iter().sum();
    let idle = v.get(3)? + v.get(4).unwrap_or(&0);
    Some((total - idle, total))
}

fn gpu() -> Option<(f64, f64)> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,memory.used",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text
        .lines()
        .next()?
        .split(',')
        .map(|x| x.trim().parse::<f64>());
    Some((it.next()?.ok()?, it.next()?.ok()?))
}

/// New complete lines of a growing JSONL file since the last read.
#[derive(Debug, Default)]
struct Tail {
    off: u64,
    buf: String,
}

impl Tail {
    fn read(&mut self, path: &Path) -> Vec<Value> {
        let Ok(mut file) = File::open(path) else {
            return Vec::new();
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.off {
            self.off = 0;
            self.buf.clear();
        }
        if file.seek(SeekFrom::Start(self.off)).is_err() {
            return Vec::new();
        }
        let mut chunk = String::new();
        let Ok(n) = file.read_to_string(&mut chunk) else {
            return Vec::new();
        };
        self.off += n as u64;
        self.buf.push_str(&chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            if let Ok(v) = serde_json::from_str(line.trim()) {
                out.push(v);
            }
        }
        out
    }
}
