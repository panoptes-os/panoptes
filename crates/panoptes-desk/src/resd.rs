//! The desk's side of residency: acquire the kiosk from panoptes-resd
//! before each tool call, and release it once no call has arrived for a
//! grace period. Every front end goes through `Desk::call`, so freezing
//! works the same for panoptes-loop and for any MCP harness.
//!
//! ```text
//! call ─> acquire (thaw if frozen) ─> tool ─> reply ─> GRACE ms quiet ─> release (freeze)
//! ```
//!
//! If panoptes-resd cannot be reached, the desk thaws its own kiosk (the
//! unit delegates the agent's cgroup to the desk's user) and acts anyway, so
//! a dead residency manager never wedges an agent.

use panoptes_cgroup::Cgroup;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

#[derive(Debug)]
struct Conn {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

#[derive(Debug, Default)]
struct Inner {
    conn: Option<Conn>,
    /// True between an acquire and the release after it.
    held: bool,
}

#[derive(Debug)]
pub struct Residency {
    socket: PathBuf,
    grace: Duration,
    kiosk: Cgroup,
    /// Bumped by every call; a pending release fires only if it is
    /// unchanged.
    generation: AtomicU64,
    inner: tokio::sync::Mutex<Inner>,
}

/// What an acquire did, for the tool reply's timing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Acquired {
    /// Round trip to panoptes-resd, including its cgroup writes.
    pub ms: f64,
    /// The kiosk was frozen or parked before this call.
    pub woke: bool,
}

impl Residency {
    pub fn new(socket: PathBuf, grace: Duration, kiosk: Cgroup) -> Arc<Residency> {
        Arc::new(Residency {
            socket,
            grace,
            kiosk,
            generation: AtomicU64::new(0),
            inner: tokio::sync::Mutex::default(),
        })
    }

    async fn send(&self, inner: &mut Inner, op: &str) -> Result<Value, String> {
        if inner.conn.is_none() {
            let s = UnixStream::connect(&self.socket)
                .await
                .map_err(|e| format!("{}: {e}", self.socket.display()))?;
            let (r, w) = s.into_split();
            inner.conn = Some(Conn {
                read: BufReader::new(r),
                write: w,
            });
        }
        let conn = inner.conn.as_mut().expect("connected");
        let line = format!("{{\"op\":\"{op}\"}}\n");
        let mut buf = String::new();
        let io = async {
            conn.write.write_all(line.as_bytes()).await?;
            conn.read.read_line(&mut buf).await
        };
        match io.await {
            Ok(n) if n > 0 => {}
            Ok(_) | Err(_) => {
                inner.conn = None;
                return Err("panoptes-resd closed the connection".into());
            }
        }
        let v: Value = serde_json::from_str(&buf).map_err(|e| e.to_string())?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v)
        } else {
            Err(v["error"].as_str().unwrap_or("refused").to_owned())
        }
    }

    /// Make the kiosk runnable before a tool call.
    pub async fn acquire(&self) -> Acquired {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut inner = self.inner.lock().await;
        if inner.held {
            return Acquired::default();
        }
        let start = std::time::Instant::now();
        let woke = match self.send(&mut inner, "acquire").await {
            Ok(v) => matches!(v["from"].as_str(), Some("waiting" | "parked")),
            Err(e) => {
                eprintln!("panoptes-desk: resd acquire: {e}; thawing the kiosk itself");
                let frozen = self.kiosk.events().map(|ev| ev.frozen).unwrap_or(false);
                if frozen {
                    let _ = self.kiosk.thaw(Duration::from_secs(5));
                }
                frozen
            }
        };
        inner.held = true;
        let ms = (start.elapsed().as_secs_f64() * 100_000.0).round() / 100.0;
        if woke {
            eprintln!("panoptes-desk: woke kiosk in {ms} ms");
        }
        Acquired { ms, woke }
    }

    /// Release the kiosk after the grace period, unless another call comes
    /// first.
    pub fn release_later(self: &Arc<Self>) {
        let generation = self.generation.load(Ordering::SeqCst);
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(me.grace).await;
            if me.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let mut inner = me.inner.lock().await;
            // A call may have started while this task waited for the lock.
            if me.generation.load(Ordering::SeqCst) != generation || !inner.held {
                return;
            }
            if let Err(e) = me.send(&mut inner, "release").await {
                eprintln!("panoptes-desk: resd release: {e}");
            }
            inner.held = false;
        });
    }
}
