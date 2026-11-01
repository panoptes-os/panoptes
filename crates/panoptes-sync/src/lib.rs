//! Threads, channels, locks and atomics for Panoptes' threads layers.
//!
//! A normal build re-exports std's. A build with `--cfg panoptes_loom`
//! re-exports loom's, so a loom model can run every order of the threads'
//! steps. Time and timed waits go through here too, because loom has no
//! clock: under loom, [`now`] stands still, and [`recv_timer`] and
//! [`recv_reply`] may time out at any point. See
//! docs/design/verification.md. The TLA+ specs in tla/ check the channel
//! and the waits.
//!
//! The cfg is not loom's usual `loom`: tokio drops its `net` module under
//! that name, and the network edges need it.

// The facade is the one crate that names std's threads, locks, channels
// and clock (threads 3 in docs/design/verification.md).
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

use std::time::Duration;

#[cfg(not(panoptes_loom))]
pub use std::sync::{Arc, Condvar, Mutex, MutexGuard, atomic};
#[cfg(not(panoptes_loom))]
pub use std::thread;

#[cfg(panoptes_loom)]
pub use loom::sync::{Arc, Condvar, Mutex, MutexGuard, atomic};
#[cfg(panoptes_loom)]
pub use loom::thread;

/// Run a loom model: every order of its threads' steps. Only under loom;
/// crates call it here so that loom stays this crate's dependency.
#[cfg(panoptes_loom)]
pub use loom::model;

// tla: begin. The specs in tla/ model the code from here to `tla: end`:
// the channel under loom, and the waits. tla/check.sh pins a hash of this
// text, so change the specs with it.

/// Unbounded channels: `send` never blocks.
///
/// Under loom these are this crate's own, built from loom's lock and
/// condvar. loom 0.7's channel never disconnects, so a thread that ends
/// when its senders go would hang a model. This one keeps std's rules:
/// queued messages come out first, then `recv` fails once every sender is
/// gone; `send` fails once the receiver is gone.
pub mod mpsc {
    #[cfg(not(panoptes_loom))]
    pub use std::sync::mpsc::{Receiver, Sender, channel};

    #[cfg(panoptes_loom)]
    pub use loom_channel::{Receiver, Sender, channel};

    #[cfg(panoptes_loom)]
    mod loom_channel {
        use loom::sync::{Arc, Condvar, Mutex};
        use std::collections::VecDeque;
        use std::fmt;
        use std::sync::mpsc::{RecvError, SendError, TryRecvError};

        struct State<T> {
            queue: VecDeque<T>,
            senders: usize,
            receiver: bool,
        }

        struct Chan<T> {
            state: Mutex<State<T>>,
            ready: Condvar,
        }

        pub struct Sender<T>(Arc<Chan<T>>);
        pub struct Receiver<T>(Arc<Chan<T>>);

        pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
            let state = State {
                queue: VecDeque::new(),
                senders: 1,
                receiver: true,
            };
            let chan = Arc::new(Chan {
                state: Mutex::new(state),
                ready: Condvar::new(),
            });
            (Sender(chan.clone()), Receiver(chan))
        }

        impl<T> Sender<T> {
            pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
                let mut s = self.0.state.lock().unwrap();
                if !s.receiver {
                    return Err(SendError(msg));
                }
                s.queue.push_back(msg);
                self.0.ready.notify_one();
                Ok(())
            }
        }

        impl<T> Clone for Sender<T> {
            fn clone(&self) -> Self {
                self.0.state.lock().unwrap().senders += 1;
                Sender(self.0.clone())
            }
        }

        impl<T> Drop for Sender<T> {
            fn drop(&mut self) {
                // A model that already failed is unwinding; loom would
                // abort on another step.
                if std::thread::panicking() {
                    return;
                }
                let mut s = self.0.state.lock().unwrap();
                s.senders -= 1;
                if s.senders == 0 {
                    self.0.ready.notify_one();
                }
            }
        }

        impl<T> Receiver<T> {
            pub fn recv(&self) -> Result<T, RecvError> {
                let mut s = self.0.state.lock().unwrap();
                loop {
                    if let Some(msg) = s.queue.pop_front() {
                        return Ok(msg);
                    }
                    if s.senders == 0 {
                        return Err(RecvError);
                    }
                    s = self.0.ready.wait(s).unwrap();
                }
            }

            pub fn try_recv(&self) -> Result<T, TryRecvError> {
                let mut s = self.0.state.lock().unwrap();
                match s.queue.pop_front() {
                    Some(msg) => Ok(msg),
                    None if s.senders == 0 => Err(TryRecvError::Disconnected),
                    None => Err(TryRecvError::Empty),
                }
            }
        }

        impl<T> Drop for Receiver<T> {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    return;
                }
                let mut s = self.0.state.lock().unwrap();
                s.receiver = false;
                // std drops queued messages with the receiver. Drop them
                // unlocked: one may hold a sender to this channel.
                let queue = std::mem::take(&mut s.queue);
                drop(s);
                drop(queue);
            }
        }

        impl<T> fmt::Debug for Sender<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("Sender { .. }")
            }
        }

        impl<T> fmt::Debug for Receiver<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("Receiver { .. }")
            }
        }
    }
}

/// std's scoped threads, in every build. loom has no `scope`, so keep a
/// scoped fan-out out of loom models: its threads should share nothing
/// they write.
pub use std::thread::scope;

/// Milliseconds since the process first asked for the time.
pub type Ms = u64;

/// The time, for policies that take `now`.
#[cfg(not(panoptes_loom))]
pub fn now() -> Ms {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as Ms
}

/// The time, for policies that take `now`. Under loom it stands still, so
/// a model must set every duration to zero.
#[cfg(panoptes_loom)]
pub fn now() -> Ms {
    0
}

/// Start a named thread. The name shows in gdb, `top -H` and panic
/// messages.
pub fn spawn<F, T>(name: &str, f: F) -> thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(f)
        .unwrap_or_else(|e| panic!("cannot start thread {name}: {e}"))
}

/// Why an owner's wait ended without a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The policy's deadline came.
    Timer,
    /// Every sender is gone.
    Closed,
}

/// Wait for the next message with no limit: an owner with nothing to time
/// is idle, not stuck.
pub fn recv<T>(rx: &mpsc::Receiver<T>) -> Result<T, Wait> {
    rx.recv().map_err(|_| Wait::Closed)
}

/// Wait for the next message until `deadline`, a time from [`now`].
///
/// Under loom this returns [`Wait::Timer`] at once if no message is queued,
/// so loom tries the timer firing at every point. loom does not implement
/// `recv_timeout`.
pub fn recv_timer<T>(rx: &mpsc::Receiver<T>, deadline: Ms) -> Result<T, Wait> {
    #[cfg(not(panoptes_loom))]
    {
        use std::sync::mpsc::RecvTimeoutError;
        let wait = Duration::from_millis(deadline.saturating_sub(now()));
        match rx.recv_timeout(wait) {
            Ok(m) => Ok(m),
            Err(RecvTimeoutError::Timeout) => Err(Wait::Timer),
            Err(RecvTimeoutError::Disconnected) => Err(Wait::Closed),
        }
    }
    #[cfg(panoptes_loom)]
    {
        use std::sync::mpsc::TryRecvError;
        let _ = deadline;
        match rx.try_recv() {
            Ok(m) => Ok(m),
            Err(TryRecvError::Empty) => Err(Wait::Timer),
            Err(TryRecvError::Disconnected) => Err(Wait::Closed),
        }
    }
}

/// Wait at most `limit` for a reply from another thread. The error names
/// `what` the caller waited for. A late reply can still come after it.
///
/// Under loom this fails at once if no reply is queued, so loom tries the
/// timeout at every point, as with [`recv_timer`].
pub fn recv_reply<T>(rx: &mpsc::Receiver<T>, limit: Duration, what: &str) -> Result<T, String> {
    #[cfg(not(panoptes_loom))]
    {
        use std::sync::mpsc::RecvTimeoutError;
        match rx.recv_timeout(limit) {
            Ok(m) => Ok(m),
            Err(RecvTimeoutError::Timeout) => Err(format!("{what}: no reply in {limit:?}")),
            Err(RecvTimeoutError::Disconnected) => Err(format!("{what}: its thread is gone")),
        }
    }
    #[cfg(panoptes_loom)]
    {
        use std::sync::mpsc::TryRecvError;
        match rx.try_recv() {
            Ok(m) => Ok(m),
            Err(TryRecvError::Empty) => Err(format!("{what}: no reply in {limit:?}")),
            Err(TryRecvError::Disconnected) => Err(format!("{what}: its thread is gone")),
        }
    }
}
// tla: end

#[cfg(all(test, not(panoptes_loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_timer_fires_at_its_deadline() {
        let (_tx, rx) = mpsc::channel::<()>();
        let start = now();
        assert_eq!(recv_timer(&rx, start + 20), Err(Wait::Timer));
        assert!(now() >= start + 20);
    }

    #[test]
    fn a_message_beats_the_timer() {
        let (tx, rx) = mpsc::channel();
        tx.send(7).unwrap();
        assert_eq!(recv_timer(&rx, now() + 10_000), Ok(7));
    }

    #[test]
    fn a_closed_channel_ends_the_wait() {
        let (tx, rx) = mpsc::channel::<()>();
        drop(tx);
        assert_eq!(recv(&rx), Err(Wait::Closed));
        assert_eq!(recv_timer(&rx, now() + 10_000), Err(Wait::Closed));
    }

    #[test]
    fn a_late_reply_names_what_was_awaited() {
        let (_tx, rx) = mpsc::channel::<()>();
        let e = recv_reply(&rx, Duration::from_millis(5), "frame").unwrap_err();
        assert!(e.starts_with("frame: no reply"), "{e}");
    }

    #[test]
    fn threads_carry_their_names() {
        let name = spawn("probe", || thread::current().name().map(str::to_owned));
        assert_eq!(name.join().unwrap().as_deref(), Some("probe"));
    }
}

/// The facade itself under loom: a timer or a reply's timeout can fire
/// before or after a message, and a channel closes as std's does. A model
/// that claims two outcomes checks that loom reached both.
#[cfg(all(test, panoptes_loom))]
mod loom_models {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

    #[test]
    fn a_timer_can_fire_before_or_after_a_message() {
        static MESSAGE: AtomicBool = AtomicBool::new(false);
        static TIMER: AtomicBool = AtomicBool::new(false);
        loom::model(|| {
            let (tx, rx) = mpsc::channel();
            let sender = thread::spawn(move || tx.send(1).unwrap());
            let first = recv_timer(&rx, 0);
            sender.join().unwrap();
            match first {
                Ok(1) => MESSAGE.store(true, Relaxed),
                Err(Wait::Timer) => {
                    assert_eq!(recv(&rx), Ok(1));
                    TIMER.store(true, Relaxed);
                }
                other => panic!("{other:?}"),
            }
        });
        assert!(
            MESSAGE.load(Relaxed) && TIMER.load(Relaxed),
            "loom tried one order"
        );
    }

    #[test]
    fn the_last_sender_closes_the_channel_after_its_messages() {
        loom::model(|| {
            let (tx, rx) = mpsc::channel();
            let other = tx.clone();
            let a = thread::spawn(move || tx.send(1).unwrap());
            let b = thread::spawn(move || drop(other));
            assert_eq!(recv(&rx), Ok(1));
            assert_eq!(recv(&rx), Err(Wait::Closed));
            a.join().unwrap();
            b.join().unwrap();
        });
    }

    #[test]
    fn a_send_fails_once_the_receiver_is_gone() {
        loom::model(|| {
            let (tx, rx) = mpsc::channel::<u32>();
            let r = thread::spawn(move || drop(rx));
            r.join().unwrap();
            assert!(tx.send(1).is_err());
        });
    }

    #[test]
    fn a_reply_can_come_before_or_after_its_timeout() {
        static REPLY: AtomicBool = AtomicBool::new(false);
        static TIMEOUT: AtomicBool = AtomicBool::new(false);
        loom::model(|| {
            let (tx, rx) = mpsc::channel();
            let replier = thread::spawn(move || tx.send("ok").unwrap());
            let got = recv_reply(&rx, Duration::ZERO, "reply");
            replier.join().unwrap();
            match got {
                Ok("ok") => REPLY.store(true, Relaxed),
                Err(e) => {
                    assert!(e.starts_with("reply: no reply"), "{e}");
                    // The late reply still arrives.
                    assert_eq!(recv(&rx), Ok("ok"));
                    TIMEOUT.store(true, Relaxed);
                }
                other => panic!("{other:?}"),
            }
        });
        assert!(
            REPLY.load(Relaxed) && TIMEOUT.load(Relaxed),
            "loom tried one order"
        );
    }
}
