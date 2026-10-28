//! The CDP commands Panoptes uses, with just enough typing to call them.
//!
//! Hand-written for Chromium 156; a generator from that build's
//! `browser_protocol.pdl` replaces this module once more commands are needed.

use crate::{Cdp, Error, Result};
use serde_json::{Value, json};
use std::collections::HashMap;

fn shape(method: &'static str, what: &'static str) -> Error {
    Error::Shape { method, what }
}

#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub kind: String,
    pub url: String,
}

/// Browser-level commands.
impl Cdp {
    pub async fn version(&self) -> Result<String> {
        let v = self.call(None, "Browser.getVersion", json!({})).await?;
        Ok(v["product"].as_str().unwrap_or_default().to_owned())
    }

    pub async fn targets(&self) -> Result<Vec<Target>> {
        let v = self.call(None, "Target.getTargets", json!({})).await?;
        let infos = v["targetInfos"]
            .as_array()
            .ok_or_else(|| shape("Target.getTargets", "targetInfos"))?;
        Ok(infos
            .iter()
            .map(|t| Target {
                id: t["targetId"].as_str().unwrap_or_default().to_owned(),
                kind: t["type"].as_str().unwrap_or_default().to_owned(),
                url: t["url"].as_str().unwrap_or_default().to_owned(),
            })
            .collect())
    }

    /// Attach to a target in flat mode; commands then carry the session id.
    pub async fn attach(&self, target_id: &str) -> Result<Session> {
        let v = self
            .call(
                None,
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .await?;
        let id = v["sessionId"]
            .as_str()
            .ok_or_else(|| shape("Target.attachToTarget", "sessionId"))?;
        Ok(Session {
            cdp: self.clone(),
            id: id.to_owned(),
            target_id: target_id.to_owned(),
        })
    }

    pub async fn discover_targets(&self) -> Result<()> {
        self.call(
            None,
            "Target.setDiscoverTargets",
            json!({ "discover": true }),
        )
        .await
        .map(drop)
    }

    /// Save downloads to `dir` under their suggested names, with progress
    /// events.
    pub async fn set_download_dir(&self, dir: &str) -> Result<()> {
        self.call(
            None,
            "Browser.setDownloadBehavior",
            json!({ "behavior": "allow", "downloadPath": dir, "eventsEnabled": true }),
        )
        .await
        .map(drop)
    }

    pub async fn close_browser(&self) -> Result<()> {
        self.call(None, "Browser.close", json!({})).await.map(drop)
    }
}

/// One attached page.
#[derive(Debug, Clone)]
pub struct Session {
    cdp: Cdp,
    id: String,
    target_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
}

impl ImageFormat {
    pub fn mime(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Png => "image/png",
        }
    }
}

/// The visual viewport, in CSS pixels; `x` and `y` are its page offset.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Which `<input type=file>` `set_file_input` fills.
#[derive(Debug, Clone, Copy)]
pub enum FileInput<'a> {
    Node(i64),
    Selector(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Moved,
    Pressed,
    Released,
    Wheel,
}

impl MouseKind {
    fn name(self) -> &'static str {
        match self {
            MouseKind::Moved => "mouseMoved",
            MouseKind::Pressed => "mousePressed",
            MouseKind::Released => "mouseReleased",
            MouseKind::Wheel => "mouseWheel",
        }
    }
}

/// One accessibility node, as `Accessibility.getFullAXTree` returns it.
#[derive(Debug, Clone)]
pub struct AxNode {
    pub id: String,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub ignored: bool,
    pub role: String,
    pub name: String,
    pub value: String,
    pub backend_id: Option<i64>,
    /// Boolean and token properties worth showing: focused, checked,
    /// disabled, expanded, required, and the like.
    pub states: Vec<String>,
}

fn ax_text(v: &Value) -> String {
    match &v["value"] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.cdp.call(Some(&self.id), method, params).await
    }

    /// Enable the domains whose events the desk tracks.
    pub async fn enable(&self) -> Result<()> {
        for m in [
            "Page.enable",
            "Network.enable",
            "DOM.enable",
            "Accessibility.enable",
        ] {
            self.call(m, json!({})).await?;
        }
        self.call("Page.setLifecycleEventsEnabled", json!({ "enabled": true }))
            .await
            .map(drop)
    }

    /// Navigate; returns the new loader id, or the browser's error text.
    pub async fn navigate(&self, url: &str) -> Result<std::result::Result<String, String>> {
        let v = self.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = v["errorText"].as_str().filter(|e| !e.is_empty()) {
            return Ok(Err(err.to_owned()));
        }
        Ok(Ok(v["loaderId"].as_str().unwrap_or_default().to_owned()))
    }

    pub async fn viewport(&self) -> Result<Viewport> {
        let v = self.call("Page.getLayoutMetrics", json!({})).await?;
        let vv = &v["cssVisualViewport"];
        Ok(Viewport {
            x: vv["pageX"].as_f64().unwrap_or(0.0),
            y: vv["pageY"].as_f64().unwrap_or(0.0),
            width: vv["clientWidth"]
                .as_f64()
                .ok_or_else(|| shape("Page.getLayoutMetrics", "clientWidth"))?,
            height: vv["clientHeight"]
                .as_f64()
                .ok_or_else(|| shape("Page.getLayoutMetrics", "clientHeight"))?,
        })
    }

    /// Capture the visible viewport at `scale`; returns base64 image data.
    pub async fn screenshot(
        &self,
        vp: Viewport,
        format: ImageFormat,
        quality: u8,
        scale: f64,
    ) -> Result<String> {
        let mut p = json!({
            "format": if format == ImageFormat::Jpeg { "jpeg" } else { "png" },
            "clip": { "x": vp.x, "y": vp.y, "width": vp.width, "height": vp.height, "scale": scale },
            "optimizeForSpeed": true,
        });
        if format == ImageFormat::Jpeg {
            p["quality"] = json!(quality);
        }
        let v = self.call("Page.captureScreenshot", p).await?;
        v["data"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| shape("Page.captureScreenshot", "data"))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn mouse(
        &self,
        kind: MouseKind,
        x: f64,
        y: f64,
        button: &str,
        click_count: u32,
        delta: (f64, f64),
    ) -> Result<()> {
        let mut p = json!({ "type": kind.name(), "x": x, "y": y });
        if kind != MouseKind::Moved && kind != MouseKind::Wheel {
            p["button"] = json!(button);
            p["clickCount"] = json!(click_count);
        }
        if kind == MouseKind::Wheel {
            p["deltaX"] = json!(delta.0);
            p["deltaY"] = json!(delta.1);
        }
        self.call("Input.dispatchMouseEvent", p).await.map(drop)
    }

    pub async fn key_event(&self, params: Value) -> Result<()> {
        self.call("Input.dispatchKeyEvent", params).await.map(drop)
    }

    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.call("Input.insertText", json!({ "text": text }))
            .await
            .map(drop)
    }

    /// Evaluate an expression and return its value by value.
    pub async fn evaluate(&self, expr: &str) -> Result<Value> {
        let v = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(ex) = v.get("exceptionDetails") {
            return Err(Error::Protocol {
                method: "Runtime.evaluate".into(),
                code: 0,
                message: ex["text"].as_str().unwrap_or("exception").to_owned(),
            });
        }
        Ok(v["result"]["value"].clone())
    }

    pub async fn ax_tree(&self) -> Result<Vec<AxNode>> {
        let v = self.call("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = v["nodes"]
            .as_array()
            .ok_or_else(|| shape("Accessibility.getFullAXTree", "nodes"))?;
        Ok(nodes
            .iter()
            .map(|n| {
                let mut states = Vec::new();
                for p in n["properties"].as_array().into_iter().flatten() {
                    let name = p["name"].as_str().unwrap_or_default();
                    let val = &p["value"]["value"];
                    match val {
                        Value::Bool(true) => states.push(name.to_owned()),
                        Value::String(s)
                            if matches!(
                                name,
                                "checked" | "pressed" | "invalid" | "autocomplete"
                            ) && s != "false" =>
                        {
                            states.push(format!("{name}={s}"));
                        }
                        _ => {}
                    }
                }
                AxNode {
                    id: n["nodeId"].as_str().unwrap_or_default().to_owned(),
                    parent: n["parentId"].as_str().map(str::to_owned),
                    children: n["childIds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c.as_str().map(str::to_owned))
                        .collect(),
                    ignored: n["ignored"].as_bool().unwrap_or(false),
                    role: ax_text(&n["role"]),
                    name: ax_text(&n["name"]),
                    value: ax_text(&n["value"]),
                    backend_id: n["backendDOMNodeId"].as_i64(),
                    states,
                }
            })
            .collect())
    }

    /// Layout boxes of every node in the main document, in viewport
    /// coordinates, keyed by backend node id. One call for the whole page.
    pub async fn layout_boxes(&self) -> Result<HashMap<i64, [f64; 4]>> {
        let v = self
            .call(
                "DOMSnapshot.captureSnapshot",
                json!({ "computedStyles": [], "includeDOMRects": false }),
            )
            .await?;
        let mut out = HashMap::new();
        let Some(doc) = v["documents"].as_array().and_then(|d| d.first()) else {
            return Ok(out);
        };
        let sx = doc["scrollOffsetX"].as_f64().unwrap_or(0.0);
        let sy = doc["scrollOffsetY"].as_f64().unwrap_or(0.0);
        let backend = doc["nodes"]["backendNodeId"].as_array();
        let idx = doc["layout"]["nodeIndex"].as_array();
        let bounds = doc["layout"]["bounds"].as_array();
        if let (Some(backend), Some(idx), Some(bounds)) = (backend, idx, bounds) {
            for (i, b) in idx.iter().zip(bounds) {
                let (Some(i), Some(b)) = (i.as_u64(), b.as_array()) else {
                    continue;
                };
                let Some(id) = backend.get(i as usize).and_then(Value::as_i64) else {
                    continue;
                };
                let f = |k: usize| b.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                out.entry(id).or_insert([f(0) - sx, f(1) - sy, f(2), f(3)]);
            }
        }
        Ok(out)
    }

    pub async fn scroll_into_view(&self, backend_id: i64) -> Result<()> {
        self.call(
            "DOM.scrollIntoViewIfNeeded",
            json!({ "backendNodeId": backend_id }),
        )
        .await
        .map(drop)
    }

    /// Put `files` (paths on this host) into an `<input type=file>`, given
    /// by backend node id or by CSS selector. Chromium fires the input's
    /// change event as if the user picked them.
    pub async fn set_file_input(&self, target: FileInput<'_>, files: &[String]) -> Result<()> {
        let mut p = json!({ "files": files });
        match target {
            FileInput::Node(id) => p["backendNodeId"] = json!(id),
            FileInput::Selector(sel) => {
                let expr = format!("document.querySelector({})", json!(sel));
                let v = self
                    .call("Runtime.evaluate", json!({ "expression": expr }))
                    .await?;
                let obj = v["result"]["objectId"]
                    .as_str()
                    .ok_or_else(|| shape("Runtime.evaluate", "an element matching the selector"))?;
                p["objectId"] = json!(obj);
            }
        }
        self.call("DOM.setFileInputFiles", p).await.map(drop)
    }

    /// Reload the page, as F5 would.
    pub async fn reload(&self) -> Result<()> {
        self.call("Page.reload", json!({})).await.map(drop)
    }

    pub async fn focus(&self, backend_id: i64) -> Result<()> {
        self.call("DOM.focus", json!({ "backendNodeId": backend_id }))
            .await
            .map(drop)
    }

    /// Center of the node's first content quad, in viewport coordinates.
    pub async fn node_center(&self, backend_id: i64) -> Result<(f64, f64)> {
        let v = self
            .call(
                "DOM.getContentQuads",
                json!({ "backendNodeId": backend_id }),
            )
            .await?;
        let q = v["quads"]
            .as_array()
            .and_then(|qs| qs.first())
            .and_then(Value::as_array)
            .filter(|q| q.len() == 8)
            .ok_or_else(|| shape("DOM.getContentQuads", "a quad (node not rendered)"))?;
        let c: Vec<f64> = q.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
        Ok((
            (c[0] + c[2] + c[4] + c[6]) / 4.0,
            (c[1] + c[3] + c[5] + c[7]) / 4.0,
        ))
    }
}
