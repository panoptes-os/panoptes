//! How the Wayland event thread hands what it hears to the desk thread.
//! The threads layer: the locks and channels live here, and nothing here
//! talks to the compositor.
//!
//! ```text
//! event thread                           desk thread
//!   window events ─> Shared::apply ──┬─> windows(), handle(id)   copies under the lock
//!                                    └─> Change ─> wait_for       woken after each change
//!   frame, sync ───> Request::handle ──> wait_timer, wait_reply   or gives up and ends it
//! ```

use crate::windows::{self, Table, Windows};
use panoptes_sync::{self as sync, Arc, Ms, Mutex, MutexGuard, Wait, mpsc};
use std::fmt;
use std::time::Duration;

/// What wakes the desk thread's waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The shown windows changed.
    Windows,
    /// A launched process exited, with its wait status when known.
    Exited { pid: u32, status: Option<i32> },
    /// The connection to the compositor is gone.
    Lost,
}

/// What the event thread records for the desk thread. `H` is a window's
/// protocol object.
pub struct Shared<H> {
    windows: Mutex<Table<H>>,
    /// The output's current mode.
    size: Mutex<(u32, u32)>,
    /// Why the event thread stopped, once it has.
    lost: Mutex<Option<String>>,
    changes: mpsc::Sender<Change>,
}

impl<H> fmt::Debug for Shared<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl<H: Clone> Shared<H> {
    pub fn new() -> (Arc<Shared<H>>, mpsc::Receiver<Change>) {
        let (changes, rx) = mpsc::channel();
        let shared = Shared {
            windows: Mutex::new(Table::default()),
            size: Mutex::new((0, 0)),
            lost: Mutex::new(None),
            changes,
        };
        (Arc::new(shared), rx)
    }

    /// Wake the desk thread. The change is already visible.
    fn wake(&self, change: Change) {
        let _ = self.changes.send(change);
    }

    /// A new window; returns its id. Nothing shows until its first done.
    pub fn announce(&self, handle: H) -> u32 {
        lock(&self.windows).announce(handle)
    }

    /// Record one event of window `id`.
    pub fn apply(&self, id: u32, event: windows::Event) {
        let changed = lock(&self.windows).apply(id, event);
        if changed {
            self.wake(Change::Windows);
        }
    }

    pub fn set_xwayland(&self, display: String) {
        lock(&self.windows).set_xwayland(display);
        self.wake(Change::Windows);
    }

    pub fn set_size(&self, width: u32, height: u32) {
        *lock(&self.size) = (width, height);
    }

    /// The connection is gone: later calls fail, and waits end.
    pub fn lose(&self, why: String) {
        *lock(&self.lost) = Some(why);
        self.wake(Change::Lost);
    }

    /// A sender for wake-ups from other threads, such as a launched
    /// process's exit.
    pub fn notifier(&self) -> mpsc::Sender<Change> {
        self.changes.clone()
    }

    pub fn windows(&self) -> Windows {
        lock(&self.windows).windows()
    }

    /// The protocol object of shown window `id`.
    pub fn handle(&self, id: u32) -> Option<H> {
        lock(&self.windows).handle(id)
    }

    pub fn size(&self) -> (u32, u32) {
        *lock(&self.size)
    }

    /// Fail once the connection to the compositor is gone.
    pub fn alive(&self) -> Result<(), String> {
        match lock(&self.lost).as_ref() {
            Some(e) => Err(format!("lost the compositor: {e}")),
            None => Ok(()),
        }
    }
}

/// Throw away the wake-ups queued while no wait ran. A wait checks the
/// current state first, so they carry nothing it needs.
pub fn drain(changes: &mpsc::Receiver<Change>) {
    while changes.try_recv().is_ok() {}
}

/// How a wait ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited<R> {
    Met(R),
    Timer,
    Lost,
}

/// Wait until `check` returns a value. It runs once first, with no change,
/// then after every change, so a change that lands between a check and
/// the wait still wakes it. With no `deadline`, wait for as long as it
/// takes; the desk's tools always pass one.
pub fn wait_for<R>(
    changes: &mpsc::Receiver<Change>,
    deadline: Option<Ms>,
    mut check: impl FnMut(Option<&Change>) -> Option<R>,
) -> Waited<R> {
    if let Some(r) = check(None) {
        return Waited::Met(r);
    }
    loop {
        let change = match deadline {
            Some(at) => sync::recv_timer(changes, at),
            None => sync::recv(changes),
        };
        match change {
            Ok(Change::Lost) | Err(Wait::Closed) => return Waited::Lost,
            Ok(c) => {
                if let Some(r) = check(Some(&c)) {
                    return Waited::Met(r);
                }
            }
            Err(Wait::Timer) => return Waited::Timer,
        }
    }
}

/// What a request's own state does when the request ends.
pub trait Release {
    fn release(&mut self);
}

impl Release for () {
    fn release(&mut self) {}
}

/// One reply that the event thread fills in and the desk thread may stop
/// waiting for. Whichever side ends the request releases its state, once;
/// after that the event side leaves it alone.
pub struct Request<S, T> {
    inner: Mutex<Inner<S, T>>,
}

struct Inner<S, T> {
    state: S,
    reply: Option<mpsc::Sender<T>>,
    ended: bool,
}

impl<S, T> fmt::Debug for Request<S, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request").finish_non_exhaustive()
    }
}

impl<S: Release, T> Request<S, T> {
    pub fn new(state: S) -> (Arc<Request<S, T>>, mpsc::Receiver<T>) {
        let (tx, rx) = mpsc::channel();
        let inner = Inner {
            state,
            reply: Some(tx),
            ended: false,
        };
        let req = Request {
            inner: Mutex::new(inner),
        };
        (Arc::new(req), rx)
    }

    /// Run `f` on the state, ended or not.
    pub fn with<R>(&self, f: impl FnOnce(&mut S) -> R) -> R {
        f(&mut lock(&self.inner).state)
    }

    /// Event side: update the state while the request is open. A reply
    /// from `f` ends it.
    pub fn handle(&self, f: impl FnOnce(&mut S) -> Option<T>) {
        let mut inner = lock(&self.inner);
        if inner.ended {
            return;
        }
        if let Some(reply) = f(&mut inner.state) {
            inner.ended = true;
            inner.state.release();
            if let Some(tx) = inner.reply.take() {
                let _ = tx.send(reply);
            }
        }
    }

    /// Desk side: the reply, or `None` if none came by `deadline`; then
    /// this side ends the request. Under loom the deadline may pass at any
    /// point.
    pub fn wait_timer(&self, rx: &mpsc::Receiver<T>, deadline: Ms) -> Option<T> {
        match sync::recv_timer(rx, deadline) {
            Ok(reply) => Some(reply),
            Err(_) => self.give_up(rx),
        }
    }

    /// Desk side: the reply within `limit`, or an error naming `what`; on
    /// timeout this side ends the request.
    pub fn wait_reply(
        &self,
        rx: &mpsc::Receiver<T>,
        limit: Duration,
        what: &str,
    ) -> Result<T, String> {
        match sync::recv_reply(rx, limit, what) {
            Ok(reply) => Ok(reply),
            Err(e) => self.give_up(rx).ok_or(e),
        }
    }

    /// End the request from the desk side. The event side replies under
    /// the lock, so after taking it either the reply is queued or it never
    /// will be.
    fn give_up(&self, rx: &mpsc::Receiver<T>) -> Option<T> {
        let mut inner = lock(&self.inner);
        if inner.ended {
            drop(inner);
            return rx.try_recv().ok();
        }
        inner.ended = true;
        inner.reply = None;
        inner.state.release();
        None
    }
}

#[cfg(all(test, panoptes_loom))]
mod loom_models {
    use super::*;
    use crate::windows::Event;
    use panoptes_sync::thread;

    #[derive(Debug, Default)]
    struct Counted {
        releases: u32,
    }

    impl Release for Counted {
        fn release(&mut self) {
            self.releases += 1;
        }
    }

    /// The event thread replies while the desk thread's timer may fire at
    /// any point: the request is released exactly once, and the desk gets
    /// the reply unless it gave up first.
    #[test]
    fn a_request_ends_once() {
        panoptes_sync::model(|| {
            let (req, rx) = Request::<Counted, u32>::new(Counted::default());
            let event = {
                let req = req.clone();
                thread::spawn(move || req.handle(|_| Some(7)))
            };
            let got = req.wait_timer(&rx, 0);
            event.join().unwrap();
            assert_eq!(req.with(|s| s.releases), 1);
            assert!(matches!(got, None | Some(7)));
        });
    }

    /// A window that shows while the desk thread checks and waits always
    /// wakes it: no lost wake-up, so the wait cannot hang.
    #[test]
    fn a_window_change_always_wakes_a_waiter() {
        panoptes_sync::model(|| {
            let (shared, changes) = Shared::<u32>::new();
            let event = {
                let shared = shared.clone();
                thread::spawn(move || {
                    let id = shared.announce(1);
                    shared.apply(id, Event::Done);
                })
            };
            let seen = wait_for(&changes, None, |_| {
                (!shared.windows().list.is_empty()).then_some(())
            });
            assert_eq!(seen, Waited::Met(()));
            event.join().unwrap();
        });
    }

    /// Losing the compositor ends a wait that nothing else would.
    #[test]
    fn loss_ends_a_wait() {
        panoptes_sync::model(|| {
            let (shared, changes) = Shared::<u32>::new();
            let event = {
                let shared = shared.clone();
                thread::spawn(move || shared.lose("gone".into()))
            };
            assert_eq!(wait_for(&changes, None, |_| None::<()>), Waited::Lost);
            assert!(shared.alive().is_err());
            event.join().unwrap();
        });
    }
}
