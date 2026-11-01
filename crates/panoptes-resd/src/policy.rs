//! Residency policy: a pure state machine from inputs and a clock to
//! effects. No I/O, clock or threads. The owner thread carries out the
//! effects it returns and reports what it observes back as inputs.
//!
//! ```text
//! starting ─release─> waiting ─park_after or memory low─> parked
//!    │                  ^  │                                 │
//!    └─acquire─> acting ┘  └──────────acquire──────────┬─────┘
//!                  ^                                    │
//!                  └──────────────(thaw)────────────────┘
//! evicted: the kiosk has no processes (it exited, or something outside
//! resd killed it); acquire fails until it runs again. resd never kills a
//! kiosk itself, because nothing relaunches one yet.
//! ```
//!
//! A parked kiosk is frozen at once, and reclaimed into zswap once a slot
//! is free: at most `park_jobs` reclaims run at once, first come first.

use std::collections::{BTreeMap, VecDeque};

/// Milliseconds on the daemon's clock.
pub type Ms = u64;
/// An agent. The owner numbers agents in the order it finds them and never
/// reuses a number.
pub type Id = u32;

/// An agent's residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Running, never handed to the policy by its desk.
    Starting,
    /// Thawed and weighted up while its agent acts.
    Acting,
    /// Frozen while its agent waits.
    Waiting,
    /// Frozen, and reclaimed into zswap or queued for it.
    Parked,
    /// The kiosk has no processes.
    Evicted,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Acting => "acting",
            State::Waiting => "waiting",
            State::Parked => "parked",
            State::Evicted => "evicted",
        }
    }

    pub fn from_name(s: &str) -> Option<State> {
        Some(match s {
            "starting" => State::Starting,
            "acting" => State::Acting,
            "waiting" => State::Waiting,
            "parked" => State::Parked,
            "evicted" => State::Evicted,
            _ => return None,
        })
    }
}

/// Where an agent's reclaim stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    None,
    /// Parked, waiting for a free slot.
    Queued,
    /// A worker is reclaiming the kiosk.
    Running,
    /// Running, and parked again since it started: an acquire stopped that
    /// reclaim, so queue another once it ends.
    Rerun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Agent {
    pub state: State,
    /// When the agent entered `state`.
    pub since: Ms,
    pub job: Job,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Discovery found a kiosk. `frozen` is what its `cgroup.freeze` asks
    /// for, `populated` what its `cgroup.events` says, and `saved` the state
    /// file's entry.
    Found {
        id: Id,
        frozen: bool,
        populated: bool,
        saved: Option<State>,
    },
    /// The kiosk has no processes.
    Lost {
        id: Id,
    },
    /// The agent's cgroup is gone.
    Gone {
        id: Id,
    },
    Acquire {
        id: Id,
    },
    Release {
        id: Id,
    },
    /// The periodic check, with the host's MemAvailable in bytes.
    Tick {
        mem_available: u64,
    },
    /// A reclaim ended: finished, stopped early, or failed.
    Reclaimed {
        id: Id,
    },
    /// An effect failed. `frozen` is what the kiosk's `cgroup.freeze` asks
    /// for now. A freeze that timed out still asks for frozen, and the
    /// kernel may finish it later.
    Failed {
        id: Id,
        frozen: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Thaw,
    Freeze,
    /// Set the kiosk's CPU weight. Best effort: a failure changes nothing
    /// the policy tracks.
    Weight(u16),
    /// Reclaim the frozen kiosk into zswap with writeback off, and answer
    /// with `Reclaimed` however it ends.
    Park,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Waiting this long parks an agent.
    pub park_after: Ms,
    /// Below this MemAvailable, park the longest-waiting agents early.
    pub mem_low: u64,
    /// Agents parked early per tick while memory is low.
    pub pressure_batch: usize,
    /// Reclaims running at once.
    pub park_jobs: u32,
    pub weight_acting: u16,
    pub weight_idle: u16,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            park_after: 30_000,
            mem_low: 4 << 30,
            pressure_batch: 8,
            park_jobs: 4,
            weight_acting: 10_000,
            weight_idle: 100,
        }
    }
}

/// Why an acquire cannot proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Unknown,
    Evicted,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub cfg: Config,
    agents: BTreeMap<Id, Agent>,
    /// Parked agents waiting for a reclaim slot, first come first.
    queue: VecDeque<Id>,
    /// Agents whose job is `Running` or `Rerun`.
    running: u32,
    /// Counts changes to the agents and their states, so the owner can
    /// save them only when they changed.
    version: u64,
}

impl Policy {
    pub fn new(cfg: Config) -> Policy {
        Policy {
            cfg,
            agents: BTreeMap::new(),
            queue: VecDeque::new(),
            running: 0,
            version: 0,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn agents(&self) -> &BTreeMap<Id, Agent> {
        &self.agents
    }

    pub fn state(&self, id: Id) -> Option<State> {
        self.agents.get(&id).map(|a| a.state)
    }

    /// Apply one input at `now`. Acquire returns `Err` when the agent cannot
    /// act; every other input succeeds. Each effect names its agent.
    pub fn step(&mut self, input: Input, now: Ms) -> Result<Vec<(Id, Effect)>, Refusal> {
        let mut out = Vec::new();
        let r = self.apply(input, now, &mut out);
        self.start_reclaims(&mut out);
        if cfg!(debug_assertions) {
            self.check();
        }
        r.map(|()| out)
    }

    fn apply(&mut self, input: Input, now: Ms, out: &mut Vec<(Id, Effect)>) -> Result<(), Refusal> {
        match input {
            Input::Found {
                id,
                frozen,
                populated,
                saved,
            } => match self.state(id) {
                // A restarted kiosk comes back as a new start.
                Some(State::Evicted) if populated && !frozen => self.set(id, State::Starting, now),
                Some(_) => {}
                None => {
                    // Recovery never thaws or kills: a frozen kiosk stays
                    // frozen, and the state file only says whether it was
                    // parked.
                    let state = match (populated, frozen, saved) {
                        (false, _, _) => State::Evicted,
                        (true, true, Some(State::Parked)) => State::Parked,
                        (true, true, _) => State::Waiting,
                        (true, false, Some(State::Acting)) => State::Acting,
                        (true, false, _) => State::Starting,
                    };
                    let job = Job::None;
                    self.version += 1;
                    self.agents.insert(
                        id,
                        Agent {
                            state,
                            since: now,
                            job,
                        },
                    );
                    // The state file records parked before the reclaim
                    // runs, so a crash can leave a parked kiosk
                    // unreclaimed. Reclaiming again is harmless.
                    if state == State::Parked {
                        self.queue_reclaim(id);
                    }
                }
            },
            Input::Lost { id } => {
                if self.agents.contains_key(&id) {
                    self.unpark(id);
                    self.set(id, State::Evicted, now);
                }
            }
            Input::Gone { id } => {
                self.queue.retain(|&q| q != id);
                if let Some(a) = self.agents.remove(&id) {
                    self.version += 1;
                    if matches!(a.job, Job::Running | Job::Rerun) {
                        self.running -= 1;
                    }
                }
            }
            Input::Acquire { id } => {
                let state = self.state(id).ok_or(Refusal::Unknown)?;
                match state {
                    State::Evicted => return Err(Refusal::Evicted),
                    State::Acting => {}
                    State::Starting => out.push((id, Effect::Weight(self.cfg.weight_acting))),
                    State::Waiting | State::Parked => {
                        out.push((id, Effect::Thaw));
                        out.push((id, Effect::Weight(self.cfg.weight_acting)));
                    }
                }
                self.unpark(id);
                self.set(id, State::Acting, now);
            }
            Input::Release { id } => {
                if matches!(self.state(id), Some(State::Starting | State::Acting)) {
                    out.push((id, Effect::Freeze));
                    out.push((id, Effect::Weight(self.cfg.weight_idle)));
                    self.set(id, State::Waiting, now);
                }
            }
            Input::Tick { mem_available } => {
                let mut waiting: Vec<(Ms, Id)> = self
                    .agents
                    .iter()
                    .filter(|(_, a)| a.state == State::Waiting)
                    .map(|(&id, a)| (a.since, id))
                    .collect();
                waiting.sort();
                let low = mem_available < self.cfg.mem_low;
                for (i, (since, id)) in waiting.into_iter().enumerate() {
                    let due = now.saturating_sub(since) >= self.cfg.park_after;
                    if due || (low && i < self.cfg.pressure_batch) {
                        self.set(id, State::Parked, now);
                        self.queue_reclaim(id);
                    }
                }
            }
            Input::Reclaimed { id } => {
                if let Some(a) = self.agents.get_mut(&id) {
                    let rerun = match a.job {
                        Job::Running => false,
                        Job::Rerun => true,
                        Job::None | Job::Queued => return Ok(()),
                    };
                    a.job = Job::None;
                    self.running -= 1;
                    if rerun {
                        self.queue_reclaim(id);
                    }
                }
            }
            Input::Failed { id, frozen } => {
                let Some(state) = self.state(id) else {
                    return Ok(());
                };
                let state = match (state, frozen) {
                    (State::Waiting | State::Parked, false) => State::Acting,
                    (State::Acting | State::Starting, true) => State::Waiting,
                    (s, _) => s,
                };
                if state == State::Acting {
                    self.unpark(id);
                }
                self.set(id, state, now);
            }
        }
        Ok(())
    }

    fn set(&mut self, id: Id, state: State, now: Ms) {
        if let Some(a) = self.agents.get_mut(&id)
            && a.state != state
        {
            a.state = state;
            a.since = now;
            self.version += 1;
        }
    }

    /// The agent was just parked: reclaim it once a slot is free.
    fn queue_reclaim(&mut self, id: Id) {
        let Some(a) = self.agents.get_mut(&id) else {
            return;
        };
        match a.job {
            Job::None => {
                a.job = Job::Queued;
                self.queue.push_back(id);
            }
            Job::Running => a.job = Job::Rerun,
            Job::Queued | Job::Rerun => {}
        }
    }

    /// The agent is no longer parked: drop its queued reclaim. A running
    /// one ends on its own.
    fn unpark(&mut self, id: Id) {
        let Some(a) = self.agents.get_mut(&id) else {
            return;
        };
        match a.job {
            Job::Queued => {
                a.job = Job::None;
                self.queue.retain(|&q| q != id);
            }
            Job::Rerun => a.job = Job::Running,
            Job::None | Job::Running => {}
        }
    }

    fn start_reclaims(&mut self, out: &mut Vec<(Id, Effect)>) {
        while self.running < self.cfg.park_jobs {
            let Some(id) = self.queue.pop_front() else {
                break;
            };
            let a = self.agents.get_mut(&id).expect("queued agents exist");
            a.job = Job::Running;
            self.running += 1;
            out.push((id, Effect::Park));
        }
    }

    /// Assert the invariants.
    pub fn check(&self) {
        let running = self
            .agents
            .values()
            .filter(|a| matches!(a.job, Job::Running | Job::Rerun))
            .count();
        assert_eq!(running, self.running as usize, "running count");
        assert!(self.running <= self.cfg.park_jobs, "too many reclaims");
        let queued = self
            .agents
            .values()
            .filter(|a| a.job == Job::Queued)
            .count();
        assert_eq!(queued, self.queue.len(), "queue and jobs disagree");
        for id in &self.queue {
            let a = &self.agents[id];
            assert_eq!(a.job, Job::Queued, "agent {id} queued twice or not at all");
        }
        for (id, a) in &self.agents {
            if matches!(a.job, Job::Queued | Job::Rerun) {
                assert_eq!(a.state, State::Parked, "agent {id} queued but not parked");
            }
        }
        assert!(
            self.queue.is_empty() || self.running == self.cfg.park_jobs,
            "a reclaim waits while a slot is free"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;
    const A: Id = 1;
    const B: Id = 2;
    const C: Id = 3;

    fn policy() -> Policy {
        let mut p = Policy::new(Config {
            park_after: 1_000,
            mem_low: 2 * GIB,
            pressure_batch: 2,
            ..Config::default()
        });
        for (i, id) in [A, B, C].into_iter().enumerate() {
            p.step(
                Input::Found {
                    id,
                    frozen: false,
                    populated: true,
                    saved: None,
                },
                i as Ms,
            )
            .unwrap();
        }
        p
    }

    fn state(p: &Policy, id: Id) -> State {
        p.state(id).unwrap()
    }

    fn acq(p: &mut Policy, id: Id, now: Ms) -> Vec<(Id, Effect)> {
        p.step(Input::Acquire { id }, now).unwrap()
    }

    fn rel(p: &mut Policy, id: Id, now: Ms) -> Vec<(Id, Effect)> {
        p.step(Input::Release { id }, now).unwrap()
    }

    fn tick(p: &mut Policy, mem: u64, now: Ms) -> Vec<(Id, Effect)> {
        p.step(Input::Tick { mem_available: mem }, now).unwrap()
    }

    fn reclaimed(p: &mut Policy, id: Id) -> Vec<(Id, Effect)> {
        p.step(Input::Reclaimed { id }, 0).unwrap()
    }

    #[test]
    fn acquire_release_cycle() {
        let mut p = policy();
        assert_eq!(acq(&mut p, A, 10), vec![(A, Effect::Weight(10_000))]);
        assert_eq!(state(&p, A), State::Acting);
        // A second acquire while acting does nothing.
        assert!(acq(&mut p, A, 11).is_empty());
        // The freeze comes first, so a failed weight cannot skip it.
        assert_eq!(
            rel(&mut p, A, 20),
            vec![(A, Effect::Freeze), (A, Effect::Weight(100))]
        );
        assert_eq!(state(&p, A), State::Waiting);
        // Releasing again is idempotent.
        assert!(rel(&mut p, A, 21).is_empty());
        assert_eq!(
            acq(&mut p, A, 30),
            vec![(A, Effect::Thaw), (A, Effect::Weight(10_000))]
        );
    }

    #[test]
    fn park_after_idle_timeout() {
        let mut p = policy();
        rel(&mut p, A, 100);
        assert!(tick(&mut p, 100 * GIB, 1_099).is_empty());
        assert_eq!(tick(&mut p, 100 * GIB, 1_100), vec![(A, Effect::Park)]);
        assert_eq!(state(&p, A), State::Parked);
        // Parked agents are not parked twice.
        assert!(tick(&mut p, 100 * GIB, 5_000).is_empty());
        // Acquire resumes a parked agent with a thaw.
        assert_eq!(
            acq(&mut p, A, 5_001),
            vec![(A, Effect::Thaw), (A, Effect::Weight(10_000))]
        );
    }

    #[test]
    fn memory_pressure_parks_longest_waiting_first() {
        let mut p = policy();
        rel(&mut p, C, 10);
        rel(&mut p, A, 20);
        rel(&mut p, B, 30);
        // Plenty of memory and nobody past park_after: nothing.
        assert!(tick(&mut p, 100 * GIB, 40).is_empty());
        // Low memory parks the batch of two, oldest first.
        assert_eq!(
            tick(&mut p, GIB, 50),
            vec![(C, Effect::Park), (A, Effect::Park)]
        );
        assert_eq!(state(&p, B), State::Waiting);
        assert_eq!(tick(&mut p, GIB, 60), vec![(B, Effect::Park)]);
    }

    #[test]
    fn reclaims_wait_for_a_slot_first_come_first() {
        let mut p = policy();
        p.cfg.park_jobs = 1;
        rel(&mut p, B, 10);
        rel(&mut p, A, 20);
        // Both are parked; only the first reclaims.
        assert_eq!(tick(&mut p, 100 * GIB, 2_000), vec![(B, Effect::Park)]);
        assert_eq!(state(&p, A), State::Parked);
        // The next starts as soon as the first ends.
        assert_eq!(reclaimed(&mut p, B), vec![(A, Effect::Park)]);
        // A stray report changes nothing.
        assert!(reclaimed(&mut p, B).is_empty());
    }

    #[test]
    fn an_agent_parked_again_mid_reclaim_is_reclaimed_again() {
        let mut p = policy();
        rel(&mut p, A, 0);
        assert_eq!(tick(&mut p, 100 * GIB, 1_000), vec![(A, Effect::Park)]);
        // The acquire stops that reclaim, which has not reported yet.
        acq(&mut p, A, 1_001);
        rel(&mut p, A, 1_002);
        assert!(tick(&mut p, 100 * GIB, 2_002).is_empty());
        assert_eq!(state(&p, A), State::Parked);
        assert_eq!(reclaimed(&mut p, A), vec![(A, Effect::Park)]);
    }

    #[test]
    fn acting_and_starting_agents_are_never_parked() {
        let mut p = policy();
        acq(&mut p, A, 0);
        assert!(tick(&mut p, 0, 1_000_000).is_empty());
        assert_eq!(state(&p, A), State::Acting);
        assert_eq!(state(&p, B), State::Starting);
    }

    #[test]
    fn evicted_agents_refuse_acquire() {
        let mut p = policy();
        p.step(Input::Lost { id: A }, 5).unwrap();
        assert_eq!(p.step(Input::Acquire { id: A }, 6), Err(Refusal::Evicted));
        assert_eq!(p.step(Input::Acquire { id: 99 }, 6), Err(Refusal::Unknown));
    }

    #[test]
    fn recovery_keeps_frozen_kiosks_frozen() {
        let mut p = Policy::new(Config::default());
        let found = |id, frozen, populated, saved| Input::Found {
            id,
            frozen,
            populated,
            saved,
        };
        let mut effects = Vec::new();
        for input in [
            found(1, true, true, Some(State::Parked)),
            found(2, true, true, Some(State::Waiting)),
            found(3, true, true, None),
            found(4, false, true, Some(State::Acting)),
            found(5, false, true, Some(State::Parked)),
            found(6, false, false, Some(State::Acting)),
        ] {
            effects.extend(p.step(input, 0).unwrap());
        }
        // Nothing is thawed or killed; the parked kiosk is reclaimed again.
        assert_eq!(effects, vec![(1, Effect::Park)]);
        assert_eq!(state(&p, 1), State::Parked);
        assert_eq!(state(&p, 2), State::Waiting);
        assert_eq!(state(&p, 3), State::Waiting);
        assert_eq!(state(&p, 4), State::Acting);
        // A kiosk saved as parked but found thawed was thawed by someone
        // else; trust the kernel.
        assert_eq!(state(&p, 5), State::Starting);
        assert_eq!(state(&p, 6), State::Evicted);
    }

    #[test]
    fn a_failure_is_corrected_from_what_cgroup_freeze_asks_for() {
        let mut p = policy();
        rel(&mut p, A, 0);
        // The freeze write failed: cgroup.freeze still asks for thawed.
        p.step(
            Input::Failed {
                id: A,
                frozen: false,
            },
            1,
        )
        .unwrap();
        assert_eq!(state(&p, A), State::Acting);
        // The thaw timed out after its write: thawed is what it asks for.
        rel(&mut p, A, 2);
        acq(&mut p, A, 3);
        p.step(
            Input::Failed {
                id: A,
                frozen: false,
            },
            4,
        )
        .unwrap();
        assert_eq!(state(&p, A), State::Acting);
        // The freeze timed out after its write: the kernel may still
        // finish it, so the next acquire must thaw.
        acq(&mut p, B, 5);
        rel(&mut p, B, 6);
        acq(&mut p, B, 7);
        p.step(
            Input::Failed {
                id: B,
                frozen: true,
            },
            8,
        )
        .unwrap();
        assert_eq!(state(&p, B), State::Waiting);
        assert_eq!(acq(&mut p, B, 9)[0], (B, Effect::Thaw));
    }

    #[test]
    fn state_names_round_trip() {
        for s in [
            State::Starting,
            State::Acting,
            State::Waiting,
            State::Parked,
            State::Evicted,
        ] {
            assert_eq!(State::from_name(s.name()), Some(s));
        }
    }

    /// The kernel side of one agent, as the effects leave it.
    #[derive(Debug, Clone, Copy, Default)]
    struct Kiosk {
        exists: bool,
        populated: bool,
        /// What cgroup.freeze asks for.
        frozen: bool,
        /// A Park started since the agent last entered Parked.
        reclaimed: bool,
    }

    /// Every sequence of up to 6 inputs in a world of 2 agents and 1
    /// reclaim slot, where memory is always low and no wait reaches
    /// park_after. After each step: the policy's state agrees with what
    /// its effects asked cgroup.freeze for, every parked agent is reclaimed
    /// or has a reclaim coming, and acquire refuses only unknown and
    /// evicted agents. Agent 1 also exits, restarts, and loses and regains
    /// its cgroup; agent 2 only acts and waits.
    #[test]
    fn exhaustive_up_to_6_inputs() {
        const IDS: [Id; 2] = [1, 2];

        fn inputs(k: &[Kiosk; 2]) -> Vec<Input> {
            let mut v = Vec::new();
            for id in IDS {
                v.push(Input::Acquire { id });
                v.push(Input::Release { id });
                v.push(Input::Reclaimed { id });
            }
            v.push(Input::Tick { mem_available: 0 });
            for frozen in [false, true] {
                v.push(Input::Failed { id: 1, frozen });
            }
            let one = k[0];
            if one.exists && one.populated {
                v.push(Input::Lost { id: 1 });
            }
            if one.exists && !one.populated {
                v.push(Input::Found {
                    id: 1,
                    frozen: false,
                    populated: true,
                    saved: None,
                });
            }
            if one.exists {
                v.push(Input::Gone { id: 1 });
            } else {
                v.push(Input::Found {
                    id: 1,
                    frozen: false,
                    populated: true,
                    saved: None,
                });
            }
            v
        }

        fn walk(p: &Policy, k: &[Kiosk; 2], depth: u32, now: Ms) {
            for (i, id) in IDS.into_iter().enumerate() {
                let kiosk = k[i];
                let Some(a) = p.agents().get(&id) else {
                    assert!(!kiosk.exists, "agent {id} forgotten while it exists");
                    continue;
                };
                assert!(kiosk.exists, "agent {id} known after its cgroup went");
                match a.state {
                    State::Starting | State::Acting => {
                        assert!(!kiosk.frozen, "agent {id} acts on a frozen kiosk");
                    }
                    State::Waiting => assert!(kiosk.frozen, "agent {id} waits thawed"),
                    State::Parked => {
                        assert!(kiosk.frozen, "agent {id} parked thawed");
                        assert!(
                            kiosk.reclaimed || matches!(a.job, Job::Queued | Job::Rerun),
                            "agent {id} parked with no reclaim coming"
                        );
                    }
                    State::Evicted => {}
                }
            }
            if depth == 0 {
                return;
            }
            for input in inputs(k) {
                let (mut p2, mut k2) = (p.clone(), *k);
                let slot = |id: Id| (id - 1) as usize;
                // The world changes first; the input reports it.
                match input {
                    Input::Lost { id } => k2[slot(id)].populated = false,
                    Input::Found { id, .. } => {
                        let kiosk = &mut k2[slot(id)];
                        *kiosk = Kiosk {
                            exists: true,
                            populated: true,
                            ..Kiosk::default()
                        };
                    }
                    Input::Gone { id } => k2[slot(id)] = Kiosk::default(),
                    Input::Failed { id, frozen } => k2[slot(id)].frozen = frozen,
                    _ => {}
                }
                let before = p2.state(match input {
                    Input::Acquire { id } => id,
                    _ => 0,
                });
                match p2.step(input.clone(), now) {
                    Ok(effects) => {
                        if let Input::Acquire { id } = input {
                            assert_eq!(p2.state(id), Some(State::Acting));
                        }
                        for (id, e) in effects {
                            let kiosk = &mut k2[slot(id)];
                            assert!(kiosk.exists, "effect {e:?} on gone agent {id}");
                            match e {
                                Effect::Thaw => kiosk.frozen = false,
                                Effect::Freeze => kiosk.frozen = true,
                                Effect::Park => {
                                    assert!(kiosk.frozen, "reclaim of thawed agent {id}");
                                    kiosk.reclaimed = true;
                                }
                                Effect::Weight(_) => {}
                            }
                        }
                    }
                    Err(r) => {
                        let want = match before {
                            None => Refusal::Unknown,
                            Some(State::Evicted) => Refusal::Evicted,
                            Some(s) => panic!("acquire refused in {s:?}"),
                        };
                        assert_eq!(r, want);
                    }
                }
                // Entering Parked needs a reclaim started after it.
                for id in IDS {
                    if p2.state(id) == Some(State::Parked) && p.state(id) != Some(State::Parked) {
                        let started = matches!(p2.agents()[&id].job, Job::Running)
                            && p.agents().get(&id).is_none_or(|a| a.job != Job::Running);
                        k2[slot(id)].reclaimed = started;
                    }
                }
                walk(&p2, &k2, depth - 1, now + 1);
            }
        }

        let mut p = Policy::new(Config {
            park_after: Ms::MAX,
            mem_low: 1,
            pressure_batch: 1,
            park_jobs: 1,
            ..Config::default()
        });
        let mut k = [Kiosk::default(); 2];
        for id in IDS {
            p.step(
                Input::Found {
                    id,
                    frozen: false,
                    populated: true,
                    saved: None,
                },
                0,
            )
            .unwrap();
            k[(id - 1) as usize] = Kiosk {
                exists: true,
                populated: true,
                ..Kiosk::default()
            };
        }
        walk(&p, &k, 6, 1);
    }
}
