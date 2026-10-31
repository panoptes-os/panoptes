//! The tools an agent drives its desktop with. Both front ends, the
//! internal socket protocol and MCP, call [`Desk::call`].
//!
//! Coordinates are output pixels, the pixels of every screenshot. Tools
//! that act wait for the screen to settle, then report it:
//!
//! ```text
//! click {x, y}
//!   -> input sent
//!   -> settle: damaged frames until none for quiet_ms, or settle_ms passes
//!   <- {settled_ms, timed_out, windows: {opened, closed, focused}} + PNG
//! ```

use crate::keymap::{Keymap, Step};
use crate::kiosk::Kiosk;
use crate::resd::Residency;
use crate::wl::{Frame, Window, Windows, Wl};
use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A tool's result: JSON for the caller, the screen when the tool shows
/// it, and how long the tool took.
#[derive(Debug)]
pub struct Output {
    pub result: Value,
    /// The screen as a base64 PNG.
    pub png: Option<String>,
    /// Everything before the screenshot: input, waiting, settling.
    pub action_ms: f64,
    /// Taking and encoding the screenshot.
    pub capture_ms: Option<f64>,
    /// Time to thaw or resume the kiosk first, when it was frozen.
    pub wake_ms: Option<f64>,
}

/// Defaults of the tools' optional arguments, as their schemas show them.
const QUIET_MS: u64 = 300;
const SETTLE_MS: u64 = 3000;
const TIMEOUT_MS: u64 = 10_000;
const SCROLL_NOTCHES: i32 = 3;

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e5).round() / 100.0
}

#[derive(Debug)]
pub struct Desk {
    wl: Wl,
    kiosk: Arc<Kiosk>,
    /// One tool at a time; the keymap the compositor holds goes with the
    /// turn.
    turn: tokio::sync::Mutex<Keymap>,
    residency: Option<Arc<Residency>>,
    /// Small captures for `panoptes top`, off unless set.
    thumb: Mutex<Option<Thumb>>,
}

/// Where and how often the desk writes a thumbnail: a base64 PNG at a
/// tenth of the output's size, replaced atomically, taken only right after
/// a tool call while the kiosk is already thawed.
#[derive(Debug)]
struct Thumb {
    path: PathBuf,
    every: Duration,
    last: Option<Instant>,
}

/// Name, description, and JSON Schema of every tool, shared by both front
/// ends.
pub fn tools() -> Vec<(&'static str, &'static str, Value)> {
    let x = json!({ "type": "integer", "description": "pixels from the screen's left edge" });
    let y = json!({ "type": "integer", "description": "pixels from the screen's top edge" });
    let button =
        json!({ "type": "string", "enum": ["left", "right", "middle"], "default": "left" });
    let window = json!({ "type": "integer", "description": "window id from windows" });
    let mut tools = vec![
        (
            "screenshot",
            "Capture the screen as a PNG. Its pixels are the coordinates every other tool takes.",
            json!({ "type": "object", "properties": {} }),
        ),
        (
            "windows",
            "List the open windows: id, pid, app_id, title, box (x, y, width, height), focus, \
and parent (the window a dialog belongs to). Also Xwayland's DISPLAY.",
            json!({ "type": "object", "properties": {} }),
        ),
    ];
    let acting = [
        (
            "click",
            "Move the pointer to {x, y} and click.",
            json!({ "type": "object", "required": ["x", "y"], "properties": {
                "x": x, "y": y, "button": button,
                "count": { "type": "integer", "minimum": 1, "maximum": 3, "default": 1 } } }),
        ),
        (
            "move",
            "Move the pointer to {x, y}, as for hovering.",
            json!({ "type": "object", "required": ["x", "y"], "properties": {
                "x": x, "y": y } }),
        ),
        (
            "drag",
            "Press a button at {x, y}, move to {to_x, to_y}, and release it there.",
            json!({ "type": "object", "required": ["x", "y", "to_x", "to_y"], "properties": {
                "x": x, "y": y, "to_x": x, "to_y": y, "button": button } }),
        ),
        (
            "scroll",
            "Turn the wheel over {x, y} by whole notches: dy positive scrolls down, dx positive right.",
            json!({ "type": "object", "required": ["x", "y"], "properties": {
                "x": x, "y": y,
                "dx": { "type": "integer", "default": 0 },
                "dy": { "type": "integer", "default": SCROLL_NOTCHES } } }),
        ),
        (
            "key",
            "Press a key or chord on the focused window: Return, Escape, Tab, BackSpace, Page_Down, \
F5, ctrl+s, ctrl+shift+t, alt+F4, super, ... Names are xkb keysym names, case-insensitive.",
            json!({ "type": "object", "required": ["key"], "properties": {
                "key": { "type": "string" } } }),
        ),
        (
            "type",
            "Type text into the focused window, any Unicode; newlines press Return.",
            json!({ "type": "object", "required": ["text"], "properties": {
                "text": { "type": "string" } } }),
        ),
        (
            "launch",
            "Start a program on this desktop, then wait up to timeout_ms for a new window. \
Returns its pid; output goes to a log, whose tail is in the error if it exits first.",
            json!({ "type": "object", "required": ["argv"], "properties": {
                "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1,
                    "description": "program and arguments, such as [\"gnome-calculator\"]" },
                "env": { "type": "object", "additionalProperties": { "type": "string" },
                    "description": "extra environment variables" },
                "timeout_ms": { "type": "integer", "default": TIMEOUT_MS } } }),
        ),
        (
            "focus",
            "Raise a window and give it keyboard focus.",
            json!({ "type": "object", "required": ["window"], "properties": {
                "window": window } }),
        ),
        (
            "close",
            "Ask a window to close, as its close button would; the app may refuse or ask first.",
            json!({ "type": "object", "required": ["window"], "properties": {
                "window": window } }),
        ),
        (
            "kill",
            "Kill a process of this desktop at once (SIGKILL), given a window or a pid.",
            json!({ "type": "object", "properties": {
                "window": window, "pid": { "type": "integer" } } }),
        ),
        (
            "wait",
            "Wait up to timeout_ms for a window matching every given filter to exist, or with \
closed, for none to; then for the screen to settle. With no filter, only settle.",
            json!({ "type": "object", "properties": {
                "app_id": { "type": "string" },
                "title": { "type": "string", "description": "part of the title" },
                "pid": { "type": "integer" },
                "closed": { "type": "boolean", "default": false },
                "timeout_ms": { "type": "integer", "default": TIMEOUT_MS } } }),
        ),
    ];
    // Every tool that acts also takes these.
    for (name, about, mut schema) in acting {
        let p = &mut schema["properties"];
        p["screenshot"] = json!({ "type": "boolean", "default": true,
            "description": "attach a screenshot taken once the screen settles" });
        p["quiet_ms"] = json!({ "type": "integer", "default": QUIET_MS,
            "description": "the screen has settled after this long without a change" });
        p["settle_ms"] = json!({ "type": "integer", "default": SETTLE_MS,
            "description": "stop waiting for the screen to settle after this long" });
        tools.push((name, about, schema));
    }
    tools
}

/// Argument `k`: none when absent or null, an error when `get` rejects it.
/// A mistyped argument never falls back to its default.
fn arg<'a, T>(
    a: &'a Value,
    k: &str,
    what: &str,
    get: impl FnOnce(&'a Value) -> Option<T>,
) -> Result<Option<T>, String> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => get(v)
            .map(Some)
            .ok_or_else(|| format!("{k} must be {what}")),
    }
}

fn arg_u64(a: &Value, k: &str, default: u64) -> Result<u64, String> {
    Ok(arg(a, k, "a whole number", Value::as_u64)?.unwrap_or(default))
}

fn arg_ms(a: &Value, k: &str, default: u64) -> Result<Duration, String> {
    arg_u64(a, k, default).map(Duration::from_millis)
}

fn arg_bool(a: &Value, k: &str, default: bool) -> Result<bool, String> {
    Ok(arg(a, k, "true or false", Value::as_bool)?.unwrap_or(default))
}

fn arg_str<'a>(a: &'a Value, k: &str) -> Result<&'a str, String> {
    arg(a, k, "a string", Value::as_str)?.ok_or_else(|| format!("{k} is required"))
}

fn arg_id(a: &Value, k: &str) -> Result<u32, String> {
    let id = arg(a, k, "a window id", |v| {
        v.as_u64().and_then(|v| u32::try_from(v).ok())
    })?;
    id.ok_or_else(|| format!("{k} is required"))
}

/// Linux input event codes of the pointer buttons.
fn arg_button(a: &Value) -> Result<u32, String> {
    match arg(a, "button", "a string", Value::as_str)? {
        None | Some("left") => Ok(0x110),
        Some("right") => Ok(0x111),
        Some("middle") => Ok(0x112),
        Some(other) => Err(format!("unknown button {other}")),
    }
}

fn window_json(w: &Window) -> Value {
    json!({ "id": w.id, "pid": w.pid, "app_id": w.app_id, "title": w.title,
        "x": w.x, "y": w.y, "width": w.width, "height": w.height,
        "focused": w.focused, "parent": w.parent })
}

/// How the screen settled after an action.
#[derive(Debug)]
struct Settled {
    ms: f64,
    timed_out: bool,
    /// The newest frame, when no change can have followed it.
    frame: Option<Frame>,
}

impl Desk {
    pub async fn new(
        wl: Wl,
        kiosk: Arc<Kiosk>,
        residency: Option<Arc<Residency>>,
    ) -> Result<Arc<Desk>, String> {
        let keymap = Keymap::default();
        wl.keymap(&keymap.text()).await?;
        Ok(Arc::new(Desk {
            wl,
            kiosk,
            turn: tokio::sync::Mutex::new(keymap),
            residency,
            thumb: Mutex::new(None),
        }))
    }

    /// Run one tool. With a residency manager, the kiosk is acquired first
    /// and released once calls stop for the grace period.
    pub async fn call(&self, tool: &str, args: &Value) -> Result<Output, String> {
        let woke = match &self.residency {
            Some(res) => Some(res.acquire().await),
            None => None,
        };
        let out = {
            let mut keymap = self.turn.lock().await;
            let out = self.call_tool(&mut keymap, tool, args).await;
            self.thumb_if_due().await;
            out
        };
        if let Some(res) = &self.residency {
            res.release_later();
        }
        out.map(|mut o| {
            o.wake_ms = woke.filter(|w| w.woke).map(|w| w.ms);
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
        let Ok(frame) = self.wl.screenshot().await else {
            return;
        };
        let Ok(png) = encode_png(&shrink(&frame, 10)) else {
            return;
        };
        let tmp = path.with_extension("tmp");
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        if std::fs::write(&tmp, b64).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    async fn call_tool(
        &self,
        keymap: &mut Keymap,
        tool: &str,
        args: &Value,
    ) -> Result<Output, String> {
        self.wl.alive()?;
        let start = Instant::now();
        match tool {
            "screenshot" => {
                let frame = self.wl.screenshot().await?;
                let result = json!({ "width": frame.width, "height": frame.height });
                output(result, Some((frame, start)), start)
            }
            "windows" => {
                let w = self.wl.windows();
                let list: Vec<Value> = w.list.iter().map(window_json).collect();
                let result = json!({ "xwayland": w.xwayland, "windows": list });
                output(result, None, start)
            }
            _ => {
                let screenshot = arg_bool(args, "screenshot", true)?;
                let quiet = arg_ms(args, "quiet_ms", QUIET_MS)?;
                let limit = arg_ms(args, "settle_ms", SETTLE_MS)?;

                let before = self.wl.windows();
                let mut result = self.act(keymap, tool, args, &before).await?;
                let settled = self.settle(quiet, limit).await?;
                self.report(&mut result, &settled, &before);

                let shot = Instant::now();
                let frame = match (screenshot, settled.frame) {
                    (false, _) => None,
                    (true, Some(f)) => Some(f),
                    (true, None) => Some(self.wl.screenshot().await?),
                };
                output(result, frame.map(|f| (f, shot)), start)
            }
        }
    }

    /// Do what an acting tool asks; return its own result fields.
    async fn act(
        &self,
        keymap: &mut Keymap,
        tool: &str,
        args: &Value,
        before: &Windows,
    ) -> Result<Value, String> {
        let wl = &self.wl;
        match tool {
            "click" => {
                let (x, y) = self.point(args, "x", "y")?;
                let button = arg_button(args)?;
                let count = arg_u64(args, "count", 1)?.clamp(1, 3);
                wl.motion(x, y);
                for _ in 0..count {
                    wl.button(button, true);
                    wl.button(button, false);
                }
                wl.flush()?;
            }
            "move" => {
                let (x, y) = self.point(args, "x", "y")?;
                wl.motion(x, y);
                wl.flush()?;
            }
            "drag" => {
                let (x, y) = self.point(args, "x", "y")?;
                let (to_x, to_y) = self.point(args, "to_x", "to_y")?;
                let button = arg_button(args)?;
                // Toolkits start a drag on motion after the press, so move
                // in steps, at about a frame each.
                const STEPS: u32 = 10;
                wl.motion(x, y);
                wl.button(button, true);
                wl.flush()?;
                for i in 1..=STEPS {
                    tokio::time::sleep(Duration::from_millis(16)).await;
                    let lerp = |a: u32, b: u32| {
                        (f64::from(a)
                            + (f64::from(b) - f64::from(a)) * f64::from(i) / f64::from(STEPS))
                        .round() as u32
                    };
                    wl.motion(lerp(x, to_x), lerp(y, to_y));
                    wl.flush()?;
                }
                wl.button(button, false);
                wl.flush()?;
            }
            "scroll" => {
                let (x, y) = self.point(args, "x", "y")?;
                let notches = |k: &str, default: i32| {
                    let n = arg(args, k, "a whole number of notches", |v| {
                        v.as_i64().and_then(|n| i32::try_from(n).ok())
                    })?;
                    Ok::<_, String>(n.unwrap_or(default))
                };
                let (dx, dy) = (notches("dx", 0)?, notches("dy", SCROLL_NOTCHES)?);
                wl.motion(x, y);
                wl.wheel(dx, dy);
                wl.flush()?;
            }
            "key" => {
                let events = keymap.chord(arg_str(args, "key")?)?;
                wl.keys(&events);
                wl.flush()?;
            }
            "type" => {
                for step in keymap.type_text(arg_str(args, "text")?)? {
                    match step {
                        Step::Keymap(text) => wl.keymap(&text).await?,
                        Step::Keys(events) => {
                            // A sync per chunk keeps long text from
                            // overrunning the socket.
                            for chunk in events.chunks(256) {
                                wl.keys(chunk);
                                wl.sync().await?;
                            }
                        }
                    }
                }
            }
            "launch" => return self.launch(args, before).await,
            "focus" => wl.activate(arg_id(args, "window")?)?,
            "close" => wl.close(arg_id(args, "window")?)?,
            "kill" => {
                let pid = match (args.get("window"), args.get("pid")) {
                    (Some(_), None) => {
                        let id = arg_id(args, "window")?;
                        let w = before.list.iter().find(|w| w.id == id);
                        let pid = w.ok_or_else(|| format!("no window {id}"))?.pid;
                        u32::try_from(pid)
                            .ok()
                            .filter(|&p| p > 0)
                            .ok_or_else(|| format!("window {id} has no known pid"))?
                    }
                    (None, Some(p)) => p
                        .as_u64()
                        .and_then(|p| u32::try_from(p).ok())
                        .ok_or("pid must be a process id")?,
                    _ => return Err("kill takes a window or a pid".into()),
                };
                self.kiosk.kill(pid)?;
                return Ok(json!({ "pid": pid }));
            }
            "wait" => return self.wait(args).await,
            other => return Err(format!("unknown tool {other}")),
        }
        Ok(json!({}))
    }

    /// A screen point from two arguments, checked against the output.
    fn point(&self, args: &Value, kx: &str, ky: &str) -> Result<(u32, u32), String> {
        let (w, h) = self.wl.size();
        let coord = |k: &str, max: u32| {
            let v = args.get(k).and_then(Value::as_f64);
            let v = v.ok_or_else(|| format!("{k} must be a number"))?.round();
            if (0.0..f64::from(max)).contains(&v) {
                Ok(v as u32)
            } else {
                Err(format!("{k}={v} is off the {w}x{h} screen"))
            }
        };
        Ok((coord(kx, w)?, coord(ky, h)?))
    }

    /// Start a program and wait for a new window, its exit, or the timeout.
    async fn launch(&self, args: &Value, before: &Windows) -> Result<Value, String> {
        let argv: Vec<String> = args
            .get("argv")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().map(|s| s.as_str().map(str::to_owned)).collect())
            .filter(|a: &Vec<String>| !a.is_empty())
            .ok_or("argv must be a non-empty array of strings")?;
        let env: Vec<(String, String)> = match args.get("env") {
            None => Vec::new(),
            Some(Value::Object(m)) => m
                .iter()
                .map(|(k, v)| match v.as_str() {
                    Some(v) => Ok((k.clone(), v.to_owned())),
                    None => Err(format!("env.{k} must be a string")),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("env must be an object of strings".into()),
        };
        let timeout = arg_ms(args, "timeout_ms", TIMEOUT_MS)?;

        let mut changes = self.wl.window_changes();
        let (child, log) = self.kiosk.launch(&argv, &env, before.xwayland.as_deref())?;
        let pid = child.pid();
        let (exit_tx, mut exit_rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = exit_tx.send(child.wait());
        });

        let deadline = tokio::time::Instant::now() + timeout;
        let mut exited = false;
        loop {
            let opened = self
                .wl
                .windows()
                .list
                .iter()
                .any(|w| !before.list.iter().any(|b| b.id == w.id));
            if opened {
                break;
            }
            tokio::select! {
                _ = changes.changed() => {}
                status = &mut exit_rx, if !exited => {
                    exited = true;
                    let status = status.ok().and_then(Result::ok);
                    // A launcher may exit cleanly once its app is up.
                    if status != Some(0) {
                        return Err(format!(
                            "{} exited ({}) before opening a window; {}:\n{}",
                            argv[0],
                            describe_status(status),
                            log.display(),
                            tail(&log, 20),
                        ));
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        Ok(json!({ "pid": pid }))
    }

    /// Wait until a window matches the filters (or, with closed, none
    /// does), or the timeout.
    async fn wait(&self, args: &Value) -> Result<Value, String> {
        let app_id = arg(args, "app_id", "a string", Value::as_str)?;
        let title = arg(args, "title", "a string", Value::as_str)?;
        let pid = arg(args, "pid", "a whole number", Value::as_i64)?;
        let closed = arg_bool(args, "closed", false)?;
        let timeout = arg_ms(args, "timeout_ms", TIMEOUT_MS)?;
        if app_id.is_none() && title.is_none() && pid.is_none() {
            return Ok(json!({}));
        }
        let matches = |w: &Window| {
            app_id.is_none_or(|a| w.app_id == a)
                && title.is_none_or(|t| w.title.contains(t))
                && pid.is_none_or(|p| i64::from(w.pid) == p)
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let mut changes = self.wl.window_changes();
        loop {
            let found = self.wl.windows().list.into_iter().find(|w| matches(w));
            let met = found.is_some() != closed;
            if met {
                return Ok(json!({ "met": true, "window": found.as_ref().map(window_json) }));
            }
            tokio::select! {
                _ = changes.changed() => {}
                _ = tokio::time::sleep_until(deadline) => {
                    return Ok(json!({ "met": false, "window": found.as_ref().map(window_json) }));
                }
            }
            self.wl.alive()?;
        }
    }

    /// Add how the screen settled and the window changes since `before` to
    /// an acting tool's result.
    fn report(&self, result: &mut Value, settled: &Settled, before: &Windows) {
        let now = self.wl.windows();
        let opened: Vec<Value> = now
            .list
            .iter()
            .filter(|w| !before.list.iter().any(|b| b.id == w.id))
            .map(window_json)
            .collect();
        let closed: Vec<u32> = before
            .list
            .iter()
            .filter(|b| !now.list.iter().any(|w| w.id == b.id))
            .map(|b| b.id)
            .collect();
        let focused = now.list.iter().find(|w| w.focused).map(|w| w.id);
        result["settled_ms"] = json!(settled.ms);
        result["timed_out"] = json!(settled.timed_out);
        result["windows"] = json!({ "opened": opened, "closed": closed, "focused": focused });
    }

    /// Wait until no frame changes the screen for `quiet`, at most `limit`.
    async fn settle(&self, quiet: Duration, limit: Duration) -> Result<Settled, String> {
        let start = Instant::now();
        let mut last = None;
        loop {
            let left = limit.saturating_sub(start.elapsed());
            let window = quiet.min(left);
            match self.wl.next_change(window).await? {
                Some(frame) => last = Some(frame),
                // A full quiet window passed without a change, so the last
                // changed frame is still what the screen shows.
                None if window == quiet => {
                    return Ok(Settled {
                        ms: ms(start.elapsed()),
                        timed_out: false,
                        frame: last,
                    });
                }
                None => {}
            }
            if start.elapsed() >= limit {
                return Ok(Settled {
                    ms: ms(start.elapsed()),
                    timed_out: true,
                    frame: None,
                });
            }
        }
    }
}

/// A tool's output. `shot` is the screen and when taking it began; the
/// action is everything before that.
fn output(result: Value, shot: Option<(Frame, Instant)>, start: Instant) -> Result<Output, String> {
    let Some((frame, taken)) = shot else {
        return Ok(Output {
            result,
            png: None,
            action_ms: ms(start.elapsed()),
            capture_ms: None,
            wake_ms: None,
        });
    };
    let png = encode_png(&frame)?;
    Ok(Output {
        result,
        png: Some(base64::engine::general_purpose::STANDARD.encode(png)),
        action_ms: ms(taken.duration_since(start)),
        capture_ms: Some(ms(taken.elapsed())),
        wake_ms: None,
    })
}

fn encode_png(frame: &Frame) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, frame.width, frame.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().map_err(|e| format!("png: {e}"))?;
    w.write_image_data(&frame.rgb)
        .map_err(|e| format!("png: {e}"))?;
    w.finish().map_err(|e| format!("png: {e}"))?;
    Ok(out)
}

/// Average `factor` x `factor` blocks into one pixel.
fn shrink(frame: &Frame, factor: u32) -> Frame {
    let (w, h) = (
        (frame.width / factor).max(1),
        (frame.height / factor).max(1),
    );
    let f = factor as usize;
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for ty in 0..h as usize {
        for tx in 0..w as usize {
            let mut sum = [0u32; 3];
            for y in ty * f..((ty + 1) * f).min(frame.height as usize) {
                for x in tx * f..((tx + 1) * f).min(frame.width as usize) {
                    let i = (y * frame.width as usize + x) * 3;
                    for (s, &v) in sum.iter_mut().zip(&frame.rgb[i..i + 3]) {
                        *s += u32::from(v);
                    }
                }
            }
            let n = (f * f) as u32;
            rgb.extend(sum.map(|s| (s / n) as u8));
        }
    }
    Frame {
        width: w,
        height: h,
        rgb,
    }
}

fn describe_status(status: Option<i32>) -> String {
    match status {
        None => "status unknown".into(),
        Some(s) if s & 0x7f == 0 => format!("exit code {}", (s >> 8) & 0xff),
        Some(s) => format!("signal {}", s & 0x7f),
    }
}

/// The last `n` lines of a log.
fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
