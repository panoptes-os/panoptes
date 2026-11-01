//! When the desk holds its kiosk. A policy: no I/O, clock or threads.
//!
//! With panoptes-resd, the kiosk is frozen unless its desk holds it. The
//! desk acquires the kiosk before a call, and releases it once no call has
//! run for `grace`:
//!
//! ```text
//! Begin   ──> Acquire, unless held
//! Refused ──> undo that Begin; nothing to release
//! End     ──> idle from now
//! Tick    ──> Release, once idle for grace
//! ```

/// Milliseconds on the caller's clock.
pub type Ms = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A call starts.
    Begin,
    /// resd refused the acquire that `Begin` asked for. The call never ran.
    Refused,
    /// A call ended.
    End,
    /// The deadline came, or time passed.
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Acquire,
    Release,
}

#[derive(Debug, Clone)]
pub struct Lease {
    grace: Ms,
    /// Calls in flight.
    calls: u32,
    /// The kiosk is acquired or being acquired, or the desk thawed it itself.
    held: bool,
    /// When the last call ended, while no call runs.
    idle_since: Option<Ms>,
}

impl Lease {
    pub fn new(grace: Ms) -> Lease {
        Lease {
            grace,
            calls: 0,
            held: false,
            idle_since: None,
        }
    }

    /// Apply one input at `now`. Never returns `Release` while a call is in
    /// flight. The caller sends `Refused` right after a `Begin` whose
    /// `Acquire` was refused, and `End` only after any other `Begin`.
    pub fn step(&mut self, input: Input, now: Ms) -> Option<Effect> {
        let effect = match input {
            Input::Begin => {
                self.calls += 1;
                self.idle_since = None;
                if self.held {
                    None
                } else {
                    self.held = true;
                    Some(Effect::Acquire)
                }
            }
            Input::Refused => {
                assert!(self.calls == 1, "a refusal with no acquire to refuse");
                self.calls -= 1;
                self.held = false;
                None
            }
            Input::End => {
                assert!(self.calls > 0, "a call ended that never began");
                self.calls -= 1;
                if self.calls == 0 {
                    self.idle_since = Some(now);
                }
                None
            }
            Input::Tick => match self.deadline() {
                Some(at) if now >= at => {
                    self.held = false;
                    self.idle_since = None;
                    Some(Effect::Release)
                }
                _ => None,
            },
        };
        if cfg!(debug_assertions) {
            self.check();
        }
        effect
    }

    /// When the next `Tick` is due: `grace` after the last call ended,
    /// while the kiosk is held.
    pub fn deadline(&self) -> Option<Ms> {
        match (self.held, self.idle_since) {
            (true, Some(t)) => Some(t.saturating_add(self.grace)),
            _ => None,
        }
    }

    /// Assert the invariants.
    pub fn check(&self) {
        assert!(
            self.calls == 0 || self.held,
            "a call runs on a released kiosk"
        );
        assert!(
            self.idle_since.is_none() || self.calls == 0,
            "idle while a call runs"
        );
        assert!(
            self.held || self.idle_since.is_none(),
            "a released kiosk counts as idle"
        );
        assert!(
            !self.held || self.calls != 0 || self.idle_since.is_some(),
            "a held kiosk with no call and no timer"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Effect::*;
    use Input::*;

    #[test]
    fn the_first_call_acquires() {
        let mut l = Lease::new(100);
        assert_eq!(l.step(Begin, 0), Some(Acquire));
        assert_eq!(l.step(End, 5), None);
        assert_eq!(l.step(Begin, 10), None);
    }

    #[test]
    fn release_waits_for_the_grace_after_the_last_call() {
        let mut l = Lease::new(100);
        l.step(Begin, 0);
        l.step(End, 50);
        assert_eq!(l.deadline(), Some(150));
        assert_eq!(l.step(Tick, 149), None);
        assert_eq!(l.step(Tick, 150), Some(Release));
        assert_eq!(l.deadline(), None);
        assert_eq!(l.step(Begin, 200), Some(Acquire));
    }

    #[test]
    fn a_call_in_the_grace_keeps_the_kiosk() {
        let mut l = Lease::new(100);
        l.step(Begin, 0);
        l.step(End, 10);
        assert_eq!(l.step(Begin, 60), None);
        assert_eq!(l.step(Tick, 500), None);
        l.step(End, 600);
        assert_eq!(l.step(Tick, 700), Some(Release));
    }

    #[test]
    fn overlapping_calls_release_after_the_last() {
        // The 2026-10-31 race: B begins while A runs; A ends; the timer
        // fires while B still runs.
        let mut l = Lease::new(0);
        assert_eq!(l.step(Begin, 0), Some(Acquire));
        assert_eq!(l.step(Begin, 1), None);
        l.step(End, 2);
        assert_eq!(l.step(Tick, 1000), None);
        l.step(End, 1001);
        assert_eq!(l.step(Tick, 1001), Some(Release));
    }

    #[test]
    fn a_refused_call_leaves_nothing_to_release() {
        let mut l = Lease::new(100);
        assert_eq!(l.step(Begin, 0), Some(Acquire));
        assert_eq!(l.step(Refused, 1), None);
        assert_eq!(l.deadline(), None);
        assert_eq!(l.step(Tick, 1000), None);
    }

    #[test]
    fn the_call_after_a_refusal_acquires_again() {
        let mut l = Lease::new(100);
        l.step(Begin, 0);
        l.step(Refused, 1);
        assert_eq!(l.step(Begin, 2), Some(Acquire));
        l.step(End, 3);
        assert_eq!(l.step(Tick, 103), Some(Release));
    }

    /// Every sequence of up to 7 inputs, each at a time step of 0, 1 or 2
    /// ms, with grace 0 and 1. `End` comes only after a `Begin`, and
    /// `Refused` only right after a `Begin` that returned `Acquire`, as the
    /// desk thread sends them. After every step: the invariants hold, no
    /// release lands during a call, acquire and release alternate with a
    /// refused acquire counting as neither, and a release comes only once
    /// the grace has passed.
    #[test]
    fn exhaustive_up_to_7_inputs() {
        fn walk(
            l: &Lease,
            now: Ms,
            last_end: Option<Ms>,
            held: bool,
            acquiring: bool,
            depth: u32,
            grace: Ms,
        ) {
            if depth == 0 {
                return;
            }
            for input in [Begin, Refused, End, Tick] {
                if input == End && l.calls == 0 {
                    continue;
                }
                if input == Refused && !acquiring {
                    continue;
                }
                for dt in 0..3 {
                    let now = now + dt;
                    let mut next = l.clone();
                    let calls = l.calls;
                    let effect = next.step(input, now);
                    next.check();
                    match effect {
                        Some(Acquire) => assert!(!held && input == Begin),
                        Some(Release) => {
                            assert!(held && calls == 0 && input == Tick);
                            assert!(now >= last_end.expect("a call ended") + grace);
                        }
                        None => {}
                    }
                    let held = match effect {
                        Some(Acquire) => true,
                        Some(Release) => false,
                        None => held && input != Refused,
                    };
                    assert_eq!(held, next.held);
                    let last_end = if input == End { Some(now) } else { last_end };
                    let acquiring = effect == Some(Acquire);
                    walk(&next, now, last_end, held, acquiring, depth - 1, grace);
                }
            }
        }
        for grace in [0, 1] {
            walk(&Lease::new(grace), 0, None, false, false, 7, grace);
        }
    }
}
