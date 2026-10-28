//! The command center's model of the fleet, updated from each [`Frame`].

use crate::source::Frame;
use base64::Engine;
use serde_json::Value;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Starting,
    Acting,
    Waiting,
    Parked,
    Evicted,
    /// Its desk is gone and the loop has not reported yet.
    Finished,
    Done,
    Failed,
    /// Assigned but not started.
    Queued,
}

impl Kind {
    fn from_resd(s: &str) -> Kind {
        match s {
            "acting" => Kind::Acting,
            "waiting" => Kind::Waiting,
            "parked" => Kind::Parked,
            "evicted" => Kind::Evicted,
            _ => Kind::Starting,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Starting => "starting",
            Kind::Acting => "acting",
            Kind::Waiting => "waiting",
            Kind::Parked => "parked",
            Kind::Evicted => "evicted",
            Kind::Finished => "finished",
            Kind::Done => "done",
            Kind::Failed => "failed",
            Kind::Queued => "queued",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub kind: Kind,
    pub for_ms: u64,
    pub mem: Option<u64>,
    pub zswap: Option<u64>,
    pub zswapped: Option<u64>,
    pub last_park: Option<Value>,
    pub invoice: Option<String>,
    pub steps: usize,
    /// The most recent trace lines, newest last.
    pub recent: VecDeque<Value>,
    pub thumb_b64: Option<String>,
    /// Seen by resd at some point.
    pub seen: bool,
}

impl Agent {
    fn new(name: String) -> Agent {
        Agent {
            name,
            kind: Kind::Queued,
            for_ms: 0,
            mem: None,
            zswap: None,
            zswapped: None,
            last_park: None,
            invoice: None,
            steps: 0,
            recent: VecDeque::new(),
            thumb_b64: None,
            seen: false,
        }
    }
}

/// A decoded thumbnail: width, height, RGB triples.
#[derive(Debug, Clone)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<[u8; 3]>,
}

#[derive(Debug, Clone, Copy)]
pub enum Tone {
    Park,
    Wake,
    Freeze,
    Done,
    Fail,
}

#[derive(Debug)]
pub struct App {
    /// Loop agents, by loop index.
    pub agents: Vec<Agent>,
    /// resd agents that are not loop desks; counted, not drawn.
    pub others: usize,
    pub selected: usize,
    pub big: bool,
    pub host_name: String,
    pub frame: Frame,
    pub zswap_pool: Option<u64>,
    pub resd_up: bool,
    /// Frame times of recent steps, for steps/s.
    steps_at: VecDeque<u64>,
    pub steps_total: usize,
    pub events: VecDeque<(String, Tone)>,
    thumb_cache: Option<(usize, usize, Image)>,
    pub replay: bool,
}

/// `pdesk-loop-7.service` is loop agent 7.
fn loop_index(name: &str) -> Option<usize> {
    name.strip_prefix("pdesk-loop-")?
        .strip_suffix(".service")?
        .parse()
        .ok()
}

fn short(name: &str) -> String {
    match loop_index(name) {
        Some(i) => format!("{i:04}"),
        None => name.trim_end_matches(".service").to_owned(),
    }
}

impl App {
    pub fn new(host_name: String, replay: bool) -> App {
        App {
            agents: Vec::new(),
            others: 0,
            selected: 0,
            big: false,
            host_name,
            frame: Frame::default(),
            zswap_pool: None,
            resd_up: false,
            steps_at: VecDeque::new(),
            steps_total: 0,
            events: VecDeque::new(),
            thumb_cache: None,
            replay,
        }
    }

    /// The slot for loop agent `i`, growing the fleet as needed.
    fn slot(&mut self, i: usize) -> &mut Agent {
        while self.agents.len() <= i {
            let n = self.agents.len();
            self.agents
                .push(Agent::new(format!("pdesk-loop-{n}.service")));
        }
        &mut self.agents[i]
    }

    fn push_event(&mut self, text: String, tone: Tone) {
        self.events.push_front((text, tone));
        self.events.truncate(64);
    }

    pub fn ingest(&mut self, f: Frame) {
        if f.assigned > 0 {
            self.slot(f.assigned - 1);
        }
        self.resd_up = f.status.is_object();
        self.zswap_pool = f.status["host"]["zswap_pool"].as_u64();
        // Residency: present agents, then those that vanished.
        let mut present = vec![false; self.agents.len()];
        if let Some(rows) = f.status["agents"].as_array() {
            self.others = 0;
            for r in rows {
                let Some(i) = r["agent"].as_str().and_then(loop_index) else {
                    self.others += 1;
                    continue;
                };
                self.slot(i);
                present.resize(self.agents.len(), false);
                present[i] = true;
                let a = &mut self.agents[i];
                a.seen = true;
                if !matches!(a.kind, Kind::Done | Kind::Failed) {
                    a.kind = Kind::from_resd(r["state"].as_str().unwrap_or(""));
                }
                a.for_ms = r["for_ms"].as_u64().unwrap_or(0);
                a.mem = r["memory_current"].as_u64();
                a.zswap = r["zswap"].as_u64();
                a.zswapped = r["zswapped"].as_u64();
                if !r["last_park"].is_null() {
                    a.last_park = Some(r["last_park"].clone());
                }
            }
            for (i, a) in self.agents.iter_mut().enumerate() {
                let here = present.get(i).copied().unwrap_or(false);
                if !here && a.seen {
                    // Its cgroup is gone; stale numbers would inflate totals.
                    (a.mem, a.zswap, a.zswapped) = (None, None, None);
                    if !matches!(a.kind, Kind::Done | Kind::Failed) {
                        a.kind = Kind::Finished;
                    }
                }
            }
        }
        // Transitions, oldest first so the newest ends up leftmost.
        let events = f.status["events"].as_array().cloned().unwrap_or_default();
        for e in events {
            let who = short(e["agent"].as_str().unwrap_or("?"));
            let ms = e["ms"].as_f64().unwrap_or(0.0);
            let (from, to) = (
                e["from"].as_str().unwrap_or(""),
                e["to"].as_str().unwrap_or(""),
            );
            match (from, to) {
                (_, "parked") => {
                    let b = e["before"].as_u64().unwrap_or(0) >> 20;
                    let a = e["after"].as_u64().unwrap_or(0) >> 20;
                    self.push_event(format!("{who} parked {b}→{a}M in {ms:.0}ms"), Tone::Park);
                }
                ("parked", "acting") => {
                    self.push_event(format!("{who} unparked, thaw {ms:.1}ms"), Tone::Wake);
                }
                (_, "acting") => self.push_event(format!("{who} woke, thaw {ms:.1}ms"), Tone::Wake),
                (_, "waiting") => self.push_event(format!("{who} froze {ms:.1}ms"), Tone::Freeze),
                _ => {}
            }
        }
        // Steps.
        for t in &f.trace {
            let Some(i) = t["agent"].as_u64().map(|i| i as usize) else {
                continue;
            };
            let a = self.slot(i);
            if a.kind == Kind::Queued {
                a.kind = Kind::Starting;
            }
            a.invoice = t["invoice"].as_str().map(str::to_owned);
            let step = t["step"].as_u64().unwrap_or(0) as usize;
            a.steps = a.steps.max(step);
            a.recent.push_back(t.clone());
            while a.recent.len() > 6 {
                a.recent.pop_front();
            }
            if step > 0 {
                self.steps_at.push_back(f.t_ms);
                self.steps_total += 1;
            }
        }
        while self.steps_at.front().is_some_and(|&t| t + 10_000 < f.t_ms) {
            self.steps_at.pop_front();
        }
        // Outcomes, as each agent finishes.
        if let Some(results) = &f.results {
            for r in results.clone() {
                let Some(i) = r["agent"].as_u64().map(|i| i as usize) else {
                    continue;
                };
                // The loop's REST check when it ran; else the agent's claim.
                let done = r["verified"].as_bool().unwrap_or(r["done"] == true);
                let inv = r["invoice"].as_str().unwrap_or("?").to_owned();
                let a = self.slot(i);
                if matches!(a.kind, Kind::Done | Kind::Failed) {
                    continue;
                }
                a.kind = if done { Kind::Done } else { Kind::Failed };
                let who = format!("{i:04}");
                if done {
                    self.push_event(format!("{who} done {inv} ✓"), Tone::Done);
                } else {
                    self.push_event(format!("{who} failed {inv} ✗"), Tone::Fail);
                }
            }
        }
        for (i, b) in &f.thumbs {
            self.slot(*i).thumb_b64 = Some(b.clone());
        }
        self.frame = Frame {
            trace: Vec::new(),
            thumbs: Vec::new(),
            results: None,
            ..f
        };
        if self.selected >= self.agents.len() {
            self.selected = self.agents.len().saturating_sub(1);
        }
    }

    pub fn steps_per_s(&self) -> f64 {
        let span = self.frame.t_ms.clamp(1, 10_000) as f64 / 1000.0;
        self.steps_at.len() as f64 / span
    }

    pub fn count(&self, k: Kind) -> usize {
        self.agents.iter().filter(|a| a.kind == k).count()
    }

    /// The selected agent's thumbnail, decoded once per new capture.
    pub fn thumb(&mut self) -> Option<&Image> {
        let i = self.selected;
        let b64 = self.agents.get(i)?.thumb_b64.as_ref()?;
        let key = b64.len() ^ b64.as_bytes().get(b64.len() / 2).copied().unwrap_or(0) as usize;
        let fresh = !matches!(&self.thumb_cache, Some((ci, ck, _)) if *ci == i && *ck == key);
        if fresh {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .ok()?;
            let mut d = jpeg_decoder::Decoder::new(&bytes[..]);
            let px = d.decode().ok()?;
            let info = d.info()?;
            let (w, h) = (info.width as usize, info.height as usize);
            let rgb = match info.pixel_format {
                jpeg_decoder::PixelFormat::RGB24 => {
                    px.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
                }
                jpeg_decoder::PixelFormat::L8 => px.iter().map(|&g| [g, g, g]).collect(),
                _ => return None,
            };
            self.thumb_cache = Some((i, key, Image { w, h, rgb }));
        }
        self.thumb_cache.as_ref().map(|(_, _, img)| img)
    }

    pub fn move_sel(&mut self, dx: isize, dy: isize, per_row: usize) {
        let n = self.agents.len() as isize;
        if n == 0 {
            return;
        }
        let s = self.selected as isize + dx + dy * per_row.max(1) as isize;
        self.selected = s.clamp(0, n - 1) as usize;
    }
}
