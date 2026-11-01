//! The desk thread. It owns the desktop and runs one tool call at a time,
//! holding the kiosk from panoptes-resd while calls run. The threads layer:
//! the call channel and the lease's timer live here.
//!
//! ```text
//! edge ── Call ──> calls ──> desk thread ── Begin ─> Lease ── Acquire ─> resd
//!   ^                          │ Tools::call, Tools::after
//!   └──────── reply ───────────┤ End ─> Lease
//!                              └ deadline ─> Tick ─> Lease ── Release ─> resd
//! ```
//!
//! Calls run in order on this one thread, so a release can only come
//! between calls.

use crate::lease::{Effect, Input, Lease};
use panoptes_sync::{self as sync, Wait, mpsc};
use serde_json::Value;
use std::fmt;

/// Hands a call's result back to its edge.
pub type Reply<O> = Box<dyn FnOnce(Result<O, String>) + Send>;

/// One tool call from an edge.
pub struct Call<O> {
    pub tool: String,
    pub args: Value,
    pub reply: Reply<O>,
}

impl<O> fmt::Debug for Call<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

/// What runs the calls.
pub trait Tools {
    type Output;
    /// Run one tool. `wake_ms` is the time spent thawing the kiosk first,
    /// when it was frozen.
    fn call(
        &mut self,
        tool: &str,
        args: &Value,
        wake_ms: Option<f64>,
    ) -> Result<Self::Output, String>;
    /// After each call, while the kiosk is still held.
    fn after(&mut self) {}
}

/// What an acquire did.
#[derive(Debug, Clone, Copy, Default)]
pub struct Acquired {
    /// The round trip to panoptes-resd, with its cgroup writes.
    pub ms: f64,
    /// The kiosk was frozen or parked.
    pub woke: bool,
}

/// The residency manager's side of the lease.
pub trait Residency {
    /// Make the kiosk runnable. An error is resd's refusal; the call fails
    /// with it.
    fn acquire(&mut self) -> Result<Acquired, String>;
    fn release(&mut self);
}

/// Run calls until every edge is gone.
pub fn run<T: Tools, R: Residency>(
    calls: mpsc::Receiver<Call<T::Output>>,
    tools: &mut T,
    mut residency: Option<(R, Lease)>,
) {
    loop {
        let deadline = residency.as_ref().and_then(|(_, lease)| lease.deadline());
        let call = match deadline {
            Some(at) => sync::recv_timer(&calls, at),
            None => sync::recv(&calls),
        };
        match call {
            Ok(call) => {
                let woke = step(&mut residency, Input::Begin);
                let out = match woke {
                    Ok(woke) => {
                        let wake_ms = woke.filter(|a| a.woke).map(|a| a.ms);
                        let out = tools.call(&call.tool, &call.args, wake_ms);
                        tools.after();
                        let _ = step(&mut residency, Input::End);
                        out
                    }
                    Err(e) => {
                        let _ = step(&mut residency, Input::Refused);
                        Err(e)
                    }
                };
                (call.reply)(out);
            }
            Err(Wait::Timer) => {
                let _ = step(&mut residency, Input::Tick);
            }
            Err(Wait::Closed) => return,
        }
    }
}

/// Feed the lease one input and carry out its effect.
fn step<R: Residency>(
    residency: &mut Option<(R, Lease)>,
    input: Input,
) -> Result<Option<Acquired>, String> {
    let Some((resd, lease)) = residency.as_mut() else {
        return Ok(None);
    };
    match lease.step(input, sync::now()) {
        Some(Effect::Acquire) => resd.acquire().map(Some),
        Some(Effect::Release) => {
            resd.release();
            Ok(None)
        }
        None => Ok(None),
    }
}

#[cfg(all(test, panoptes_loom))]
mod loom_models {
    use super::*;
    use panoptes_sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use panoptes_sync::{Arc, thread};

    #[derive(Debug, Default)]
    struct Kiosk {
        held: AtomicBool,
        calls: AtomicU32,
        afters: AtomicU32,
    }

    /// The kiosk, and how many acquires to refuse before granting.
    struct FakeResd(Arc<Kiosk>, u32);

    impl Residency for FakeResd {
        fn acquire(&mut self) -> Result<Acquired, String> {
            if self.1 > 0 {
                self.1 -= 1;
                return Err("no".into());
            }
            assert!(!self.0.held.swap(true, Ordering::SeqCst), "acquired twice");
            Ok(Acquired {
                ms: 0.0,
                woke: true,
            })
        }
        fn release(&mut self) {
            assert!(
                self.0.held.swap(false, Ordering::SeqCst),
                "released a kiosk it does not hold"
            );
        }
    }

    struct FakeTools(Arc<Kiosk>);

    impl Tools for FakeTools {
        type Output = u32;
        fn call(&mut self, _: &str, _: &Value, _: Option<f64>) -> Result<u32, String> {
            assert!(
                self.0.held.load(Ordering::SeqCst),
                "a call ran on a frozen kiosk"
            );
            Ok(self.0.calls.fetch_add(1, Ordering::SeqCst))
        }
        fn after(&mut self) {
            assert!(
                self.0.held.load(Ordering::SeqCst),
                "after ran on a frozen kiosk"
            );
            self.0.afters.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// An edge sends two calls while the grace is zero, so the timer can
    /// fire and release the kiosk before the second call arrives, or not.
    /// Every call runs on a held kiosk, each gets one reply, and the desk
    /// thread ends once the edge is gone.
    ///
    /// One edge suffices: the desk thread cannot tell two edges' calls
    /// apart, so a second edge adds only orders of the same queue. With
    /// two edges the model ran over ten minutes.
    #[test]
    fn every_call_runs_on_a_held_kiosk() {
        panoptes_sync::model(|| {
            let kiosk = Arc::new(Kiosk::default());
            let (tx, rx) = mpsc::channel::<Call<u32>>();
            let owner = {
                let kiosk = kiosk.clone();
                thread::spawn(move || {
                    let mut tools = FakeTools(kiosk.clone());
                    run(rx, &mut tools, Some((FakeResd(kiosk, 0), Lease::new(0))));
                })
            };
            let (reply_tx, reply_rx) = mpsc::channel();
            for _ in 0..2 {
                let reply_tx = reply_tx.clone();
                let reply: Reply<u32> = Box::new(move |r| reply_tx.send(r).unwrap());
                let call = Call {
                    tool: "click".into(),
                    args: Value::Null,
                    reply,
                };
                tx.send(call).unwrap();
            }
            drop((tx, reply_tx));
            let mut got: Vec<u32> = (0..2).map(|_| reply_rx.recv().unwrap().unwrap()).collect();
            assert!(reply_rx.recv().is_err(), "a call replied twice");
            got.sort();
            assert_eq!(got, [0, 1]);
            owner.join().unwrap();
        });
    }

    /// resd refuses the first call and grants the second. The first call
    /// fails with the refusal. The fakes assert the kiosk is held in
    /// `call`, `after` and `release`, so none of them ran for it. The second
    /// call acquires again and runs. The grace is zero, so a timer that the
    /// refusal left behind could fire at any point.
    #[test]
    fn a_refused_call_runs_nothing_and_the_next_acquires_again() {
        panoptes_sync::model(|| {
            let kiosk = Arc::new(Kiosk::default());
            let (tx, rx) = mpsc::channel::<Call<u32>>();
            let owner = {
                let kiosk = kiosk.clone();
                thread::spawn(move || {
                    let mut tools = FakeTools(kiosk.clone());
                    run(rx, &mut tools, Some((FakeResd(kiosk, 1), Lease::new(0))));
                })
            };
            let (reply_tx, reply_rx) = mpsc::channel();
            for _ in 0..2 {
                let reply_tx = reply_tx.clone();
                let reply: Reply<u32> = Box::new(move |r| reply_tx.send(r).unwrap());
                let call = Call {
                    tool: "click".into(),
                    args: Value::Null,
                    reply,
                };
                tx.send(call).unwrap();
            }
            drop((tx, reply_tx));
            assert_eq!(reply_rx.recv().unwrap(), Err("no".to_owned()));
            assert_eq!(reply_rx.recv().unwrap(), Ok(0));
            assert!(reply_rx.recv().is_err(), "a call replied twice");
            owner.join().unwrap();
            assert_eq!(kiosk.calls.load(Ordering::SeqCst), 1);
            assert_eq!(kiosk.afters.load(Ordering::SeqCst), 1);
        });
    }
}
