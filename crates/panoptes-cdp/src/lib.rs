//! Thin Chrome DevTools Protocol client.
//!
//! Chromium speaks CDP over `--remote-debugging-pipe`, reading commands from
//! fd 3 and writing replies and events to fd 4, each message ending in NUL,
//! or over a WebSocket on `--remote-debugging-port`. The pipe keeps CDP off
//! the network: only the process holding the other ends can drive the
//! browser. Both transports feed the same [`Cdp`] handle.
//!
//! Only the commands Panoptes uses have typed helpers, all in [`cmd`].

pub mod cmd;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The browser closed the connection, or exited.
    Closed,
    Timeout {
        method: String,
    },
    /// The browser answered with a CDP error.
    Protocol {
        method: String,
        code: i64,
        message: String,
    },
    /// A reply lacked a field the helper needs.
    Shape {
        method: &'static str,
        what: &'static str,
    },
    WebSocket(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "cdp i/o: {e}"),
            Error::Closed => write!(f, "cdp connection closed"),
            Error::Timeout { method } => write!(f, "{method}: no reply in time"),
            Error::Protocol {
                method,
                code,
                message,
            } => write!(f, "{method}: {message} ({code})"),
            Error::Shape { method, what } => write!(f, "{method}: reply has no {what}"),
            Error::WebSocket(e) => write!(f, "cdp websocket: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A CDP event. `session` is set for events from an attached target.
#[derive(Debug, Clone)]
pub struct Event {
    pub session: Option<String>,
    pub method: String,
    pub params: Value,
}

type Reply = std::result::Result<Value, (i64, String)>;

struct Inner {
    next: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    out: mpsc::UnboundedSender<String>,
    events: broadcast::Sender<Event>,
    timeout: Duration,
}

/// A connection to one browser. Cloning shares the connection.
#[derive(Clone)]
pub struct Cdp {
    inner: Arc<Inner>,
}

impl fmt::Debug for Cdp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pending = self.inner.pending.lock().map(|p| p.len()).unwrap_or(0);
        f.debug_struct("Cdp").field("pending", &pending).finish()
    }
}

/// The browser's ends of the debugging pipe: give `read` to the child as
/// fd 3 and `write` as fd 4, then drop both in the parent so a browser exit
/// reads as end of file.
#[derive(Debug)]
pub struct ChildPipe {
    pub read: OwnedFd,
    pub write: OwnedFd,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

impl Cdp {
    fn new(out: mpsc::UnboundedSender<String>) -> Self {
        let (events, _) = broadcast::channel(4096);
        Cdp {
            inner: Arc::new(Inner {
                next: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
                out,
                events,
                timeout: DEFAULT_TIMEOUT,
            }),
        }
    }

    /// Create the debugging pipe and a client on our ends. Must run inside a
    /// Tokio runtime.
    pub fn pipe() -> Result<(Cdp, ChildPipe)> {
        let (cmd_read, cmd_write) = pipe2()?;
        let (ev_read, ev_write) = pipe2()?;
        set_nonblocking(&cmd_write)?;
        set_nonblocking(&ev_read)?;
        let tx = tokio::net::unix::pipe::Sender::from_owned_fd(cmd_write)?;
        let rx = tokio::net::unix::pipe::Receiver::from_owned_fd(ev_read)?;
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let cdp = Cdp::new(out_tx);

        tokio::spawn(async move {
            let mut tx = tx;
            while let Some(msg) = out_rx.recv().await {
                let mut bytes = msg.into_bytes();
                bytes.push(0);
                if tx.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let reader = cdp.clone();
        tokio::spawn(async move {
            let mut rx = BufReader::with_capacity(1 << 20, rx);
            let mut buf = Vec::with_capacity(1 << 16);
            loop {
                buf.clear();
                match rx.read_until(0, &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if buf.last() == Some(&0) {
                            buf.pop();
                        }
                        reader.dispatch(&buf);
                    }
                }
            }
            reader.close();
        });
        Ok((
            cdp,
            ChildPipe {
                read: cmd_read,
                write: ev_write,
            },
        ))
    }

    /// Connect to a browser started with `--remote-debugging-port=port` on
    /// loopback, retrying until `wait` runs out.
    pub async fn port(port: u16, wait: Duration) -> Result<Cdp> {
        let deadline = tokio::time::Instant::now() + wait;
        let url = loop {
            match browser_ws_url(port).await {
                Ok(u) => break u,
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        };
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| Error::WebSocket(e.to_string()))?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let cdp = Cdp::new(out_tx);
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink
                    .send(tokio_tungstenite::tungstenite::Message::text(msg))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let reader = cdp.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
                    reader.dispatch(t.as_bytes());
                }
            }
            reader.close();
        });
        Ok(cdp)
    }

    fn dispatch(&self, raw: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(raw) else {
            return;
        };
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let tx = self
                .inner
                .pending
                .lock()
                .ok()
                .and_then(|mut p| p.remove(&id));
            if let Some(tx) = tx {
                let reply = match v.get("error") {
                    Some(e) => Err((
                        e.get("code").and_then(Value::as_i64).unwrap_or(0),
                        e.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    )),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(reply);
            }
            return;
        }
        if let Some(method) = v.get("method").and_then(Value::as_str) {
            let _ = self.inner.events.send(Event {
                session: v
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                method: method.to_owned(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
            });
        }
    }

    /// Fail every call still waiting; the senders drop with the map.
    fn close(&self) {
        if let Ok(mut p) = self.inner.pending.lock() {
            p.clear();
        }
    }

    /// Send one command and wait for its reply. `session` targets an
    /// attached page; `None` is the browser itself.
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .map_err(|_| Error::Closed)?
            .insert(id, tx);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        if self.inner.out.send(msg.to_string()).is_err() {
            self.forget(id);
            return Err(Error::Closed);
        }
        match tokio::time::timeout(self.inner.timeout, rx).await {
            Err(_) => {
                self.forget(id);
                Err(Error::Timeout {
                    method: method.to_owned(),
                })
            }
            Ok(Err(_)) => Err(Error::Closed),
            Ok(Ok(Err((code, message)))) => Err(Error::Protocol {
                method: method.to_owned(),
                code,
                message,
            }),
            Ok(Ok(Ok(v))) => Ok(v),
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut p) = self.inner.pending.lock() {
            p.remove(&id);
        }
    }

    /// Subscribe to events from now on.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }
}

fn pipe2() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe2 writes two descriptors into the two-element array.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: pipe2 succeeded, so both are new descriptors owned by us.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn set_nonblocking(fd: &OwnedFd) -> Result<()> {
    // SAFETY: fcntl on a descriptor we own; F_GETFL and F_SETFL take no
    // pointers.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

/// Read `webSocketDebuggerUrl` from `/json/version` with a bare HTTP/1.0
/// GET, which is all the DevTools HTTP endpoint needs.
async fn browser_ws_url(port: u16) -> Result<String> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    s.write_all(b"GET /json/version HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .await?;
    let mut body = Vec::new();
    s.read_to_end(&mut body).await?;
    let start = body
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(0, |i| i + 4);
    let v: Value = serde_json::from_slice(&body[start..])
        .map_err(|e| Error::WebSocket(format!("/json/version: {e}")))?;
    v.get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(Error::Shape {
            method: "/json/version",
            what: "webSocketDebuggerUrl",
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replies_and_events_route() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cdp = Cdp::new(tx);
        let mut ev = cdp.events();
        let c = cdp.clone();
        let call =
            tokio::spawn(async move { c.call(Some("S1"), "Page.navigate", json!({})).await });
        let sent: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(sent["sessionId"], "S1");
        let id = sent["id"].as_u64().unwrap();
        cdp.dispatch(
            br#"{"method":"Page.loadEventFired","sessionId":"S1","params":{"timestamp":1}}"#,
        );
        cdp.dispatch(format!(r#"{{"id":{id},"result":{{"frameId":"F"}}}}"#).as_bytes());
        assert_eq!(call.await.unwrap().unwrap()["frameId"], "F");
        let e = ev.recv().await.unwrap();
        assert_eq!(
            (e.method.as_str(), e.session.as_deref()),
            ("Page.loadEventFired", Some("S1"))
        );
    }

    #[tokio::test]
    async fn protocol_errors_surface() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cdp = Cdp::new(tx);
        let c = cdp.clone();
        let call = tokio::spawn(async move { c.call(None, "Nope.nope", json!({})).await });
        let sent: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        let id = sent["id"].as_u64().unwrap();
        cdp.dispatch(
            format!(r#"{{"id":{id},"error":{{"code":-32601,"message":"not found"}}}}"#).as_bytes(),
        );
        match call.await.unwrap() {
            Err(Error::Protocol { code, .. }) => assert_eq!(code, -32601),
            other => panic!("{other:?}"),
        }
    }
}
