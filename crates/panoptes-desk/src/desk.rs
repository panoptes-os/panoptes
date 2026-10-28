//! The tools an agent drives its kiosk with. Both front ends, the internal
//! socket protocol and MCP, call [`Desk::call`].

use crate::resd::Residency;
use crate::{axtext, keys};
use panoptes_cdp::cmd::{FileInput, ImageFormat, MouseKind, Session};
use panoptes_cdp::{Cdp, Event};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A tool's result: JSON for the caller, an image for screenshot, and how
/// long the action and the capture took.
#[derive(Debug)]
pub struct Output {
    pub result: Value,
    pub image: Option<(&'static str, String)>,
    pub action_ms: f64,
    pub capture_ms: Option<f64>,
    /// Time to thaw or resume the kiosk first, when it was frozen.
    pub wake_ms: Option<f64>,
}

impl Output {
    fn json(result: Value, action: Instant) -> Self {
        Output {
            result,
            image: None,
            action_ms: ms(action.elapsed()),
            capture_ms: None,
            wake_ms: None,
        }
    }
}

fn clip_url(u: &str) -> String {
    u.chars().take(160).collect()
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e5).round() / 100.0
}

/// Page activity, kept current by the event task.
#[derive(Debug, Default)]
struct PageState {
    /// Requests in flight, with their start time; long polls age out.
    /// requestId -> (sent, loaderId, url)
    inflight: HashMap<String, (Instant, String, String)>,
    loader: String,
    lifecycle: Vec<String>,
    downloads: HashMap<String, Value>,
}

#[derive(Debug)]
pub struct Desk {
    browser: Cdp,
    page: Mutex<Session>,
    /// Pages opened from the current one, newest last.
    stack: Mutex<Vec<Session>>,
    state: Arc<Mutex<PageState>>,
    downloads: PathBuf,
    busy: tokio::sync::Mutex<()>,
    residency: Option<Arc<Residency>>,
    /// Small captures for `panoptes top`, off unless set.
    thumb: Mutex<Option<Thumb>>,
}

/// Where and how often the desk writes a thumbnail of its page: a base64
/// JPEG at a tenth of the viewport, replaced atomically, taken only right
/// after a tool call while the kiosk is already thawed.
#[derive(Debug)]
struct Thumb {
    path: PathBuf,
    every: Duration,
    last: Option<Instant>,
}

/// Name, description, and JSON Schema of every tool, shared by both front
/// ends.
pub fn tools() -> Vec<(&'static str, &'static str, Value)> {
    let node = json!({ "type": "integer", "description": "node id from ax_tree" });
    vec![
        (
            "screenshot",
            "Capture the visible page. Returns an image plus its size.",
            json!({ "type": "object", "properties": {
                "format": { "type": "string", "enum": ["jpeg", "png"], "default": "jpeg" },
                "quality": { "type": "integer", "minimum": 1, "maximum": 100, "default": 80 },
                "scale": { "type": "number", "minimum": 0.1, "maximum": 1.0, "default": 1.0 } } }),
        ),
        (
            "click",
            "Click at viewport coordinates {x, y}, or at the center of an ax_tree node {node_id}.",
            json!({ "type": "object", "properties": {
                "x": { "type": "number" }, "y": { "type": "number" }, "node_id": node,
                "button": { "type": "string", "enum": ["left", "right", "middle"], "default": "left" },
                "count": { "type": "integer", "minimum": 1, "maximum": 3, "default": 1 } } }),
        ),
        (
            "type",
            "Insert text at the focus, or into an ax_tree node {node_id} after focusing it.",
            json!({ "type": "object", "required": ["text"], "properties": {
                "text": { "type": "string" }, "node_id": node } }),
        ),
        (
            "key",
            "Press one key or chord: Enter, Tab, Escape, Backspace, ArrowDown, Control+a, ...",
            json!({ "type": "object", "required": ["key"], "properties": {
                "key": { "type": "string" } } }),
        ),
        (
            "scroll",
            "Scroll by {dy} pixels (positive is down), at {x, y} or the viewport center.",
            json!({ "type": "object", "properties": {
                "dx": { "type": "number", "default": 0 }, "dy": { "type": "number", "default": 300 },
                "x": { "type": "number" }, "y": { "type": "number" } } }),
        ),
        (
            "navigate",
            "Open a URL and wait for its load event.",
            json!({ "type": "object", "required": ["url"], "properties": {
                "url": { "type": "string" },
                "timeout_ms": { "type": "integer", "default": 15000 } } }),
        ),
        (
            "ax_tree",
            "The page's accessibility tree as compact text: [node_id] role \"name\" value=... @(x,y wxh), and the page's URL.",
            json!({ "type": "object", "properties": {
                "max_bytes": { "type": "integer", "default": 24000 } } }),
        ),
        (
            "wait_settled",
            "Wait until the page has loaded, the network is quiet, and the layout stops changing for quiet_ms.",
            json!({ "type": "object", "properties": {
                "quiet_ms": { "type": "integer", "default": 300 },
                "timeout_ms": { "type": "integer", "default": 5000 } } }),
        ),
        (
            "download_list",
            "Files in this agent's download directory, and downloads still in progress.",
            json!({ "type": "object", "properties": {} }),
        ),
        (
            "upload",
            "Put a downloaded file into a file input, given by ax_tree {node_id} or CSS {selector}. \
{path} is a file name in this agent's download directory.",
            json!({ "type": "object", "required": ["path"], "properties": {
                "path": { "type": "string" }, "node_id": node,
                "selector": { "type": "string", "default": "input[type=file]" } } }),
        ),
        (
            "read_pdf",
            "Text of a downloaded PDF, laid out as on the page (pdftotext -layout). \
{path} is a file name in this agent's download directory; the text is cut to max_bytes.",
            json!({ "type": "object", "required": ["path"], "properties": {
                "path": { "type": "string" },
                "max_bytes": { "type": "integer", "default": 8000 } } }),
        ),
        (
            "reload",
            "Reload the page and wait for its load event.",
            json!({ "type": "object", "properties": {
                "timeout_ms": { "type": "integer", "default": 15000 } } }),
        ),
    ]
}

/// A request that has run this long is a long poll, not page work.
const LONG_POLL: Duration = Duration::from_secs(5);

fn arg_f64(a: &Value, k: &str) -> Option<f64> {
    a.get(k).and_then(Value::as_f64)
}

fn arg_u64(a: &Value, k: &str, default: u64) -> u64 {
    a.get(k).and_then(Value::as_u64).unwrap_or(default)
}

impl Desk {
    /// Attach to the kiosk's page and start tracking its events.
    pub async fn new(
        browser: Cdp,
        downloads: PathBuf,
        residency: Option<Arc<Residency>>,
    ) -> Result<Arc<Desk>, String> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let page = loop {
            let targets = browser.targets().await.map_err(|e| e.to_string())?;
            if let Some(t) = targets.iter().find(|t| t.kind == "page") {
                break t.id.clone();
            }
            if Instant::now() > deadline {
                return Err("no page target appeared".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let events = browser.events();
        let session = browser.attach(&page).await.map_err(|e| e.to_string())?;
        session.enable().await.map_err(|e| e.to_string())?;
        browser
            .discover_targets()
            .await
            .map_err(|e| e.to_string())?;
        browser
            .set_download_dir(&downloads.display().to_string())
            .await
            .map_err(|e| e.to_string())?;
        let desk = Arc::new(Desk {
            browser,
            page: Mutex::new(session),
            stack: Mutex::new(Vec::new()),
            state: Arc::new(Mutex::new(PageState::default())),
            downloads,
            busy: tokio::sync::Mutex::new(()),
            residency,
            thumb: Mutex::new(None),
        });
        tokio::spawn(track(Arc::downgrade(&desk), events));
        Ok(desk)
    }

    fn page(&self) -> Session {
        self.page.lock().map(|p| p.clone()).expect("page lock")
    }

    /// Run one tool. With a residency manager, the kiosk is acquired first
    /// and released once calls stop for the grace period.
    pub async fn call(&self, tool: &str, args: &Value) -> Result<Output, String> {
        let Some(res) = &self.residency else {
            let out = self.call_tool(tool, args).await;
            self.thumb_if_due().await;
            return out;
        };
        let woke = res.acquire().await;
        let out = self.call_tool(tool, args).await;
        self.thumb_if_due().await;
        res.release_later();
        out.map(|mut o| {
            if woke.woke {
                o.wake_ms = Some(woke.ms);
            }
            o
        })
    }

    /// Write a thumbnail every `every` to `path` (written as `path.tmp`, then
    /// renamed).
    pub fn set_thumb(&self, path: PathBuf, every: Duration) {
        if let Ok(mut t) = self.thumb.lock() {
            *t = Some(Thumb {
                path,
                every,
                last: None,
            });
        }
    }

    async fn thumb_if_due(&self) {
        let path = {
            let Ok(mut guard) = self.thumb.lock() else {
                return;
            };
            let Some(t) = guard.as_mut() else { return };
            if t.last.is_some_and(|l| l.elapsed() < t.every) {
                return;
            }
            t.last = Some(Instant::now());
            t.path.clone()
        };
        let _turn = self.busy.lock().await;
        let page = self.page();
        let Ok(vp) = page.viewport().await else {
            return;
        };
        let Ok(data) = page.screenshot(vp, ImageFormat::Jpeg, 50, 0.1).await else {
            return;
        };
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    async fn call_tool(&self, tool: &str, args: &Value) -> Result<Output, String> {
        let _turn = self.busy.lock().await;
        let start = Instant::now();
        let page = self.page();
        let e = |e: panoptes_cdp::Error| e.to_string();
        match tool {
            "screenshot" => {
                let format = match args.get("format").and_then(Value::as_str) {
                    Some("png") => ImageFormat::Png,
                    None | Some("jpeg") => ImageFormat::Jpeg,
                    Some(other) => return Err(format!("unknown format {other}")),
                };
                let quality = arg_u64(args, "quality", 80).clamp(1, 100) as u8;
                let scale = arg_f64(args, "scale").unwrap_or(1.0).clamp(0.1, 1.0);
                let vp = page.viewport().await.map_err(e)?;
                let cap = Instant::now();
                let data = page
                    .screenshot(vp, format, quality, scale)
                    .await
                    .map_err(e)?;
                let capture_ms = ms(cap.elapsed());
                Ok(Output {
                    result: json!({
                        "format": if format == ImageFormat::Png { "png" } else { "jpeg" },
                        "width": (vp.width * scale).round(), "height": (vp.height * scale).round(),
                        "bytes": data.len() / 4 * 3,
                    }),
                    image: Some((format.mime(), data)),
                    action_ms: ((ms(start.elapsed()) - capture_ms) * 100.0).round() / 100.0,
                    capture_ms: Some(capture_ms),
                    wake_ms: None,
                })
            }
            "click" => {
                let (x, y) = match args.get("node_id").and_then(Value::as_i64) {
                    Some(node) => {
                        page.scroll_into_view(node).await.map_err(e)?;
                        page.node_center(node).await.map_err(e)?
                    }
                    None => (
                        arg_f64(args, "x").ok_or("click needs x and y, or node_id")?,
                        arg_f64(args, "y").ok_or("click needs x and y, or node_id")?,
                    ),
                };
                let button = args.get("button").and_then(Value::as_str).unwrap_or("left");
                let count = arg_u64(args, "count", 1).clamp(1, 3) as u32;
                page.mouse(MouseKind::Moved, x, y, button, 0, (0.0, 0.0))
                    .await
                    .map_err(e)?;
                for n in 1..=count {
                    page.mouse(MouseKind::Pressed, x, y, button, n, (0.0, 0.0))
                        .await
                        .map_err(e)?;
                    page.mouse(MouseKind::Released, x, y, button, n, (0.0, 0.0))
                        .await
                        .map_err(e)?;
                }
                Ok(Output::json(
                    json!({ "x": x.round(), "y": y.round() }),
                    start,
                ))
            }
            "type" => {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("type needs text")?;
                if let Some(node) = args.get("node_id").and_then(Value::as_i64) {
                    page.scroll_into_view(node).await.map_err(e)?;
                    page.focus(node).await.map_err(e)?;
                }
                page.insert_text(text).await.map_err(e)?;
                Ok(Output::json(
                    json!({ "chars": text.chars().count() }),
                    start,
                ))
            }
            "key" => {
                let spec = args
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or("key needs key")?;
                let [down, up] = keys::press(spec)?;
                page.key_event(down).await.map_err(e)?;
                page.key_event(up).await.map_err(e)?;
                Ok(Output::json(json!({ "key": spec }), start))
            }
            "scroll" => {
                let vp = page.viewport().await.map_err(e)?;
                let x = arg_f64(args, "x").unwrap_or(vp.width / 2.0);
                let y = arg_f64(args, "y").unwrap_or(vp.height / 2.0);
                let dx = arg_f64(args, "dx").unwrap_or(0.0);
                let dy = arg_f64(args, "dy").unwrap_or(300.0);
                page.mouse(MouseKind::Wheel, x, y, "none", 0, (dx, dy))
                    .await
                    .map_err(e)?;
                Ok(Output::json(json!({ "dx": dx, "dy": dy }), start))
            }
            "navigate" => {
                let url = args
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("navigate needs url")?;
                let timeout = Duration::from_millis(arg_u64(args, "timeout_ms", 15_000));
                let loader = match page.navigate(url).await.map_err(e)? {
                    Ok(l) => l,
                    Err(text) => return Err(format!("navigation failed: {text}")),
                };
                let loaded = self.wait_lifecycle(&loader, "load", timeout).await;
                Ok(Output::json(json!({ "url": url, "loaded": loaded }), start))
            }
            "ax_tree" => {
                let max = arg_u64(args, "max_bytes", 24_000) as usize;
                let (nodes, boxes, url) = tokio::join!(
                    page.ax_tree(),
                    page.layout_boxes(),
                    page.evaluate("location.href")
                );
                let nodes = nodes.map_err(e)?;
                let boxes = boxes.unwrap_or_default();
                let r = axtext::render(&nodes, &boxes, max);
                Ok(Output::json(
                    json!({ "tree": r.text, "nodes": r.nodes, "truncated": r.truncated,
                            "url": url.unwrap_or_default() }),
                    start,
                ))
            }
            "wait_settled" => {
                let quiet = Duration::from_millis(arg_u64(args, "quiet_ms", 300));
                let timeout = Duration::from_millis(arg_u64(args, "timeout_ms", 5000));
                let r = self.wait_settled(&page, quiet, timeout).await;
                Ok(Output::json(r, start))
            }
            "download_list" => {
                let mut files = Vec::new();
                if let Ok(rd) = std::fs::read_dir(&self.downloads) {
                    for ent in rd.flatten() {
                        let name = ent.file_name().to_string_lossy().into_owned();
                        let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
                        files.push(json!({
                            "name": name, "bytes": size,
                            "complete": !name.ends_with(".crdownload"),
                        }));
                    }
                }
                files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                let active: Vec<Value> = self
                    .state
                    .lock()
                    .map(|s| s.downloads.values().cloned().collect())
                    .unwrap_or_default();
                Ok(Output::json(
                    json!({ "dir": self.downloads, "files": files, "events": active }),
                    start,
                ))
            }
            "upload" => {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("upload needs path")?;
                let file = self.download_path(path)?;
                let target = match args.get("node_id").and_then(Value::as_i64) {
                    Some(node) => FileInput::Node(node),
                    None => FileInput::Selector(
                        args.get("selector")
                            .and_then(Value::as_str)
                            .unwrap_or("input[type=file]"),
                    ),
                };
                page.set_file_input(target, std::slice::from_ref(&file))
                    .await
                    .map_err(e)?;
                Ok(Output::json(json!({ "file": file }), start))
            }
            "read_pdf" => {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("read_pdf needs path")?;
                let file = self.download_path(path)?;
                let max = arg_u64(args, "max_bytes", 8000) as usize;
                let bin =
                    std::env::var("PANOPTES_PDFTOTEXT").unwrap_or_else(|_| "pdftotext".into());
                let run = tokio::process::Command::new(&bin)
                    .args(["-layout", &file, "-"])
                    .kill_on_drop(true)
                    .output();
                let out = tokio::time::timeout(Duration::from_secs(10), run)
                    .await
                    .map_err(|_| format!("{bin}: timed out after 10 s"))?
                    .map_err(|err| format!("{bin}: {err}"))?;
                if !out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stderr);
                    return Err(format!("{bin}: {}: {}", out.status, msg.trim()));
                }
                let text = String::from_utf8_lossy(&out.stdout);
                let mut cut = text.len().min(max);
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                Ok(Output::json(
                    json!({ "file": file, "text": &text[..cut], "truncated": cut < text.len() }),
                    start,
                ))
            }
            "reload" => {
                let timeout = Duration::from_millis(arg_u64(args, "timeout_ms", 15_000));
                // Page.reload returns no loader id: wait for a loader other
                // than the current one to reach load.
                let old = self
                    .state
                    .lock()
                    .map(|s| s.loader.clone())
                    .unwrap_or_default();
                page.reload().await.map_err(e)?;
                let deadline = Instant::now() + timeout;
                let mut loaded = false;
                while Instant::now() < deadline {
                    loaded = self
                        .state
                        .lock()
                        .map(|s| s.loader != old && s.lifecycle.iter().any(|l| l == "load"))
                        .unwrap_or(false);
                    if loaded {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok(Output::json(json!({ "loaded": loaded }), start))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    /// `p` resolved inside the download directory; anything that escapes it
    /// (`..`, absolute paths, symlinks out) is refused.
    fn download_path(&self, p: &str) -> Result<String, String> {
        let root = self
            .downloads
            .canonicalize()
            .map_err(|err| format!("download dir: {err}"))?;
        let file = root
            .join(p)
            .canonicalize()
            .map_err(|err| format!("{p}: {err}"))?;
        if !file.starts_with(&root) || !file.is_file() {
            return Err(format!("{p} is not a file in {}", root.display()));
        }
        Ok(file.display().to_string())
    }

    async fn wait_lifecycle(&self, loader: &str, name: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let done = self
                .state
                .lock()
                .map(|s| s.loader == loader && s.lifecycle.iter().any(|l| l == name))
                .unwrap_or(false);
            if done {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// Settled: document complete, no request younger than [`LONG_POLL`]
    /// in flight, and the same layout signature for `quiet`. The reply says
    /// how long the network held it and, unsettled, which requests.
    async fn wait_settled(&self, page: &Session, quiet: Duration, timeout: Duration) -> Value {
        const SIG: &str = "document.readyState + ':' + document.getElementsByTagName('*').length \
            + ':' + (document.body ? document.body.innerText.length : 0) + ':' \
            + document.documentElement.scrollHeight";
        let start = Instant::now();
        let mut last = Value::Null;
        let mut stable_since = Instant::now();
        let mut network = Duration::ZERO;
        let mut held: Vec<String> = Vec::new();
        while start.elapsed() < timeout {
            let mut pending: Vec<String> = self
                .state
                .lock()
                .map(|s| {
                    s.inflight
                        .values()
                        .filter(|(t, _, _)| t.elapsed() < LONG_POLL)
                        .map(|(_, _, url)| url.clone())
                        .collect()
                })
                .unwrap_or_default();
            let busy = !pending.is_empty();
            if busy {
                pending.truncate(8);
                held = pending;
            }
            let tick = Instant::now();
            let sig = page.evaluate(SIG).await.unwrap_or(Value::Null);
            let complete = sig.as_str().is_some_and(|s| s.starts_with("complete:"));
            if sig != last || busy || !complete {
                last = sig;
                stable_since = Instant::now();
            } else if stable_since.elapsed() >= quiet {
                return json!({ "settled": true, "network_ms": ms(network), "held_by": held });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            if busy {
                network += tick.elapsed();
            }
        }
        json!({ "settled": false, "network_ms": ms(network), "held_by": held,
            "layout": last })
    }

    /// Follow a page the current one opened, such as a link with
    /// `target=_blank`.
    async fn adopt(&self, target_id: &str) {
        let Ok(s) = self.browser.attach(target_id).await else {
            return;
        };
        if s.enable().await.is_err() {
            return;
        }
        if let (Ok(mut page), Ok(mut stack)) = (self.page.lock(), self.stack.lock()) {
            stack.push(std::mem::replace(&mut *page, s));
        }
        if let Ok(mut st) = self.state.lock() {
            *st = PageState {
                downloads: std::mem::take(&mut st.downloads),
                ..PageState::default()
            };
        }
    }

    /// Return to the opener when the current page closes.
    fn drop_target(&self, target_id: &str) {
        if let (Ok(mut page), Ok(mut stack)) = (self.page.lock(), self.stack.lock()) {
            if page.target_id() == target_id {
                if let Some(prev) = stack.pop() {
                    *page = prev;
                }
            } else {
                stack.retain(|s| s.target_id() != target_id);
            }
        }
    }
}

async fn track(desk: std::sync::Weak<Desk>, mut events: tokio::sync::broadcast::Receiver<Event>) {
    loop {
        let ev = match events.recv().await {
            Ok(ev) => ev,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        let Some(desk) = desk.upgrade() else { return };
        let (current, main_frame) = {
            let page = desk.page();
            (page.id().to_owned(), page.target_id().to_owned())
        };
        let p = &ev.params;
        match ev.method.as_str() {
            "Target.targetCreated" => {
                let info = &p["targetInfo"];
                let opened_here = info["openerId"].as_str() == Some(desk.page().target_id());
                if info["type"] == "page" && opened_here {
                    if let Some(id) = info["targetId"].as_str() {
                        desk.adopt(id).await;
                    }
                }
            }
            "Target.targetDestroyed" => {
                if let Some(id) = p["targetId"].as_str() {
                    desk.drop_target(id);
                }
            }
            "Browser.downloadWillBegin" | "Browser.downloadProgress" => {
                if let (Some(guid), Ok(mut st)) = (p["guid"].as_str(), desk.state.lock()) {
                    let entry = st
                        .downloads
                        .entry(guid.to_owned())
                        .or_insert_with(|| json!({}));
                    for (k, v) in p.as_object().into_iter().flatten() {
                        entry[k] = v.clone();
                    }
                }
            }
            _ if ev.session.as_deref() != Some(current.as_str()) => {}
            // A page target's main frame shares its id.
            "Page.lifecycleEvent" if p["frameId"].as_str() == Some(main_frame.as_str()) => {
                if let Ok(mut st) = desk.state.lock() {
                    let loader = p["loaderId"].as_str().unwrap_or_default();
                    let name = p["name"].as_str().unwrap_or_default().to_owned();
                    if name == "init" || st.loader != loader {
                        st.loader = loader.to_owned();
                        st.lifecycle.clear();
                        // The old document's requests never finish.
                        st.inflight.retain(|_, (_, l, _)| l == loader);
                    }
                    st.lifecycle.push(name);
                }
            }
            "Network.requestWillBeSent" => {
                if let (Some(id), Ok(mut st)) = (p["requestId"].as_str(), desk.state.lock()) {
                    let url = p["request"]["url"].as_str().unwrap_or_default();
                    let loader = p["loaderId"].as_str().unwrap_or_default().to_owned();
                    st.inflight
                        .insert(id.to_owned(), (Instant::now(), loader, clip_url(url)));
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                if let (Some(id), Ok(mut st)) = (p["requestId"].as_str(), desk.state.lock()) {
                    st.inflight.remove(id);
                }
            }
            _ => {}
        }
    }
}
