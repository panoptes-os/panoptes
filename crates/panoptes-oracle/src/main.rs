//! panoptes-oracle: an OpenAI-compatible chat endpoint that cheats. It
//! reads which invoice the agent works on from the task message, looks the
//! invoice up in the demo's ground truth, and replies with the next correct
//! actions for the flow in `flow`. It tests the harness, not a model.
//!
//! ```text
//! panoptes-oracle --truth truth.jsonl --creds creds.tsv [--delay-ms 50] [--port 8099]
//!                 [--erp http://127.0.0.1:8080] [--log-dir DIR]
//! POST /v1/chat/completions   {"messages":[...], "stream":true}
//! ```
//!
//! `--delay-ms` holds every reply before its first token, standing in for
//! a model's time to first token.

mod flow;
mod tree;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use futures_util::stream;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug)]
struct Oracle {
    truth: HashMap<String, flow::Truth>,
    delay: Duration,
    served: AtomicU64,
    /// Where to append each invoice's transcript, if anywhere.
    log_dir: Option<std::path::PathBuf>,
}

#[derive(Debug)]
struct Args {
    truth: String,
    creds: String,
    delay_ms: u64,
    port: u16,
    erp: String,
    log_dir: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        truth: String::new(),
        creds: String::new(),
        delay_ms: 50,
        port: 8099,
        erp: "http://127.0.0.1:8080".into(),
        log_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().ok_or(format!("{k} needs a value"))?;
        match k.as_str() {
            "--truth" => a.truth = v,
            "--creds" => a.creds = v,
            "--delay-ms" => a.delay_ms = v.parse().map_err(|_| "--delay-ms N")?,
            "--port" => a.port = v.parse().map_err(|_| "--port N")?,
            "--erp" => a.erp = v,
            "--log-dir" => a.log_dir = Some(v),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.truth.is_empty() || a.creds.is_empty() {
        return Err("usage: panoptes-oracle --truth FILE --creds FILE [--delay-ms N] [--port N]".into());
    }
    Ok(a)
}

fn load(a: &Args) -> Result<HashMap<String, flow::Truth>, String> {
    let creds = std::fs::read_to_string(&a.creds).map_err(|e| format!("{}: {e}", a.creds))?;
    // slug  url  user_field  user  password
    let creds: HashMap<String, Vec<String>> = creds
        .lines()
        .skip(1)
        .map(|l| l.split('\t').map(str::to_owned).collect::<Vec<_>>())
        .filter(|f| f.len() >= 5)
        .map(|f| (f[0].clone(), f))
        .collect();
    let truth = std::fs::read_to_string(&a.truth).map_err(|e| format!("{}: {e}", a.truth))?;
    let mut out = HashMap::new();
    for l in truth.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(l).map_err(|e| format!("truth: {e}"))?;
        let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
        let Some(c) = creds.get(&s("slug")) else { continue };
        out.insert(
            s("file"),
            flow::Truth {
                file: s("file"),
                invoice_no: s("invoice_no"),
                date: s("date"),
                po: s("po"),
                tax_template: s("tax_template"),
                lines: v["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|l| (l["item_code"].as_str().unwrap_or("").to_owned(), l["qty"].as_f64().unwrap_or(f64::NAN)))
                    .collect(),
                total: v["total"].as_f64().unwrap_or(f64::NAN),
                portal_login: c[1].clone(),
                portal_user: c[3].clone(),
                portal_password: c[4].clone(),
                erp: a.erp.clone(),
                erp_user: "Administrator".into(),
                erp_password: "admin".into(),
            },
        );
    }
    Ok(out)
}

fn content(m: &Value) -> &str {
    m["content"].as_str().unwrap_or("")
}

/// The invoice a task is about. The loop hands an agent only the invoice
/// number and the portal, as a clerk gets them; numbers repeat across
/// suppliers, so the portal decides. An `invoice_file:` line, from older
/// tasks, names the truth row directly.
fn find_truth<'a>(o: &'a Oracle, task: &str) -> Result<&'a flow::Truth, String> {
    let line = |key: &str| task.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
    if let Some(file) = line("invoice_file:") {
        return o.truth.get(file).ok_or(format!("{file} is not in the truth file"));
    }
    let no = line("Invoice number:").ok_or("no invoice number in the task")?;
    let portal = line("Supplier portal:").ok_or("no supplier portal in the task")?;
    o.truth
        .values()
        .find(|t| t.invoice_no == no && t.portal_login == portal)
        .ok_or(format!("{no} at {portal} is not in the truth file"))
}

/// The next reply for one conversation, as the JSON text the model emits.
fn answer(o: &Oracle, messages: &[Value]) -> Result<String, String> {
    let task = messages.iter().find(|m| m["role"] == "user").map(content).unwrap_or("");
    let t = find_truth(o, task)?;
    let file = t.file.as_str();
    let note = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")
        .and_then(|m| serde_json::from_str::<Value>(content(m)).ok())
        .and_then(|v| v["note"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let state = flow::state_of(&note);
    let (phase, n) = (&state.phase, state.n);
    let last = messages.last().filter(|m| m["role"] == "user").map(content).unwrap_or("");
    let (results, page) = match last.split_once("Page:\n") {
        Some((r, p)) => (r, p),
        None => ("", last),
    };
    let nodes = tree::parse(page);
    let reply = flow::next(t, &state, results, &nodes).to_json().to_string();
    if let Some(dir) = &o.log_dir {
        use std::io::Write;
        let path = dir.join(format!("{file}.log"));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "=== phase={phase} n={n}\n{results}{page}\n>>> {reply}\n");
        }
    }
    Ok(reply)
}

fn chunk(id: &str, delta: Value, usage: Option<Value>) -> String {
    let mut c = json!({ "id": id, "object": "chat.completion.chunk", "model": "oracle",
                        "choices": [{ "index": 0, "delta": delta, "finish_reason": null }] });
    if let Some(u) = usage {
        c["choices"] = json!([]);
        c["usage"] = u;
    }
    format!("data: {c}\n\n")
}

async fn completions(State(o): State<Arc<Oracle>>, body: String) -> Response {
    let req: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad JSON: {e}")).into_response(),
    };
    let messages = req["messages"].as_array().cloned().unwrap_or_default();
    let text = match answer(&o, &messages) {
        Ok(t) => t,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let n = o.served.fetch_add(1, Ordering::Relaxed);
    let id = format!("oracle-{n}");
    let prompt_tokens = (body.len() / 4) as u64;
    let usage = json!({ "prompt_tokens": prompt_tokens, "completion_tokens": text.len() / 4,
                        "total_tokens": prompt_tokens + (text.len() / 4) as u64 });
    if req["stream"] != true {
        let r = json!({ "id": id, "object": "chat.completion", "model": "oracle",
                        "choices": [{ "index": 0, "finish_reason": "stop",
                                      "message": { "role": "assistant", "content": text } }],
                        "usage": usage });
        tokio::time::sleep(o.delay).await;
        return ([(header::CONTENT_TYPE, "application/json")], r.to_string()).into_response();
    }
    // Hold the first token for the configured delay, then stream the
    // reply in a few pieces the way a model would.
    let mut events = vec![chunk(&id, json!({ "role": "assistant", "content": "" }), None)];
    let chars: Vec<char> = text.chars().collect();
    for piece in chars.chunks(chars.len().div_ceil(4).max(1)) {
        events.push(chunk(&id, json!({ "content": piece.iter().collect::<String>() }), None));
    }
    events.push(chunk(&id, json!({}), Some(usage)));
    events.push("data: [DONE]\n\n".into());
    let delay = o.delay;
    let mut events = events.into_iter();
    let head = events.next().unwrap_or_default();
    let first = stream::once(async move {
        tokio::time::sleep(delay).await;
        Ok::<_, Infallible>(head)
    });
    let s = first.chain(stream::iter(events.map(Ok::<_, Infallible>)));
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(s))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[tokio::main]
async fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("panoptes-oracle: {e}");
            std::process::exit(2);
        }
    };
    let truth = match load(&a) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("panoptes-oracle: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("panoptes-oracle: {} invoices, delay {} ms, port {}", truth.len(), a.delay_ms, a.port);
    let o = Arc::new(Oracle { truth, delay: Duration::from_millis(a.delay_ms), served: AtomicU64::new(0),
                             log_dir: a.log_dir.map(Into::into) });
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .route("/v1/models", get(|| async { axum::Json(json!({ "data": [{ "id": "oracle", "object": "model" }] })) }))
        .with_state(o);
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", a.port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("panoptes-oracle: port {}: {e}", a.port);
            std::process::exit(1);
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("panoptes-oracle: {e}");
        std::process::exit(1);
    }
}
