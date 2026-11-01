//! The owner thread. It holds the policy, the agents' names and their
//! workers, and is the only thread that changes them. The threads layer:
//! the policy decides, and the [`Host`] and the workers do the I/O.
//!
//! ```text
//! conn-N ── Request ──> owner ── Work ──> kiosk-N, one per agent
//!   ^                    │ ^                │ cgroup writes, in order
//!   └──── reply ─────────┘ └──── Done ──────┘
//! tick ──── Tick ──────> owner: discovery, then the policy's Tick
//! ```
//!
//! A request's reply rides with its agent's work, so it comes only after
//! every effect queued before it has run: an acquire is answered once the
//! kiosk is thawed. A failed effect is corrected in the policy before the
//! requester hears of it.

use crate::policy::{Effect, Id, Input, Ms, Policy, Refusal, State};
use panoptes_sync::{self as sync, mpsc};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Acquire,
    Release,
}

/// What the owner hears.
pub enum Msg {
    /// A desk's or root's request for the agent `agent`. The reply is the
    /// JSON reply line.
    Request {
        agent: String,
        op: Op,
        reply: mpsc::Sender<Value>,
    },
    /// Every agent's residency, and the transitions after `since`.
    Status {
        since: u64,
        reply: mpsc::Sender<Snapshot>,
    },
    /// Discover kiosks, then run the policy's periodic check.
    Tick,
    /// A worker finished some work.
    Done { id: Id, done: Done },
    /// SIGTERM or SIGINT. Kiosks stay as they are.
    Stop,
}

impl fmt::Debug for Msg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Msg::Request { agent, op, .. } => write!(f, "Request({agent}, {op:?})"),
            Msg::Status { since, .. } => write!(f, "Status({since})"),
            Msg::Tick => f.write_str("Tick"),
            Msg::Done { id, .. } => write!(f, "Done({id})"),
            Msg::Stop => f.write_str("Stop"),
        }
    }
}

/// One agent's effects, in order, then the reply that waits for them.
#[derive(Debug)]
pub struct Work {
    pub effects: Vec<Effect>,
    pub reply: Option<Pending>,
}

/// A request's reply, carried through its agent's worker.
pub struct Pending {
    reply: mpsc::Sender<Value>,
    from: Option<State>,
}

impl fmt::Debug for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending").field("from", &self.from).finish()
    }
}

#[derive(Debug)]
pub enum Done {
    /// A batch of effects ran, or stopped at a failure.
    Ran {
        ms: f64,
        failed: Option<Failure>,
        reply: Option<Pending>,
    },
    /// A reclaim ended.
    Parked {
        stat: Option<ParkStat>,
        error: Option<String>,
    },
}

#[derive(Debug)]
pub struct Failure {
    pub error: String,
    /// What `cgroup.freeze` asked for after the failure, when readable.
    pub frozen: Option<bool>,
}

/// The result of an agent's most recent reclaim.
#[derive(Debug, Clone, Copy)]
pub struct ParkStat {
    pub before: u64,
    pub after: u64,
    pub ms: f64,
    pub full: bool,
}

/// One kiosk, as discovery read it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub name: String,
    /// What its `cgroup.freeze` asks for.
    pub frozen: bool,
    pub populated: bool,
}

/// The owner's I/O.
pub trait Host {
    /// Every kiosk under the root.
    fn scan(&mut self) -> Vec<Seen>;
    /// The kiosk of one agent.
    fn look(&mut self, name: &str) -> Option<Seen>;
    fn mem_available(&mut self) -> u64;
    /// Write the state file.
    fn save(&mut self, body: &str);
    /// Start a new agent's worker, which reports to `done`.
    fn start(&mut self, id: Id, name: &str, done: mpsc::Sender<Msg>) -> mpsc::Sender<Work>;
}

/// Every agent's residency, for `status`.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub now_ms: Ms,
    pub agents: Vec<Row>,
    pub seq: u64,
    pub events: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub state: State,
    pub for_ms: Ms,
    pub park: Option<ParkStat>,
}

/// Transitions kept for status readers.
const LOG_LEN: usize = 1024;

struct Agent {
    name: String,
    work: mpsc::Sender<Work>,
    park: Option<ParkStat>,
}

struct Owner<H> {
    host: H,
    policy: Policy,
    /// A sender for each new worker.
    me: mpsc::Sender<Msg>,
    ids: HashMap<String, Id>,
    agents: BTreeMap<Id, Agent>,
    next_id: Id,
    /// The state file as resd started, for agents found later.
    saved: HashMap<String, State>,
    /// The policy version the state file holds.
    saved_version: Option<u64>,
    /// Recent transitions for `panoptes top`, numbered from 1.
    seq: u64,
    log: VecDeque<Value>,
}

/// Run until `Stop`. Returns the host and the policy, for tests.
pub fn run<H: Host>(
    host: H,
    policy: Policy,
    saved: HashMap<String, State>,
    msgs: mpsc::Receiver<Msg>,
    me: mpsc::Sender<Msg>,
) -> (H, Policy) {
    let mut o = Owner {
        host,
        policy,
        me,
        ids: HashMap::new(),
        agents: BTreeMap::new(),
        next_id: 1,
        saved,
        saved_version: None,
        seq: 0,
        log: VecDeque::new(),
    };
    // Recovery: learn every kiosk before serving, without thawing any.
    o.discover();
    o.save();
    let mut counts = BTreeMap::<&str, usize>::new();
    for a in o.policy.agents().values() {
        *counts.entry(a.state.name()).or_default() += 1;
    }
    eprintln!("panoptes-resd: recovered {counts:?}");
    loop {
        match sync::recv(&msgs) {
            Ok(Msg::Request { agent, op, reply }) => o.request(agent, op, reply),
            Ok(Msg::Status { since, reply }) => {
                let _ = reply.send(o.snapshot(since));
            }
            Ok(Msg::Tick) => {
                o.discover();
                let mem_available = o.host.mem_available();
                o.step(Input::Tick { mem_available });
                o.save();
            }
            Ok(Msg::Done { id, done }) => o.done(id, done),
            Ok(Msg::Stop) | Err(_) => break,
        }
    }
    o.save();
    (o.host, o.policy)
}

impl<H: Host> Owner<H> {
    /// Feed the policy one input and send its effects to the workers.
    fn step(&mut self, input: Input) {
        if let Ok(effects) = self.policy.step(input, sync::now()) {
            self.route(effects, None);
        }
    }

    /// Group effects by agent, and send each agent's to its worker, with
    /// `pending` riding behind the effects of its agent.
    fn route(&mut self, effects: Vec<(Id, Effect)>, mut pending: Option<(Id, Pending)>) {
        let mut by: BTreeMap<Id, Vec<Effect>> = BTreeMap::new();
        for (id, e) in effects {
            by.entry(id).or_default().push(e);
        }
        if let Some((id, _)) = &pending {
            by.entry(*id).or_default();
        }
        for (id, effects) in by {
            let reply = pending.take_if(|(p, _)| *p == id).map(|(_, p)| p);
            let agent = self.agents.get(&id).expect("effects name known agents");
            let _ = agent.work.send(Work { effects, reply });
        }
    }

    fn request(&mut self, name: String, op: Op, reply: mpsc::Sender<Value>) {
        // A desk that connects before discovery found its kiosk.
        if !self.ids.contains_key(&name)
            && let Some(seen) = self.host.look(&name)
        {
            self.found(seen);
        }
        let Some(&id) = self.ids.get(&name) else {
            let _ = reply.send(json!({ "ok": false, "error": "unknown agent" }));
            return;
        };
        let from = self.policy.state(id);
        let input = match op {
            Op::Acquire => Input::Acquire { id },
            Op::Release => Input::Release { id },
        };
        match self.policy.step(input, sync::now()) {
            Ok(effects) => self.route(effects, Some((id, Pending { reply, from }))),
            Err(refusal) => {
                let error = match refusal {
                    Refusal::Unknown => "unknown agent",
                    Refusal::Evicted => "evicted",
                };
                let _ = reply.send(json!({ "ok": false, "error": error }));
            }
        }
    }

    fn done(&mut self, id: Id, done: Done) {
        let name = self.agents.get(&id).map_or("?", |a| &a.name).to_owned();
        match done {
            Done::Ran { ms, failed, reply } => {
                if let Some(f) = &failed {
                    eprintln!("panoptes-resd: {name}: {}", f.error);
                    if let Some(frozen) = f.frozen {
                        self.step(Input::Failed { id, frozen });
                    }
                }
                let Some(Pending { reply, from }) = reply else {
                    return;
                };
                let state = self.policy.state(id);
                let line = match failed {
                    Some(f) => json!({ "ok": false, "error": f.error }),
                    None => {
                        if let (Some(f), Some(t)) = (from, state)
                            && f != t
                        {
                            self.note(&name, f.name(), t.name(), ms, json!({}));
                        }
                        json!({
                            "ok": true,
                            "state": state.map(State::name),
                            "from": from.map(State::name),
                            "ms": ms,
                        })
                    }
                };
                let _ = reply.send(line);
            }
            Done::Parked { stat, error } => {
                if let Some(e) = error {
                    eprintln!("panoptes-resd: {name}: reclaim: {e}");
                }
                if let Some(s) = stat {
                    eprintln!(
                        "panoptes-resd: parked {name}: {} -> {} MiB in {} ms",
                        s.before >> 20,
                        s.after >> 20,
                        s.ms
                    );
                    if let Some(a) = self.agents.get_mut(&id) {
                        a.park = Some(s);
                    }
                    let extra = json!({ "before": s.before, "after": s.after });
                    self.note(&name, "waiting", "parked", s.ms, extra);
                }
                self.step(Input::Reclaimed { id });
            }
        }
    }

    /// Tell the policy about new, revived, emptied and vanished kiosks.
    fn discover(&mut self) {
        let mut present = HashSet::new();
        for seen in self.host.scan() {
            present.insert(seen.name.clone());
            self.found(seen);
        }
        let gone: Vec<(String, Id)> = self
            .ids
            .iter()
            .filter(|(name, _)| !present.contains(*name))
            .map(|(name, &id)| (name.clone(), id))
            .collect();
        for (name, id) in gone {
            self.ids.remove(&name);
            // Dropping its sender ends the worker once its queue is done.
            self.agents.remove(&id);
            self.step(Input::Gone { id });
        }
    }

    fn found(&mut self, seen: Seen) {
        let id = match self.ids.get(&seen.name) {
            Some(&id) => id,
            None => {
                let id = self.next_id;
                self.next_id += 1;
                let work = self.host.start(id, &seen.name, self.me.clone());
                self.ids.insert(seen.name.clone(), id);
                let agent = Agent {
                    name: seen.name.clone(),
                    work,
                    park: None,
                };
                self.agents.insert(id, agent);
                id
            }
        };
        let input = match self.policy.state(id) {
            Some(s) if !seen.populated && s != State::Evicted => Input::Lost { id },
            _ => Input::Found {
                id,
                frozen: seen.frozen,
                populated: seen.populated,
                saved: self.saved.get(&seen.name).copied(),
            },
        };
        self.step(input);
    }

    /// Write the state file, if the states changed since the last write.
    /// Recovery trusts the kernel over the file, so a write a tick late
    /// costs nothing.
    fn save(&mut self) {
        let version = self.policy.version();
        if self.saved_version == Some(version) {
            return;
        }
        let map: serde_json::Map<String, Value> = self
            .policy
            .agents()
            .iter()
            .filter_map(|(id, a)| Some((self.agents.get(id)?.name.clone(), a.state.name().into())))
            .collect();
        self.host.save(&Value::Object(map).to_string());
        self.saved_version = Some(version);
    }

    /// Record one transition, with the time its cgroup writes took.
    fn note(&mut self, agent: &str, from: &str, to: &str, took_ms: f64, extra: Value) {
        self.seq += 1;
        let mut e = json!({ "seq": self.seq, "t_ms": sync::now(), "agent": agent,
                            "from": from, "to": to, "ms": took_ms });
        if let (Some(e), Value::Object(x)) = (e.as_object_mut(), extra) {
            e.extend(x);
        }
        self.log.push_back(e);
        if self.log.len() > LOG_LEN {
            self.log.pop_front();
        }
    }

    fn snapshot(&self, since: u64) -> Snapshot {
        let now = sync::now();
        let agents = self
            .policy
            .agents()
            .iter()
            .filter_map(|(id, a)| {
                let agent = self.agents.get(id)?;
                Some(Row {
                    name: agent.name.clone(),
                    state: a.state,
                    for_ms: now.saturating_sub(a.since),
                    park: agent.park,
                })
            })
            .collect();
        let events = self
            .log
            .iter()
            .filter(|e| e["seq"].as_u64().is_some_and(|s| s > since))
            .cloned()
            .collect();
        Snapshot {
            now_ms: now,
            agents,
            seq: self.seq,
            events,
        }
    }
}

#[cfg(all(test, panoptes_loom))]
mod loom_models {
    use super::*;
    use crate::policy::{Config, Job};
    use crate::worker::{self, Chunk, Kiosk};
    use panoptes_sync::{Arc, thread};
    // std's atomics, not loom's: the fake kiosk is not under test, and
    // loom would branch on every access. The channels order its accesses.
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::SeqCst};

    /// loom's bound on preemptions. Unbounded, each model ran for over 10
    /// minutes: three threads pass many messages. A planted bug still
    /// fails each model at this bound.
    const PREEMPTIONS: usize = 2;

    fn model(f: impl Fn() + Sync + Send + 'static) {
        let mut b = panoptes_sync::model::Builder::new();
        b.preemption_bound = Some(PREEMPTIONS);
        b.check(f);
    }

    /// One kiosk, as the fake cgroup writes leave it.
    #[derive(Debug, Default)]
    struct Cg {
        frozen: AtomicBool,
        thaws: AtomicU32,
        /// Thaws still to fail, leaving the kiosk frozen.
        failing: AtomicU32,
        chunks: AtomicU32,
    }

    struct FakeKiosk(Arc<Cg>);

    impl Kiosk for FakeKiosk {
        fn apply(&mut self, effect: Effect) -> Result<f64, String> {
            match effect {
                Effect::Thaw => {
                    self.0.thaws.fetch_add(1, SeqCst);
                    if self.0.failing.load(SeqCst) > 0 {
                        self.0.failing.fetch_sub(1, SeqCst);
                        return Err("thaw: timed out".into());
                    }
                    self.0.frozen.store(false, SeqCst);
                }
                Effect::Freeze => self.0.frozen.store(true, SeqCst),
                Effect::Weight(_) => {}
                Effect::Park => unreachable!("parks go through park_begin"),
            }
            Ok(0.0)
        }
        fn frozen(&mut self) -> Option<bool> {
            Some(self.0.frozen.load(SeqCst))
        }
        fn park_begin(&mut self) -> Result<bool, String> {
            Ok(self.0.frozen.load(SeqCst))
        }
        /// Two chunks take everything.
        fn reclaim_chunk(&mut self) -> Result<Chunk, String> {
            let n = self.0.chunks.fetch_add(1, SeqCst) + 1;
            Ok(if n >= 2 { Chunk::Full } else { Chunk::More })
        }
        fn park_end(&mut self, full: bool) -> Result<ParkStat, String> {
            Ok(ParkStat {
                before: 0,
                after: 0,
                ms: 0.0,
                full,
            })
        }
    }

    /// One agent, `a`, whose kiosk is `cg`.
    struct FakeHost(Arc<Cg>);

    impl FakeHost {
        fn seen(&self) -> Seen {
            Seen {
                name: "a".into(),
                frozen: self.0.frozen.load(SeqCst),
                populated: true,
            }
        }
    }

    impl Host for FakeHost {
        fn scan(&mut self) -> Vec<Seen> {
            vec![self.seen()]
        }
        fn look(&mut self, _: &str) -> Option<Seen> {
            Some(self.seen())
        }
        fn mem_available(&mut self) -> u64 {
            u64::MAX
        }
        fn save(&mut self, _: &str) {}
        fn start(&mut self, id: Id, _: &str, done: mpsc::Sender<Msg>) -> mpsc::Sender<Work> {
            let (tx, rx) = mpsc::channel();
            let kiosk = FakeKiosk(self.0.clone());
            thread::spawn(move || worker::run(id, kiosk, rx, done));
            tx
        }
    }

    /// Start the owner over a frozen kiosk saved as `saved`.
    fn start(
        cg: &Arc<Cg>,
        saved: State,
    ) -> (mpsc::Sender<Msg>, thread::JoinHandle<(FakeHost, Policy)>) {
        cg.frozen.store(true, SeqCst);
        let (tx, rx) = mpsc::channel();
        let saved = HashMap::from([("a".to_owned(), saved)]);
        let host = FakeHost(cg.clone());
        let me = tx.clone();
        let owner = thread::spawn(move || run(host, Policy::new(Config::default()), saved, rx, me));
        (tx, owner)
    }

    fn ask(owner: &mpsc::Sender<Msg>, op: Op) -> mpsc::Receiver<Value> {
        let (reply, rx) = mpsc::channel();
        let agent = "a".into();
        owner.send(Msg::Request { agent, op, reply }).unwrap();
        rx
    }

    /// Two acquires arrive while the first one's thaw runs. Neither is
    /// answered before the kiosk is thawed. The second, which needs no
    /// effect, is read first: read after the first, it would always find
    /// the thaw done.
    #[test]
    fn every_acquire_is_answered_after_its_thaw() {
        model(|| {
            let cg = Arc::new(Cg::default());
            let (tx, owner) = start(&cg, State::Waiting);
            let first = ask(&tx, Op::Acquire);
            let second = ask(&tx, Op::Acquire);
            for rx in [second, first] {
                assert_eq!(rx.recv().unwrap()["ok"], true);
                assert!(!cg.frozen.load(SeqCst), "answered before the thaw");
            }
            tx.send(Msg::Stop).unwrap();
            owner.join().unwrap();
        });
    }

    /// An acquire arrives while a recovered kiosk is being reclaimed. It
    /// stops the reclaim, is answered with the kiosk thawed, and the
    /// reclaim's slot is free afterwards.
    #[test]
    fn an_acquire_stops_a_reclaim_and_frees_its_slot() {
        model(|| {
            let cg = Arc::new(Cg::default());
            let (tx, owner) = start(&cg, State::Parked);
            let reply = ask(&tx, Op::Acquire).recv().unwrap();
            assert_eq!(reply["ok"], true);
            assert!(!cg.frozen.load(SeqCst));
            tx.send(Msg::Stop).unwrap();
            let (_, policy) = owner.join().unwrap();
            assert!(cg.chunks.load(SeqCst) <= 2);
            let a = policy.agents().values().next().unwrap();
            assert_eq!(a.job, Job::None, "the reclaim's slot stayed taken");
        });
    }

    /// A thaw fails and leaves the kiosk frozen. The policy learns that
    /// before the reply, so the next acquire thaws again.
    #[test]
    fn a_failed_thaw_is_corrected_before_its_reply() {
        model(|| {
            let cg = Arc::new(Cg::default());
            cg.failing.store(1, SeqCst);
            let (tx, owner) = start(&cg, State::Waiting);
            assert_eq!(ask(&tx, Op::Acquire).recv().unwrap()["ok"], false);
            assert_eq!(ask(&tx, Op::Acquire).recv().unwrap()["ok"], true);
            assert_eq!(cg.thaws.load(SeqCst), 2);
            assert!(!cg.frozen.load(SeqCst));
            tx.send(Msg::Stop).unwrap();
            owner.join().unwrap();
        });
    }
}
