//! Residency policy: a pure state machine from events and a clock to
//! actions. It does no I/O; the daemon applies the actions it returns and
//! reports what it observes back as events.
//!
//! ```text
//! starting ─release─> waiting ─park_after or memory low─> parked
//!    │                  ^  │                                 │
//!    └─acquire─> acting ┘  └──────────acquire──────────┬─────┘
//!                  ^                                    │
//!                  └──────────────(thaw)────────────────┘
//! evicted: the kiosk has no processes; acquire fails until it runs again.
//! Below mem_critical MemAvailable or swap_low SwapFree, the longest-parked
//! kiosks are killed into evicted, since a full zswap pool cannot be freed
//! with writeback off.
//! ```

use std::collections::BTreeMap;

/// Milliseconds on the daemon's monotonic clock.
pub type Ms = u64;

/// An agent's residency, with the time it entered that state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Running, never handed to the policy by its desk.
    Starting,
    /// Thawed and weighted up while its agent acts.
    Acting,
    /// Frozen while its agent waits.
    Waiting,
    /// Frozen and reclaimed into zswap.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Agent {
    pub state: State,
    pub since: Ms,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Discovery found a kiosk. `frozen` and `populated` come from its
    /// `cgroup.events`; `saved` is the state file's entry, if any.
    Found {
        agent: String,
        frozen: bool,
        populated: bool,
        saved: Option<State>,
    },
    /// The kiosk's processes are gone, or its cgroup is.
    Lost {
        agent: String,
    },
    Acquire {
        agent: String,
    },
    Release {
        agent: String,
    },
    /// Periodic check, with the host's MemAvailable and SwapFree in bytes.
    Tick {
        mem_available: u64,
        swap_free: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Thaw(String),
    Freeze(String),
    /// Reclaim a frozen kiosk into zswap with writeback off.
    Park(String),
    Weight(String, u16),
    /// Kill every process in the kiosk; its profile stays on disk.
    Evict(String),
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Waiting this long parks an agent.
    pub park_after: Ms,
    /// Below this MemAvailable, park the longest-waiting agents early.
    pub mem_low: u64,
    /// Agents parked early per tick while memory is low.
    pub pressure_batch: usize,
    /// Below this MemAvailable, evict the longest-parked agents.
    pub mem_critical: u64,
    /// Below this SwapFree, evict too: a park needs a swap slot for each
    /// page it compresses, and without one the page stays resident.
    pub swap_low: u64,
    /// Agents evicted per tick while memory or swap is critical.
    pub evict_batch: usize,
    pub weight_acting: u16,
    pub weight_idle: u16,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            park_after: 30_000,
            mem_low: 4 << 30,
            pressure_batch: 8,
            mem_critical: 2 << 30,
            swap_low: 4 << 30,
            evict_batch: 4,
            weight_acting: 10_000,
            weight_idle: 100,
        }
    }
}

/// Why an acquire cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unknown,
    Evicted,
}

#[derive(Debug)]
pub struct Policy {
    pub cfg: Config,
    agents: BTreeMap<String, Agent>,
}

impl Policy {
    pub fn new(cfg: Config) -> Policy {
        Policy {
            cfg,
            agents: BTreeMap::new(),
        }
    }

    pub fn agents(&self) -> &BTreeMap<String, Agent> {
        &self.agents
    }

    fn set(&mut self, agent: &str, state: State, now: Ms) {
        if let Some(a) = self.agents.get_mut(agent) {
            if a.state != state {
                *a = Agent { state, since: now };
            }
        }
    }

    /// Apply one event at time `now`. Acquire returns `Err` when the agent
    /// cannot act; every other event succeeds.
    pub fn handle(&mut self, ev: Event, now: Ms) -> Result<Vec<Action>, Refusal> {
        let mut out = Vec::new();
        match ev {
            Event::Found {
                agent,
                frozen,
                populated,
                saved,
            } => {
                if let Some(a) = self.agents.get(&agent) {
                    // A restarted kiosk comes back as a new start.
                    if a.state == State::Evicted && populated && !frozen {
                        self.set(&agent, State::Starting, now);
                    }
                    return Ok(out);
                }
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
                self.agents.insert(agent, Agent { state, since: now });
            }
            Event::Lost { agent } => self.set(&agent, State::Evicted, now),
            Event::Acquire { agent } => {
                let a = *self.agents.get(&agent).ok_or(Refusal::Unknown)?;
                match a.state {
                    State::Evicted => return Err(Refusal::Evicted),
                    State::Acting => {}
                    State::Starting => {
                        out.push(Action::Weight(agent.clone(), self.cfg.weight_acting));
                    }
                    State::Waiting | State::Parked => {
                        out.push(Action::Thaw(agent.clone()));
                        out.push(Action::Weight(agent.clone(), self.cfg.weight_acting));
                    }
                }
                self.set(&agent, State::Acting, now);
            }
            Event::Release { agent } => {
                let Some(a) = self.agents.get(&agent).copied() else {
                    return Ok(out);
                };
                if matches!(a.state, State::Starting | State::Acting) {
                    out.push(Action::Weight(agent.clone(), self.cfg.weight_idle));
                    out.push(Action::Freeze(agent.clone()));
                    self.set(&agent, State::Waiting, now);
                }
            }
            Event::Tick {
                mem_available,
                swap_free,
            } => {
                if mem_available < self.cfg.mem_critical || swap_free < self.cfg.swap_low {
                    let mut parked: Vec<(Ms, String)> = self
                        .agents
                        .iter()
                        .filter(|(_, a)| a.state == State::Parked)
                        .map(|(n, a)| (a.since, n.clone()))
                        .collect();
                    parked.sort();
                    for (_, n) in parked.into_iter().take(self.cfg.evict_batch) {
                        out.push(Action::Evict(n.clone()));
                        self.set(&n, State::Evicted, now);
                    }
                }
                let mut waiting: Vec<(Ms, String)> = self
                    .agents
                    .iter()
                    .filter(|(_, a)| a.state == State::Waiting)
                    .map(|(n, a)| (a.since, n.clone()))
                    .collect();
                waiting.sort();
                let mut park: Vec<String> = waiting
                    .iter()
                    .filter(|(since, _)| now.saturating_sub(*since) >= self.cfg.park_after)
                    .map(|(_, n)| n.clone())
                    .collect();
                if mem_available < self.cfg.mem_low {
                    for (_, n) in waiting.iter().take(self.cfg.pressure_batch) {
                        if !park.contains(n) {
                            park.push(n.clone());
                        }
                    }
                }
                for n in park {
                    out.push(Action::Park(n.clone()));
                    self.set(&n, State::Parked, now);
                }
            }
        }
        Ok(out)
    }

    /// Drop an agent whose cgroup no longer exists.
    pub fn forget(&mut self, agent: &str) {
        self.agents.remove(agent);
    }

    /// Correct the model after an action failed: the daemon reread the
    /// kiosk and found it frozen or not.
    pub fn observed(&mut self, agent: &str, frozen: bool, now: Ms) {
        let Some(a) = self.agents.get(agent).copied() else {
            return;
        };
        let state = match (a.state, frozen) {
            (State::Waiting | State::Parked, false) => State::Acting,
            (State::Acting | State::Starting, true) => State::Waiting,
            (s, _) => s,
        };
        self.set(agent, state, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn policy() -> Policy {
        let mut p = Policy::new(Config {
            park_after: 1_000,
            mem_low: 2 * GIB,
            pressure_batch: 2,
            mem_critical: GIB,
            swap_low: GIB,
            evict_batch: 1,
            ..Config::default()
        });
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            p.handle(
                Event::Found {
                    agent: (*name).into(),
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

    fn state(p: &Policy, n: &str) -> State {
        p.agents()[n].state
    }

    fn acq(p: &mut Policy, n: &str, now: Ms) -> Vec<Action> {
        p.handle(Event::Acquire { agent: n.into() }, now).unwrap()
    }

    fn rel(p: &mut Policy, n: &str, now: Ms) -> Vec<Action> {
        p.handle(Event::Release { agent: n.into() }, now).unwrap()
    }

    fn tick(p: &mut Policy, mem: u64, now: Ms) -> Vec<Action> {
        tick_swap(p, mem, 100 * GIB, now)
    }

    fn tick_swap(p: &mut Policy, mem: u64, swap: u64, now: Ms) -> Vec<Action> {
        p.handle(
            Event::Tick {
                mem_available: mem,
                swap_free: swap,
            },
            now,
        )
        .unwrap()
    }

    #[test]
    fn acquire_release_cycle() {
        let mut p = policy();
        assert_eq!(
            acq(&mut p, "a", 10),
            vec![Action::Weight("a".into(), 10_000)]
        );
        assert_eq!(state(&p, "a"), State::Acting);
        // A second acquire while acting does nothing.
        assert!(acq(&mut p, "a", 11).is_empty());
        assert_eq!(
            rel(&mut p, "a", 20),
            vec![Action::Weight("a".into(), 100), Action::Freeze("a".into())]
        );
        assert_eq!(state(&p, "a"), State::Waiting);
        // Releasing again is idempotent.
        assert!(rel(&mut p, "a", 21).is_empty());
        assert_eq!(
            acq(&mut p, "a", 30),
            vec![Action::Thaw("a".into()), Action::Weight("a".into(), 10_000)]
        );
    }

    #[test]
    fn park_after_idle_timeout() {
        let mut p = policy();
        rel(&mut p, "a", 100);
        assert!(tick(&mut p, 100 * GIB, 1_099).is_empty());
        assert_eq!(
            tick(&mut p, 100 * GIB, 1_100),
            vec![Action::Park("a".into())]
        );
        assert_eq!(state(&p, "a"), State::Parked);
        // Parked agents are not parked twice.
        assert!(tick(&mut p, 100 * GIB, 5_000).is_empty());
        // Acquire resumes a parked agent with a thaw.
        assert_eq!(
            acq(&mut p, "a", 5_001),
            vec![Action::Thaw("a".into()), Action::Weight("a".into(), 10_000)]
        );
    }

    #[test]
    fn memory_pressure_parks_longest_waiting_first() {
        let mut p = policy();
        rel(&mut p, "c", 10);
        rel(&mut p, "a", 20);
        rel(&mut p, "b", 30);
        // Plenty of memory and nobody past park_after: nothing.
        assert!(tick(&mut p, 100 * GIB, 40).is_empty());
        // Low memory parks the batch of two, oldest first.
        assert_eq!(
            tick(&mut p, GIB, 50),
            vec![Action::Park("c".into()), Action::Park("a".into())]
        );
        assert_eq!(state(&p, "b"), State::Waiting);
        assert_eq!(tick(&mut p, GIB, 60), vec![Action::Park("b".into())]);
    }

    #[test]
    fn critical_memory_evicts_longest_parked_first() {
        let mut p = policy();
        rel(&mut p, "b", 10);
        rel(&mut p, "a", 20);
        tick(&mut p, 100 * GIB, 1_010);
        tick(&mut p, 100 * GIB, 1_020);
        rel(&mut p, "c", 1_030);
        // Above the floors nothing is evicted.
        assert!(tick(&mut p, 100 * GIB, 1_040).is_empty());
        // Below mem_critical: one per tick, oldest park first; the waiting
        // agent is parked early, not killed.
        assert_eq!(
            tick(&mut p, GIB / 2, 1_050),
            vec![Action::Evict("b".into()), Action::Park("c".into())]
        );
        assert_eq!(state(&p, "b"), State::Evicted);
        // Low swap alone also evicts.
        assert_eq!(
            tick_swap(&mut p, 100 * GIB, GIB / 2, 1_060),
            vec![Action::Evict("a".into())]
        );
        assert_eq!(
            p.handle(Event::Acquire { agent: "a".into() }, 1_070),
            Err(Refusal::Evicted)
        );
    }

    #[test]
    fn acting_and_starting_agents_are_never_parked() {
        let mut p = policy();
        acq(&mut p, "a", 0);
        assert!(tick(&mut p, 0, 1_000_000).is_empty());
        assert_eq!(state(&p, "a"), State::Acting);
        assert_eq!(state(&p, "b"), State::Starting);
    }

    #[test]
    fn evicted_agents_refuse_acquire() {
        let mut p = policy();
        p.handle(Event::Lost { agent: "a".into() }, 5).unwrap();
        assert_eq!(
            p.handle(Event::Acquire { agent: "a".into() }, 6),
            Err(Refusal::Evicted)
        );
        assert_eq!(
            p.handle(Event::Acquire { agent: "zz".into() }, 6),
            Err(Refusal::Unknown)
        );
    }

    #[test]
    fn recovery_keeps_frozen_kiosks_frozen() {
        let mut p = Policy::new(Config::default());
        let found = |agent: &str, frozen, populated, saved| Event::Found {
            agent: agent.into(),
            frozen,
            populated,
            saved,
        };
        for ev in [
            found("parked", true, true, Some(State::Parked)),
            found("waiting", true, true, Some(State::Waiting)),
            found("nosave", true, true, None),
            found("acting", false, true, Some(State::Acting)),
            found("stale", false, true, Some(State::Parked)),
            found("gone", false, false, Some(State::Acting)),
        ] {
            // Recovery emits no actions: nothing is thawed or killed.
            assert!(p.handle(ev, 0).unwrap().is_empty());
        }
        assert_eq!(state(&p, "parked"), State::Parked);
        assert_eq!(state(&p, "waiting"), State::Waiting);
        assert_eq!(state(&p, "nosave"), State::Waiting);
        assert_eq!(state(&p, "acting"), State::Acting);
        // A kiosk saved as parked but found thawed was thawed by someone
        // else; trust the kernel.
        assert_eq!(state(&p, "stale"), State::Starting);
        assert_eq!(state(&p, "gone"), State::Evicted);
    }

    #[test]
    fn observed_corrects_failed_actions() {
        let mut p = policy();
        rel(&mut p, "a", 0);
        // The freeze failed: the kernel says it is thawed.
        p.observed("a", false, 1);
        assert_eq!(state(&p, "a"), State::Acting);
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
}
