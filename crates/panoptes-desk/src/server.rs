//! The desk's two front ends on one unix socket.
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

use crate::desk::{Desk, Output, tools};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

pub async fn listen(desk: Arc<Desk>, path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    loop {
        let (stream, _) = listener.accept().await?;
        let desk = desk.clone();
        tokio::spawn(async move {
            let (read, write) = stream.into_split();
            let mut read = BufReader::new(read);
            let mut first = String::new();
            if read.read_line(&mut first).await.unwrap_or(0) == 0 {
                return;
            }
            if first.contains("\"jsonrpc\"") {
                let replay = std::io::Cursor::new(first.into_bytes()).chain(read);
                serve_mcp(desk, replay, write).await;
            } else {
                serve_lines(desk, first, read, write).await;
            }
        });
    }
}

async fn serve_lines<R, W>(desk: Arc<Desk>, first: String, mut read: BufReader<R>, mut write: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut line = first;
    loop {
        let reply = handle_line(&desk, line.trim()).await;
        let mut out = reply.to_string();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            return;
        }
        line = String::new();
        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
    }
}

async fn handle_line(desk: &Desk, line: &str) -> Value {
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
    match desk.call(tool, &args).await {
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
pub async fn serve_mcp<R, W>(desk: Arc<Desk>, read: R, write: W)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let server = DeskMcp { desk };
    match server.serve((read, write)).await {
        Ok(running) => {
            let _ = running.waiting().await;
        }
        Err(e) => eprintln!("panoptes-desk: mcp session failed: {e}"),
    }
}

#[derive(Clone)]
struct DeskMcp {
    desk: Arc<Desk>,
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
        let desk = self.desk.clone();
        async move {
            let args = request
                .arguments
                .map(Value::Object)
                .unwrap_or_else(|| json!({}));
            let start = Instant::now();
            let result = match desk.call(&request.name, &args).await {
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
