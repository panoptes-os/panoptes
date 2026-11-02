//! Which VPN tunnel each agent's task runs on. A policy: no I/O, clock or
//! threads. See docs/design/vpn.md.
//!
//! ```text
//! Start ──> Assign to a tunnel, or wait
//! End   ──> free its place; Assign waiting agents that now fit
//! Down  ──> Park every agent on the tunnel
//! Up    ──> Resume them; Assign waiting agents that now fit
//! ```
//!
//! An agent keeps its tunnel for its whole task. Agents signed in to the
//! same account share a tunnel. A pin names the only tunnel a task may use.
//! Otherwise a task goes to the up tunnel with the fewest agents, below its
//! limit. The policy has no timers, so `step` takes no time.

/// Tunnel ids are indexes into the tunnel list; agent and account ids are
/// the caller's.
pub type TunnelId = usize;
pub type AgentId = u32;
pub type AccountId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// An agent starts a task.
    Start {
        agent: AgentId,
        account: Option<AccountId>,
        pin: Option<TunnelId>,
    },
    /// An agent's task ended, or was cancelled while it waited.
    End { agent: AgentId },
    /// A tunnel went down.
    Down { tunnel: TunnelId },
    /// A tunnel came back up.
    Up { tunnel: TunnelId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Start the agent's desk in this tunnel.
    Assign { agent: AgentId, tunnel: TunnelId },
    /// The agent's tunnel went down: park it, never move it.
    Park { agent: AgentId },
    /// The agent's tunnel is back: wake it.
    Resume { agent: AgentId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Task {
    agent: AgentId,
    account: Option<AccountId>,
    pin: Option<TunnelId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Tunnel {
    /// Most agents at once; `None` is no limit.
    limit: Option<usize>,
    up: bool,
    /// Agents on the tunnel, running or parked. Parked ones keep their place.
    agents: Vec<Task>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnels {
    tunnels: Vec<Tunnel>,
    /// Tasks with no tunnel yet, oldest first.
    waiting: Vec<Task>,
}

impl Tunnels {
    /// One tunnel per limit, all up.
    pub fn new(limits: &[Option<usize>]) -> Tunnels {
        let mut tunnels = Vec::new();
        for &limit in limits {
            tunnels.push(Tunnel {
                limit,
                up: true,
                agents: Vec::new(),
            });
        }
        Tunnels {
            tunnels,
            waiting: Vec::new(),
        }
    }

    /// Apply one input. The caller sends `Start` only for an agent with no
    /// task, and `End` only for one with a task, running or waiting.
    pub fn step(&mut self, input: Input) -> Vec<Effect> {
        let mut effects = Vec::new();
        match input {
            Input::Start {
                agent,
                account,
                pin,
            } => {
                assert!(
                    self.tunnel_of(agent).is_none(),
                    "agent {agent} already has a tunnel"
                );
                assert!(!self.is_waiting(agent), "agent {agent} already waits");
                if let Some(t) = pin {
                    assert!(t < self.tunnels.len(), "pin to unknown tunnel {t}");
                }
                let task = Task {
                    agent,
                    account,
                    pin,
                };
                match self.choose(&task) {
                    Some(t) => {
                        self.tunnels[t].agents.push(task);
                        effects.push(Effect::Assign { agent, tunnel: t });
                    }
                    None => self.waiting.push(task),
                }
            }
            Input::End { agent } => {
                if let Some(t) = self.tunnel_of(agent) {
                    let list = &mut self.tunnels[t].agents;
                    let mut i = 0;
                    while i < list.len() {
                        if list[i].agent == agent {
                            list.remove(i);
                        } else {
                            i += 1;
                        }
                    }
                    self.drain(&mut effects);
                } else {
                    assert!(self.is_waiting(agent), "agent {agent} ended with no task");
                    let mut i = 0;
                    while i < self.waiting.len() {
                        if self.waiting[i].agent == agent {
                            self.waiting.remove(i);
                        } else {
                            i += 1;
                        }
                    }
                }
            }
            Input::Down { tunnel } => {
                assert!(tunnel < self.tunnels.len(), "unknown tunnel {tunnel}");
                if self.tunnels[tunnel].up {
                    self.tunnels[tunnel].up = false;
                    for task in &self.tunnels[tunnel].agents {
                        effects.push(Effect::Park { agent: task.agent });
                    }
                }
            }
            Input::Up { tunnel } => {
                assert!(tunnel < self.tunnels.len(), "unknown tunnel {tunnel}");
                if !self.tunnels[tunnel].up {
                    self.tunnels[tunnel].up = true;
                    for task in &self.tunnels[tunnel].agents {
                        effects.push(Effect::Resume { agent: task.agent });
                    }
                    self.drain(&mut effects);
                }
            }
        }
        if cfg!(debug_assertions) {
            self.check();
        }
        effects
    }

    /// The agent's tunnel, if it has one.
    pub fn tunnel_of(&self, agent: AgentId) -> Option<TunnelId> {
        for (t, tunnel) in self.tunnels.iter().enumerate() {
            for task in &tunnel.agents {
                if task.agent == agent {
                    return Some(t);
                }
            }
        }
        None
    }

    pub fn is_waiting(&self, agent: AgentId) -> bool {
        for task in &self.waiting {
            if task.agent == agent {
                return true;
            }
        }
        false
    }

    /// Agents on tunnel `t`, running or parked.
    pub fn load(&self, t: TunnelId) -> usize {
        self.tunnels[t].agents.len()
    }

    /// The tunnel an account is signed in on, if any.
    fn account_tunnel(&self, account: AccountId) -> Option<TunnelId> {
        for (t, tunnel) in self.tunnels.iter().enumerate() {
            for task in &tunnel.agents {
                if task.account == Some(account) {
                    return Some(t);
                }
            }
        }
        None
    }

    fn has_room(&self, t: TunnelId) -> bool {
        let tunnel = &self.tunnels[t];
        match tunnel.limit {
            Some(limit) => tunnel.up && tunnel.agents.len() < limit,
            None => tunnel.up,
        }
    }

    /// Where a task may go now, or `None` if it must wait.
    fn choose(&self, task: &Task) -> Option<TunnelId> {
        // the account's tunnel, if it is signed in somewhere
        let forced = match task.account {
            Some(a) => self.account_tunnel(a),
            None => None,
        };
        let only = match (forced, task.pin) {
            (Some(f), Some(p)) if f != p => return None,
            (Some(f), _) => Some(f),
            (None, p) => p,
        };
        if let Some(t) = only {
            return if self.has_room(t) { Some(t) } else { None };
        }
        // least loaded, lowest id on a tie
        let mut best: Option<TunnelId> = None;
        for t in 0..self.tunnels.len() {
            if !self.has_room(t) {
                continue;
            }
            match best {
                Some(b) if self.load(b) <= self.load(t) => {}
                _ => best = Some(t),
            }
        }
        best
    }

    /// Assign every waiting task that fits, oldest first. A task that cannot
    /// go yet does not hold back the ones behind it.
    fn drain(&mut self, effects: &mut Vec<Effect>) {
        let mut i = 0;
        while i < self.waiting.len() {
            let task = self.waiting[i];
            match self.choose(&task) {
                Some(t) => {
                    self.waiting.remove(i);
                    self.tunnels[t].agents.push(task);
                    effects.push(Effect::Assign {
                        agent: task.agent,
                        tunnel: t,
                    });
                }
                None => i += 1,
            }
        }
    }

    /// Assert the invariants.
    pub fn check(&self) {
        let mut seen: Vec<AgentId> = Vec::new();
        for (t, tunnel) in self.tunnels.iter().enumerate() {
            if let Some(limit) = tunnel.limit {
                assert!(tunnel.agents.len() <= limit, "tunnel {t} over its limit");
            }
            for task in &tunnel.agents {
                assert!(!seen.contains(&task.agent), "agent {} twice", task.agent);
                seen.push(task.agent);
                if let Some(p) = task.pin {
                    assert!(p == t, "agent {} off its pin", task.agent);
                }
                if let Some(a) = task.account {
                    assert!(
                        self.account_tunnel(a) == Some(t),
                        "account {a} on two tunnels"
                    );
                }
            }
        }
        for task in &self.waiting {
            assert!(!seen.contains(&task.agent), "agent {} twice", task.agent);
            seen.push(task.agent);
            assert!(
                self.choose(task).is_none(),
                "agent {} waits with a place open",
                task.agent
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Effect::*;

    fn start(agent: AgentId) -> Input {
        Input::Start {
            agent,
            account: None,
            pin: None,
        }
    }

    fn start_with(agent: AgentId, account: Option<AccountId>, pin: Option<TunnelId>) -> Input {
        Input::Start {
            agent,
            account,
            pin,
        }
    }

    #[test]
    fn agents_spread_to_the_least_loaded_tunnel() {
        let mut t = Tunnels::new(&[None, None, None]);
        let mut got = Vec::new();
        for a in 0..6 {
            got.extend(t.step(start(a)));
        }
        assert_eq!(
            got,
            vec![
                Assign {
                    agent: 0,
                    tunnel: 0
                },
                Assign {
                    agent: 1,
                    tunnel: 1
                },
                Assign {
                    agent: 2,
                    tunnel: 2
                },
                Assign {
                    agent: 3,
                    tunnel: 0
                },
                Assign {
                    agent: 4,
                    tunnel: 1
                },
                Assign {
                    agent: 5,
                    tunnel: 2
                },
            ]
        );
    }

    #[test]
    fn a_freed_place_takes_the_next_agent() {
        let mut t = Tunnels::new(&[None, None]);
        t.step(start(0));
        t.step(start(1));
        t.step(start(2));
        t.step(Input::End { agent: 1 });
        assert_eq!(
            t.step(start(3)),
            vec![Assign {
                agent: 3,
                tunnel: 1
            }]
        );
    }

    #[test]
    fn a_full_tunnel_takes_no_more() {
        let mut t = Tunnels::new(&[Some(1), Some(1)]);
        t.step(start(0));
        t.step(start(1));
        assert_eq!(t.step(start(2)), vec![]);
        assert!(t.is_waiting(2));
    }

    #[test]
    fn an_end_lets_the_oldest_waiting_agent_in() {
        let mut t = Tunnels::new(&[Some(1)]);
        t.step(start(0));
        t.step(start(1));
        t.step(start(2));
        assert_eq!(
            t.step(Input::End { agent: 0 }),
            vec![Assign {
                agent: 1,
                tunnel: 0
            }]
        );
        assert!(t.is_waiting(2));
    }

    #[test]
    fn a_pinned_task_goes_only_to_its_tunnel() {
        let mut t = Tunnels::new(&[None, None]);
        assert_eq!(
            t.step(start_with(0, None, Some(1))),
            vec![Assign {
                agent: 0,
                tunnel: 1
            }]
        );
        assert_eq!(
            t.step(start_with(1, None, Some(1))),
            vec![Assign {
                agent: 1,
                tunnel: 1
            }]
        );
    }

    #[test]
    fn a_pinned_task_waits_for_its_full_tunnel() {
        let mut t = Tunnels::new(&[None, Some(1)]);
        t.step(start_with(0, None, Some(1)));
        assert_eq!(t.step(start_with(1, None, Some(1))), vec![]);
        assert_eq!(
            t.step(Input::End { agent: 0 }),
            vec![Assign {
                agent: 1,
                tunnel: 1
            }]
        );
    }

    #[test]
    fn a_waiting_pin_does_not_hold_back_others() {
        let mut t = Tunnels::new(&[None, Some(1)]);
        t.step(start_with(0, None, Some(1)));
        t.step(start_with(1, None, Some(1)));
        assert_eq!(
            t.step(start(2)),
            vec![Assign {
                agent: 2,
                tunnel: 0
            }]
        );
    }

    #[test]
    fn one_account_shares_one_tunnel() {
        let mut t = Tunnels::new(&[None, None]);
        t.step(start_with(0, Some(7), None));
        assert_eq!(
            t.step(start_with(1, Some(7), None)),
            vec![Assign {
                agent: 1,
                tunnel: 0
            }]
        );
    }

    #[test]
    fn an_account_waits_for_its_full_tunnel() {
        let mut t = Tunnels::new(&[Some(1), None]);
        t.step(start_with(0, Some(7), None));
        assert_eq!(t.step(start_with(1, Some(7), None)), vec![]);
        assert!(t.is_waiting(1));
    }

    #[test]
    fn a_pin_off_the_accounts_tunnel_waits() {
        let mut t = Tunnels::new(&[None, None]);
        t.step(start_with(0, Some(7), Some(0)));
        assert_eq!(t.step(start_with(1, Some(7), Some(1))), vec![]);
        assert_eq!(
            t.step(Input::End { agent: 0 }),
            vec![Assign {
                agent: 1,
                tunnel: 1
            }]
        );
    }

    #[test]
    fn a_down_tunnel_parks_its_agents_and_takes_none() {
        let mut t = Tunnels::new(&[None, None]);
        t.step(start(0));
        t.step(start(1));
        assert_eq!(t.step(Input::Down { tunnel: 0 }), vec![Park { agent: 0 }]);
        assert_eq!(t.tunnel_of(0), Some(0));
        assert_eq!(
            t.step(start(2)),
            vec![Assign {
                agent: 2,
                tunnel: 1
            }]
        );
    }

    #[test]
    fn an_up_tunnel_resumes_its_agents_then_takes_waiters() {
        let mut t = Tunnels::new(&[None]);
        t.step(start(0));
        t.step(Input::Down { tunnel: 0 });
        t.step(start(1));
        assert_eq!(
            t.step(Input::Up { tunnel: 0 }),
            vec![
                Resume { agent: 0 },
                Assign {
                    agent: 1,
                    tunnel: 0
                }
            ]
        );
    }

    #[test]
    fn a_parked_agent_keeps_its_place() {
        let mut t = Tunnels::new(&[Some(1), None]);
        t.step(start(0));
        t.step(Input::Down { tunnel: 0 });
        t.step(Input::Up { tunnel: 0 });
        assert_eq!(t.load(0), 1);
        assert_eq!(
            t.step(start(1)),
            vec![Assign {
                agent: 1,
                tunnel: 1
            }]
        );
    }

    #[test]
    fn repeated_down_or_up_does_nothing() {
        let mut t = Tunnels::new(&[None]);
        t.step(start(0));
        t.step(Input::Down { tunnel: 0 });
        assert_eq!(t.step(Input::Down { tunnel: 0 }), vec![]);
        t.step(Input::Up { tunnel: 0 });
        assert_eq!(t.step(Input::Up { tunnel: 0 }), vec![]);
    }

    #[test]
    fn a_waiting_agent_can_be_cancelled() {
        let mut t = Tunnels::new(&[Some(0)]);
        t.step(start(0));
        assert_eq!(t.step(Input::End { agent: 0 }), vec![]);
        assert!(!t.is_waiting(0));
    }

    #[test]
    fn all_tunnels_down_means_wait() {
        let mut t = Tunnels::new(&[None, None]);
        t.step(Input::Down { tunnel: 0 });
        t.step(Input::Down { tunnel: 1 });
        assert_eq!(t.step(start(0)), vec![]);
        assert_eq!(
            t.step(Input::Up { tunnel: 1 }),
            vec![Assign {
                agent: 0,
                tunnel: 1
            }]
        );
    }

    /// Every sequence of up to 5 inputs over 2 tunnels and 3 agents, with
    /// one account and tunnel limits of 1 or none. `Start` goes only to an
    /// agent with no task and `End` only to one with a task, as the loop
    /// sends them; `Down` goes only to an up tunnel and `Up` to a down one.
    /// After every step:
    /// `check` holds, an agent never changes tunnel during its task, an
    /// assign lands on an up tunnel the task may use, parks come only from
    /// `Down` and resumes only from `Up`, each for the agents on that
    /// tunnel.
    #[test]
    fn exhaustive_up_to_5_inputs() {
        const AGENTS: AgentId = 3;
        const TUNNELS: usize = 2;

        fn inputs(t: &Tunnels) -> Vec<Input> {
            let mut out = Vec::new();
            for agent in 0..AGENTS {
                if t.tunnel_of(agent).is_some() || t.is_waiting(agent) {
                    out.push(Input::End { agent });
                } else {
                    for account in [None, Some(0)] {
                        for pin in [None, Some(0), Some(1)] {
                            out.push(Input::Start {
                                agent,
                                account,
                                pin,
                            });
                        }
                    }
                }
            }
            // a repeated down or up is a no-op, see repeated_down_or_up_does_nothing
            for tunnel in 0..TUNNELS {
                if t.tunnels[tunnel].up {
                    out.push(Input::Down { tunnel });
                } else {
                    out.push(Input::Up { tunnel });
                }
            }
            out
        }

        fn walk(t: &Tunnels, depth: u32) {
            if depth == 0 {
                return;
            }
            for input in inputs(t) {
                let mut next = t.clone();
                let effects = next.step(input);
                next.check();
                for e in &effects {
                    match *e {
                        Assign { agent, tunnel } => {
                            assert_eq!(t.tunnel_of(agent), None, "agent {agent} moved");
                            assert!(next.tunnels[tunnel].up, "assign to a down tunnel");
                            assert_eq!(next.tunnel_of(agent), Some(tunnel));
                            if let Input::Start { pin: Some(p), .. } = input
                                && agent_of(input) == agent
                            {
                                assert_eq!(p, tunnel, "assign off the pin");
                            }
                        }
                        Park { agent } => {
                            let Input::Down { tunnel } = input else {
                                panic!("park from {input:?}");
                            };
                            assert_eq!(t.tunnel_of(agent), Some(tunnel));
                        }
                        Resume { agent } => {
                            let Input::Up { tunnel } = input else {
                                panic!("resume from {input:?}");
                            };
                            assert_eq!(t.tunnel_of(agent), Some(tunnel));
                        }
                    }
                }
                // agents that keep a task keep their tunnel
                for agent in 0..AGENTS {
                    if let (Some(a), Some(b)) = (t.tunnel_of(agent), next.tunnel_of(agent)) {
                        assert_eq!(a, b, "agent {agent} moved");
                    }
                }
                walk(&next, depth - 1);
            }
        }

        fn agent_of(input: Input) -> AgentId {
            match input {
                Input::Start { agent, .. } | Input::End { agent } => agent,
                _ => AgentId::MAX,
            }
        }

        // (none, 1) mirrors (1, none)
        for limits in [[Some(1), Some(1)], [Some(1), None], [None, None]] {
            walk(&Tunnels::new(&limits), 5);
        }
    }
}
