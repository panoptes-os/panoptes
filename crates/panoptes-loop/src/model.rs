//! An OpenAI-compatible chat client shared by every agent in the process.
//!
//! One `reqwest` client keeps connections open (HTTP/2 where the server
//! offers it through ALPN), a semaphore caps requests in flight per
//! provider, and each reply streams as server-sent events so the loop can
//! time the first token separately from the rest.

use crate::config::{Budget, Price, Provider};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub struct Model {
    http: reqwest::Client,
    url: String,
    model: String,
    key: Option<String>,
    gate: Arc<Semaphore>,
    retries: u32,
    json_schema: bool,
    max_tokens: Option<u32>,
    temperature: Option<f64>,
    extra: serde_json::Map<String, Value>,
    json_mode: bool,
    api_key_auth: bool,
    price: Option<Price>,
    ledger: Option<Ledger>,
}

/// Dollars spent so far, kept in a file so a cap holds across runs.
struct Ledger {
    budget: Budget,
    spent: std::sync::Mutex<f64>,
}

impl Ledger {
    fn open(budget: &Budget) -> Result<Self, String> {
        let spent = match std::fs::read_to_string(&budget.ledger) {
            Ok(s) => s.trim().parse().map_err(|e| format!("ledger {}: {e}", budget.ledger.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0.0,
            Err(e) => return Err(format!("ledger {}: {e}", budget.ledger.display())),
        };
        Ok(Ledger { budget: budget.clone(), spent: std::sync::Mutex::new(spent) })
    }

    fn spent(&self) -> f64 {
        self.spent.lock().map(|s| *s).unwrap_or(f64::INFINITY)
    }

    fn add(&self, usd: f64) -> f64 {
        let Ok(mut s) = self.spent.lock() else { return f64::INFINITY };
        *s += usd;
        let _ = std::fs::write(&self.budget.ledger, format!("{:.6}\n", *s));
        *s
    }
}

/// One completed model call.
#[derive(Debug, Default)]
pub struct Completion {
    pub text: String,
    /// Waiting for a free slot under the provider's concurrency cap.
    pub queue_ms: f64,
    /// Request sent to first content byte.
    pub ttft_ms: f64,
    /// First content byte to end of stream.
    pub decode_ms: f64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    /// This call's price from its usage, when the provider has one.
    pub cost_usd: Option<f64>,
    /// The ledger's total after this call.
    pub spent_usd: Option<f64>,
    pub attempts: u32,
}

impl Price {
    /// Cached tokens are part of the prompt tokens and bill at their own rate.
    fn cost(&self, prompt: u64, cached: u64, completion: u64) -> f64 {
        let fresh = prompt.saturating_sub(cached) as f64;
        (fresh * self.input + cached as f64 * self.cached + completion as f64 * self.output) / 1e6
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e5).round() / 100.0
}

impl Model {
    pub fn new(p: &Provider, budget: Option<&Budget>) -> Result<Self, String> {
        let key = match &p.key_file {
            Some(f) => Some(
                std::fs::read_to_string(f)
                    .map_err(|e| format!("key file {}: {e}", f.display()))?
                    .trim()
                    .to_owned(),
            ),
            None => None,
        };
        let mut http = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(p.max_concurrency.max(1))
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(20))
            .http2_keep_alive_while_idle(true)
            .timeout(Duration::from_secs(p.timeout_s));
        if p.http2_prior_knowledge {
            http = http.http2_prior_knowledge();
        }
        let http = http
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Model {
            http,
            url: format!("{}/chat/completions", p.base_url.trim_end_matches('/')),
            model: p.model.clone(),
            key,
            gate: Arc::new(Semaphore::new(p.max_concurrency.max(1))),
            retries: p.retries,
            json_schema: p.json_schema,
            max_tokens: p.max_tokens,
            temperature: p.temperature,
            extra: p.extra.clone(),
            json_mode: p.json_mode,
            api_key_auth: p.auth == "api-key",
            price: p.price,
            ledger: budget.map(Ledger::open).transpose()?,
        })
    }

    pub async fn complete(&self, messages: &[Value], schema: &Value) -> Result<Completion, String> {
        let queued = Instant::now();
        let _permit = self.gate.acquire().await.map_err(|e| e.to_string())?;
        let queue_ms = ms(queued.elapsed());
        if let Some(l) = &self.ledger
            && l.spent() >= l.budget.cap_usd
        {
            return Err(format!("budget: ${:.4} spent, cap ${}", l.spent(), l.budget.cap_usd));
        }
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if let Some(t) = self.max_tokens {
            body["max_tokens"] = json!(t);
        }
        if let Some(t) = self.temperature {
            body["temperature"] = json!(t);
        }
        if self.json_schema {
            body["response_format"] = json!({ "type": "json_schema", "json_schema": {
                "name": "actions", "strict": true, "schema": schema } });
        } else if self.json_mode {
            body["response_format"] = json!({ "type": "json_object" });
        }
        for (k, v) in &self.extra {
            body[k] = v.clone();
        }
        let mut last = String::new();
        for attempt in 1..=self.retries.max(1) {
            match self.once(&body).await {
                Ok(mut c) => {
                    c.queue_ms = queue_ms;
                    c.attempts = attempt;
                    return Ok(c);
                }
                Err((retry, after, e)) => {
                    last = e;
                    if !retry {
                        break;
                    }
                    // A rate limit's Retry-After, else doubling from 0.5 s
                    // to a minute.
                    let back = Duration::from_millis(250 << attempt.min(8));
                    tokio::time::sleep(after.map_or(back, |a| a.max(back))).await;
                }
            }
        }
        Err(last)
    }

    fn charge(&self, c: &mut Completion) {
        let Some(price) = &self.price else { return };
        let (Some(p), Some(o)) = (c.prompt_tokens, c.completion_tokens) else { return };
        let usd = price.cost(p, c.cached_tokens.unwrap_or(0), o);
        c.cost_usd = Some(usd);
        c.spent_usd = self.ledger.as_ref().map(|l| l.add(usd));
    }

    /// One request. The error says whether a retry could help, and how long
    /// the server asked to wait.
    async fn once(&self, body: &Value) -> Result<Completion, (bool, Option<Duration>, String)> {
        let mut req = self.http.post(&self.url).json(body);
        if let Some(k) = &self.key {
            req = if self.api_key_auth {
                req.header(reqwest::header::AUTHORIZATION, format!("Api-Key {k}"))
            } else {
                req.bearer_auth(k)
            };
        }
        let sent = Instant::now();
        let resp = req.send().await.map_err(|e| (true, None, format!("request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let retry = status.as_u16() == 429 || status.is_server_error();
            let after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map(|s| Duration::from_secs_f64(s.clamp(0.0, 120.0)));
            let text = resp.text().await.unwrap_or_default();
            return Err((retry, after, format!("HTTP {status}: {}", text.chars().take(400).collect::<String>())));
        }
        let mut c = Completion::default();
        let mut first: Option<Instant> = None;
        let mut buf = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| (true, None, format!("stream: {e}")))?;
            buf.extend_from_slice(&chunk);
            while let Some(end) = find(&buf, b"\n\n") {
                let event: Vec<u8> = buf.drain(..end + 2).collect();
                for line in String::from_utf8_lossy(&event).lines() {
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data == "[DONE]" {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                    // Reasoning streams before the content; the first token
                    // of either starts decode.
                    let delta = &v["choices"][0]["delta"];
                    if ["reasoning_content", "reasoning"].iter().any(|k| delta[k].as_str().is_some_and(|t| !t.is_empty())) {
                        first.get_or_insert_with(Instant::now);
                    }
                    if let Some(t) = v["choices"][0]["delta"]["content"].as_str()
                        && !t.is_empty()
                    {
                        first.get_or_insert_with(Instant::now);
                        c.text.push_str(t);
                    }
                    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                        c.prompt_tokens = u["prompt_tokens"].as_u64();
                        c.completion_tokens = u["completion_tokens"].as_u64();
                        c.cached_tokens = u["prompt_tokens_details"]["cached_tokens"].as_u64();
                        c.reasoning_tokens = u["completion_tokens_details"]["reasoning_tokens"].as_u64();
                    }
                }
            }
        }
        let end = Instant::now();
        let first = first.unwrap_or(end);
        c.ttft_ms = ms(first - sent);
        c.decode_ms = ms(end - first);
        // An empty reply is still billed.
        self.charge(&mut c);
        if c.text.is_empty() {
            return Err((true, None, "empty completion".into()));
        }
        Ok(c)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
