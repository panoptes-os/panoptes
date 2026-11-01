//! The desk's network edge: the internal protocol and MCP on one unix
//! socket, and MCP on stdio with `--mcp-stdio`. The only async code in the
//! desk. It runs on one thread with a current-thread tokio runtime, turns
//! each request into an [`owner::Call`], and awaits the desk thread's
//! reply:
//!
//! ```text
//! socket, stdio ─> rmcp / lines ─> Call ── std mpsc ──> desk thread
//!                        ^────────────── tokio oneshot ──┘
//! ```
//!
//! A connection's first line picks the protocol. MCP clients open with a
//! JSON-RPC `initialize`, so a line carrying `"jsonrpc"` starts an MCP
//! session; anything else is the internal protocol panoptes-loop speaks,
//! one JSON object per line each way:
//!
//! ```text
//! > {"id":1,"tool":"click","args":{"x":412,"y":188,"screenshot":false}}
//! < {"id":1,"ok":true,"result":{"settled_ms":301.9,"timed_out":false,"windows":{...}},"timing":{...}}
//! ```
//!
//! A reply with a screenshot adds `"image":{"mime":"image/png","data":"<base64>"}`.

use crate::desk::{MAX_WAIT_MS, Output, tools};
use crate::owner::{Call, Reply};
use panoptes_sync::mpsc;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::UnixListener;

/// How long an edge waits for the desk thread's reply. One call takes at
/// most 2 waits of `MAX_WAIT_MS` (a timeout, then settling), 15 s with
/// resd and 5 s per capture; calls queued ahead add theirs.
const REPLY_LIMIT: Duration = Duration::from_millis(3 * MAX_WAIT_MS);

/// The longest request line the internal protocol takes.
const MAX_LINE: u64 = 4 << 20;

/// A way to the desk thread, for every edge task.
#[derive(Debug, Clone)]
pub struct Handle {
    calls: mpsc::Sender<Call<Output>>,
}

impl Handle {
    pub fn new(calls: mpsc::Sender<Call<Output>>) -> Handle {
        Handle { calls }
    }

    /// Run one tool on the desk thread.
    pub async fn call(&self, tool: &str, args: Value) -> Result<Output, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let reply: Reply<Output> = Box::new(move |r| {
            let _ = tx.send(r);
        });
        let call = Call {
            tool: tool.to_owned(),
            args,
            reply,
        };
        self.calls
            .send(call)
            .map_err(|_| "the desk thread is gone".to_owned())?;
        match tokio::time::timeout(REPLY_LIMIT, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err("the desk thread dropped the call".to_owned()),
            Err(_) => Err(format!(
                "the desk thread did not reply in {} s",
                REPLY_LIMIT.as_secs()
            )),
        }
    }
}

/// Serve the socket, and stdio with `mcp_stdio`, until SIGTERM, SIGINT,
/// the end of stdio, or a socket error. Returns the exit code.
pub fn run(handle: Handle, socket: PathBuf, mcp_stdio: bool) -> i32 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = rt.block_on(async {
        let listen = listen(handle.clone(), &socket);
        eprintln!("panoptes-desk: tools on {}", socket.display());
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        let stdio = async {
            if mcp_stdio {
                serve_mcp(handle.clone(), tokio::io::stdin(), tokio::io::stdout()).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            r = listen => {
                eprintln!("panoptes-desk: socket: {r:?}");
                1
            }
            _ = stdio => 0,
            _ = tokio::signal::ctrl_c() => 0,
            _ = term.recv() => 0,
        }
    });
    let _ = std::fs::remove_file(&socket);
    code
}

/// One line of at most [`MAX_LINE`] bytes into `line`; false at the end of
/// the stream or past the limit.
async fn read_line<R: AsyncBufRead + Unpin>(read: &mut R, line: &mut String) -> bool {
    match (&mut *read).take(MAX_LINE).read_line(line).await {
        Ok(0) | Err(_) => false,
        Ok(n) => line.ends_with('\n') || (n as u64) < MAX_LINE,
    }
}

async fn listen(handle: Handle, path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    loop {
        let (stream, _) = listener.accept().await?;
        let handle = handle.clone();
        tokio::spawn(async move {
            let (read, write) = stream.into_split();
            let mut read = BufReader::new(read);
            let mut first = String::new();
            if !read_line(&mut read, &mut first).await {
                return;
            }
            if first.contains("\"jsonrpc\"") {
                let replay = std::io::Cursor::new(first.into_bytes()).chain(read);
                serve_mcp(handle, replay, write).await;
            } else {
                serve_lines(handle, first, read, write).await;
            }
        });
    }
}

async fn serve_lines<R, W>(handle: Handle, first: String, mut read: BufReader<R>, mut write: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut line = first;
    loop {
        let reply = handle_line(&handle, line.trim()).await;
        let mut out = reply.to_string();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            return;
        }
        line = String::new();
        if !read_line(&mut read, &mut line).await {
            return;
        }
    }
}

async fn handle_line(handle: &Handle, line: &str) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": format!("bad request: {e}") }),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(tool) = req.get("tool").and_then(Value::as_str) else {
        return json!({ "id": id, "ok": false, "error": "request needs tool" });
    };
    if tool == "tools" {
        let list: Vec<Value> = tools()
            .into_iter()
            .map(|(n, d, s)| json!({ "name": n, "description": d, "input_schema": s }))
            .collect();
        return json!({ "id": id, "ok": true, "result": list });
    }
    let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
    let start = Instant::now();
    match handle.call(tool, args).await {
        Ok(out) => reply(id, out, start),
        Err(e) => json!({ "id": id, "ok": false, "error": e,
            "timing": { "total_ms": total(start) } }),
    }
}

fn total(start: Instant) -> f64 {
    (start.elapsed().as_secs_f64() * 1e5).round() / 100.0
}

fn timing(out: &Output, start: Instant) -> Value {
    let mut t = json!({ "action_ms": out.action_ms, "total_ms": total(start) });
    if let Some(c) = out.capture_ms {
        t["capture_ms"] = json!(c);
    }
    if let Some(w) = out.wake_ms {
        t["wake_ms"] = json!(w);
    }
    t
}

fn reply(id: Value, out: Output, start: Instant) -> Value {
    let mut r =
        json!({ "id": id, "ok": true, "result": out.result, "timing": timing(&out, start) });
    if let Some(data) = out.png {
        r["image"] = json!({ "mime": "image/png", "data": data });
    }
    r
}

/// MCP server over any byte stream: the socket after its first line, or
/// stdio.
async fn serve_mcp<R, W>(handle: Handle, read: R, write: W)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let server = DeskMcp { handle };
    match server.serve((read, write)).await {
        Ok(running) => {
            let _ = running.waiting().await;
        }
        Err(e) => eprintln!("panoptes-desk: mcp session failed: {e}"),
    }
}

#[derive(Clone)]
struct DeskMcp {
    handle: Handle,
}

impl ServerHandler for DeskMcp {
    fn get_info(&self) -> ServerConfig {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "panoptes-desk",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "One Panoptes agent's Linux desktop. Start programs with launch, find \
                 their windows with windows, and act with click, type, and key at the \
                 pixel coordinates of the screenshot each action returns.",
            )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let list = tools()
            .into_iter()
            .map(|(name, desc, schema)| {
                let schema = match schema {
                    Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                Tool::new(name, desc, Arc::new(schema))
            })
            .collect();
        std::future::ready(Ok(ListToolsResult::with_all_items(list)))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let handle = self.handle.clone();
        async move {
            let args = request
                .arguments
                .map(Value::Object)
                .unwrap_or_else(|| json!({}));
            let start = Instant::now();
            let result = match handle.call(&request.name, args).await {
                Ok(out) => {
                    let mut text = json!({ "result": out.result, "timing": timing(&out, start) });
                    if out.png.is_some() {
                        text["image"] = json!("attached");
                    }
                    let mut content = vec![ContentBlock::text(text.to_string())];
                    if let Some(data) = out.png {
                        content.push(ContentBlock::image(data, "image/png"));
                    }
                    CallToolResult::success(content)
                }
                Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
            };
            Ok(result.into())
        }
    }
}
