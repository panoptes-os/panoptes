//! One worker thread per agent. It runs the agent's cgroup work in the
//! order the owner sent it, so a reclaim and a thaw never interleave. The
//! threads layer: the cgroup writes are the [`Kiosk`]'s.
//!
//! A reclaim runs in chunks. Work that arrives meanwhile stops it, so an
//! acquire waits for one chunk, not for the whole reclaim.

use crate::owner::{Done, Failure, Msg, ParkStat, Work};
use crate::policy::{Effect, Id};
use panoptes_sync::{self as sync, mpsc};

/// How one chunk of a reclaim went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunk {
    /// The kernel took the chunk; there is more to take.
    More,
    /// The kernel took everything asked for.
    Full,
    /// The kernel took less than the chunk: what is left is not
    /// reclaimable.
    Partial,
}

/// One agent's kiosk cgroup.
pub trait Kiosk {
    /// Thaw, freeze or weight; returns the ms the write took. Never a park.
    fn apply(&mut self, effect: Effect) -> Result<f64, String>;
    /// What `cgroup.freeze` asks for, when it can be read.
    fn frozen(&mut self) -> Option<bool>;
    /// Start a reclaim; false when the kiosk is no longer frozen.
    fn park_begin(&mut self) -> Result<bool, String>;
    fn reclaim_chunk(&mut self) -> Result<Chunk, String>;
    /// End the reclaim that `park_begin` started.
    fn park_end(&mut self, full: bool) -> Result<ParkStat, String>;
}

/// Run work until the owner drops its sender.
pub fn run<K: Kiosk>(id: Id, mut kiosk: K, work: mpsc::Receiver<Work>, done: mpsc::Sender<Msg>) {
    let mut next = None;
    loop {
        let w = match next.take() {
            Some(w) => w,
            None => match sync::recv(&work) {
                Ok(w) => w,
                Err(_) => return,
            },
        };
        let mut ms = 0.0;
        let mut failed = None;
        for effect in w.effects {
            match effect {
                Effect::Park => {
                    if let Some(w) = park(id, &mut kiosk, &work, &done) {
                        next = Some(w);
                    }
                }
                // Best effort: a weight changes nothing the policy tracks.
                Effect::Weight(_) => match kiosk.apply(effect) {
                    Ok(t) => ms += t,
                    Err(e) => eprintln!("panoptes-resd: agent {id}: {e}"),
                },
                Effect::Thaw | Effect::Freeze => match kiosk.apply(effect) {
                    Ok(t) => ms += t,
                    Err(error) => {
                        let frozen = kiosk.frozen();
                        failed = Some(Failure { error, frozen });
                        break;
                    }
                },
            }
        }
        if w.reply.is_some() || failed.is_some() {
            let ran = Done::Ran {
                ms,
                failed,
                reply: w.reply,
            };
            let _ = done.send(Msg::Done { id, done: ran });
        }
    }
}

/// Reclaim the kiosk until it is done or new work arrives, and report how
/// it ended. Returns the new work.
fn park<K: Kiosk>(
    id: Id,
    kiosk: &mut K,
    work: &mpsc::Receiver<Work>,
    done: &mpsc::Sender<Msg>,
) -> Option<Work> {
    let mut next = None;
    let (stat, error) = match kiosk.park_begin() {
        Ok(false) => (None, None),
        Err(e) => (None, Some(e)),
        Ok(true) => {
            let mut full = false;
            let mut error = None;
            loop {
                match kiosk.reclaim_chunk() {
                    Ok(Chunk::More) => {}
                    Ok(Chunk::Full) => {
                        full = true;
                        break;
                    }
                    Ok(Chunk::Partial) => break,
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                }
                if let Ok(w) = work.try_recv() {
                    next = Some(w);
                    break;
                }
            }
            match kiosk.park_end(full) {
                Ok(stat) => (Some(stat), error),
                Err(e) => (None, error.or(Some(e))),
            }
        }
    };
    let parked = Done::Parked { stat, error };
    let _ = done.send(Msg::Done { id, done: parked });
    next
}
