//! When the screen has settled after an action. A policy: no I/O, clock or
//! threads.
//!
//! The desk waits for frames that change the screen, one window at a time:
//!
//! ```text
//! Wait(w) ──> a change within w ──> Wait again
//!         └─> none for a full quiet window ──> Settled
//! limit passes first ──> TimedOut
//! ```

/// Milliseconds on the caller's clock.
pub type Ms = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Wait at most this long for a frame that changes the screen.
    Wait(Ms),
    /// No change for a full quiet window: the last changed frame is what
    /// the screen shows.
    Settled,
    /// The limit passed first.
    TimedOut,
}

/// What the last wait saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saw {
    Change,
    /// Nothing changed during a wait of this length.
    Quiet(Ms),
}

#[derive(Debug, Clone)]
pub struct Settle {
    quiet: Ms,
    limit: Ms,
    start: Ms,
}

impl Settle {
    pub fn new(quiet: Ms, limit: Ms, now: Ms) -> Settle {
        Settle {
            quiet,
            limit,
            start: now,
        }
    }

    /// What to do at `now`, given what the last wait saw; `None` before the
    /// first wait.
    pub fn next(&self, saw: Option<Saw>, now: Ms) -> Next {
        // A shorter wait, cut by the limit, does not count as quiet.
        if saw == Some(Saw::Quiet(self.quiet)) {
            return Next::Settled;
        }
        let spent = now.saturating_sub(self.start);
        if spent >= self.limit {
            return Next::TimedOut;
        }
        Next::Wait(self.quiet.min(self.limit - spent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_window_settles() {
        let s = Settle::new(300, 3000, 0);
        assert_eq!(s.next(None, 0), Next::Wait(300));
        assert_eq!(s.next(Some(Saw::Change), 120), Next::Wait(300));
        assert_eq!(s.next(Some(Saw::Quiet(300)), 420), Next::Settled);
    }

    #[test]
    fn the_last_window_is_cut_by_the_limit_and_does_not_settle() {
        let s = Settle::new(300, 1000, 0);
        assert_eq!(s.next(Some(Saw::Change), 800), Next::Wait(200));
        assert_eq!(s.next(Some(Saw::Quiet(200)), 1000), Next::TimedOut);
    }

    #[test]
    fn a_limit_below_quiet_times_out() {
        let s = Settle::new(300, 100, 0);
        assert_eq!(s.next(None, 0), Next::Wait(100));
        assert_eq!(s.next(Some(Saw::Quiet(100)), 100), Next::TimedOut);
    }

    /// Every run of up to 6 waits with quiet and limit from 0 to 3 ms,
    /// where each wait sees a change after 0 to its full length, or nothing
    /// for its full length, as the desk's waits do. Each wait is at most
    /// quiet long, the run ends within the limit plus one wait, and it
    /// settles only after a full quiet window.
    #[test]
    fn exhaustive_up_to_6_waits() {
        fn walk(s: &Settle, saw: Option<Saw>, now: Ms, depth: u32) {
            match s.next(saw, now) {
                Next::Settled => assert_eq!(saw, Some(Saw::Quiet(s.quiet))),
                Next::TimedOut => assert!(now >= s.limit),
                Next::Wait(w) => {
                    assert!(w <= s.quiet && now + w <= s.limit && w > 0 || s.quiet == 0);
                    if depth == 0 {
                        return;
                    }
                    for after in 0..=w {
                        walk(s, Some(Saw::Change), now + after, depth - 1);
                    }
                    walk(s, Some(Saw::Quiet(w)), now + w, depth - 1);
                }
            }
        }
        for quiet in 0..=3 {
            for limit in 0..=3 {
                walk(&Settle::new(quiet, limit, 0), None, 0, 6);
            }
        }
    }
}
