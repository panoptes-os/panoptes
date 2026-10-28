//! An OpenAI-compatible chat client shared by every agent in the process.
//!
//! One `reqwest` client keeps connections open (HTTP/2 where the server
//! offers it through ALPN), a semaphore caps requests in flight per
//! provider, and each reply streams as server-sent events so the loop can
//! time the first token separately from the rest.
//!
//! A 429 sets one cooldown for the whole provider. No agent sends until it
//! ends, so a burst of 429s costs every agent one shared wait instead of a
//! retry storm per agent:
//!
//! ```text
//! agent 3  send ─> 429 ─> cooldown until T (Retry-After, else full jitter)
//! agent 7  ............... wait for T ─> wait for a slot ─> send
//! agent 3  ............... wait for T ─> wait for a slot ─> send
//! ```
//!
//! A model call makes no desk calls, so under resd the agent's kiosk stays
//! frozen while the call waits, and parks once the wait passes park_after.

use crate::config::{Budget, Price, Provider};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::Instant;

pub struct Model {
    http: reqwest::Client,
    url: String,
    model: String,
    key: Option<String>,
    gate: Arc<Semaphore>,
    retries: u32,
    rate_limit_wait: Duration,
    backoff: Backoff,
    cooldown: std::sync::Mutex<Cooldown>,
    /// Calls waiting out the cooldown now, and the most at once so far.
    rate_waiting: AtomicUsize,
    rate_waiting_peak: AtomicUsize,
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
            Ok(s) => s
                .trim()
                .parse()
                .map_err(|e| format!("ledger {}: {e}", budget.ledger.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0.0,
            Err(e) => return Err(format!("ledger {}: {e}", budget.ledger.display())),
        };
        Ok(Ledger {
            budget: budget.clone(),
            spent: std::sync::Mutex::new(spent),
        })
    }

    fn spent(&self) -> f64 {
        self.spent.lock().map(|s| *s).unwrap_or(f64::INFINITY)
    }

    fn add(&self, usd: f64) -> f64 {
        let Ok(mut s) = self.spent.lock() else {
            return f64::INFINITY;
        };
        *s += usd;
        let _ = std::fs::write(&self.budget.ledger, format!("{:.6}\n", *s));
        *s
    }
}

/// The provider's rate-limit cooldown, shared by every agent: no request
/// goes out before `until`.
#[derive(Debug)]
struct Cooldown {
    until: Instant,
    /// Rounds of 429s since the provider last served a request; sets the
    /// backoff.
    level: u32,
}

impl Cooldown {
    /// A 429 for a request sent at `sent`; `u` is uniform in `[0, 1)`. Only
    /// a request sent after the cooldown in force had ended starts a round,
    /// so 429s for requests already in flight raise the level once.
    fn limited(
        &mut self,
        sent: Instant,
        now: Instant,
        after: Option<Duration>,
        backoff: Backoff,
        u: f64,
    ) {
        let fresh = sent >= self.until;
        if fresh {
            self.level += 1;
        }
        let wait = match after {
            Some(a) => a,
            None if fresh => backoff.wait(self.level, u),
            None => return,
        };
        self.until = self.until.max(now + wait);
    }

    /// A served request sent at `sent`.
    fn served(&mut self, sent: Instant) {
        if sent >= self.until {
            self.level = 0;
        }
    }
}

/// Exponential backoff with full jitter: wait `n` (from 1) is uniform in
/// `[0, min(max, first * 2^(n-1))]`.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    first: Duration,
    max: Duration,
}

impl Backoff {
    /// Wait `n`, given `u` uniform in `[0, 1)`.
    fn wait(self, n: u32, u: f64) -> Duration {
        let ceiling = self
            .first
            .saturating_mul(1 << n.saturating_sub(1).min(30))
            .min(self.max);
        ceiling.mul_f64(u)
    }
}

/// Uniform in `[0, 1)`. Every `RandomState` gets fresh keys from a random
/// per-process seed, which is plenty to spread retries apart.
fn uniform() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let r = std::hash::RandomState::new().build_hasher().finish();
    (r >> 11) as f64 / (1u64 << 53) as f64
}

/// A model call that failed, and how long it had waited out rate limits.
#[derive(Debug)]
pub struct Failed {
    pub error: String,
    pub rate_wait_ms: f64,
}

/// One completed model call.
#[derive(Debug, Default)]
pub struct Completion {
    pub text: String,
    /// Waiting for a free slot under the provider's concurrency cap.
    pub queue_ms: f64,
    /// Waiting out the provider's rate-limit cooldown.
    pub rate_wait_ms: f64,
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
        let http = http.build().map_err(|e| format!("http client: {e}"))?;
        Ok(Model {
            http,
            url: format!("{}/chat/completions", p.base_url.trim_end_matches('/')),
            model: p.model.clone(),
            key,
            gate: Arc::new(Semaphore::new(p.max_concurrency.max(1))),
            retries: p.retries,
            rate_limit_wait: Duration::from_secs(p.rate_limit_wait_s),
            backoff: Backoff {
                first: Duration::from_millis(p.backoff_ms),
                max: Duration::from_millis(p.backoff_max_ms),
            },
            cooldown: std::sync::Mutex::new(Cooldown {
                until: Instant::now(),
                level: 0,
            }),
            rate_waiting: AtomicUsize::new(0),
            rate_waiting_peak: AtomicUsize::new(0),
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

    /// One completion. A 429 retries until `rate_limit_wait_s` after a rate
    /// limit first held this call; a 5xx, a transport error, or an empty
    /// reply retries up to `retries` attempts in all.
    pub async fn complete(&self, messages: &[Value], schema: &Value) -> Result<Completion, Failed> {
        let mut rate_wait = Duration::ZERO;
        self.call(messages, schema, &mut rate_wait)
            .await
            .map_err(|error| Failed {
                error,
                rate_wait_ms: ms(rate_wait),
            })
    }

    async fn call(
        &self,
        messages: &[Value],
        schema: &Value,
        rate_wait: &mut Duration,
    ) -> Result<Completion, String> {
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
        let mut queue = Duration::ZERO;
        // When a rate limit first held this call, by a 429 or a cooldown.
        let mut limited: Option<Instant> = None;
        let mut last = String::new();
        let (mut attempt, mut failures) = (0, 0);
        loop {
            attempt += 1;
            // Wait out the cooldown, then for a slot. A cooldown that began
            // while this call queued holds it too.
            let permit = loop {
                let now = Instant::now();
                let until = self.cooldown().until;
                if until > now {
                    limited.get_or_insert(now);
                }
                if let Some(since) = limited
                    && until.max(now) > since + self.rate_limit_wait
                {
                    return Err(format!(
                        "rate limited past rate_limit_wait_s ({} s); last reply: {last}",
                        self.rate_limit_wait.as_secs()
                    ));
                }
                if until > now {
                    let n = self.rate_waiting.fetch_add(1, Ordering::Relaxed) + 1;
                    self.rate_waiting_peak.fetch_max(n, Ordering::Relaxed);
                    tokio::time::sleep_until(until).await;
                    self.rate_waiting.fetch_sub(1, Ordering::Relaxed);
                    *rate_wait += now.elapsed();
                    continue;
                }
                let queued = Instant::now();
                let permit = self.gate.acquire().await.map_err(|e| e.to_string())?;
                queue += queued.elapsed();
                if self.cooldown().until <= Instant::now() {
                    break permit;
                }
            };
            if let Some(l) = &self.ledger
                && l.spent() >= l.budget.cap_usd
            {
                return Err(format!(
                    "budget: ${:.4} spent, cap ${}",
                    l.spent(),
                    l.budget.cap_usd
                ));
            }
            let sent = Instant::now();
            let reply = self.once(&body).await;
            drop(permit);
            match reply {
                Ok(mut c) => {
                    self.cooldown().served(sent);
                    c.queue_ms = ms(queue);
                    c.rate_wait_ms = ms(*rate_wait);
                    c.attempts = attempt;
                    return Ok(c);
                }
                Err((Failure::RateLimited(after), e)) => {
                    let now = Instant::now();
                    self.cooldown()
                        .limited(sent, now, after, self.backoff, uniform());
                    limited.get_or_insert(now);
                    last = e;
                }
                Err((Failure::Transient, e)) => {
                    failures += 1;
                    if failures >= self.retries.max(1) {
                        return Err(e);
                    }
                    last = e;
                    tokio::time::sleep(self.backoff.wait(failures, uniform())).await;
                }
                Err((Failure::Fatal, e)) => return Err(e),
            }
        }
    }

    fn cooldown(&self) -> std::sync::MutexGuard<'_, Cooldown> {
        // Nothing panics while holding it, so a poisoned lock is still whole.
        self.cooldown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The most calls that waited out a rate-limit cooldown at once.
    pub fn rate_waiting_peak(&self) -> usize {
        self.rate_waiting_peak.load(Ordering::Relaxed)
    }

    /// Keep `path` holding the client's live state for `panoptes top`:
    /// `{"rate_waiting": N}`, the calls waiting out a rate-limit cooldown
    /// now. Rewritten when it changes, at most every 250 ms, until the
    /// process exits.
    pub async fn publish(self: Arc<Self>, path: PathBuf) {
        let tmp = path.with_extension("tmp");
        let mut shown = None;
        loop {
            let n = self.rate_waiting.load(Ordering::Relaxed);
            // Renamed into place, so a reader never sees half a file.
            if shown != Some(n)
                && std::fs::write(&tmp, json!({ "rate_waiting": n }).to_string())
                    .and_then(|()| std::fs::rename(&tmp, &path))
                    .is_ok()
            {
                shown = Some(n);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    fn charge(&self, c: &mut Completion) {
        let Some(price) = &self.price else { return };
        let (Some(p), Some(o)) = (c.prompt_tokens, c.completion_tokens) else {
            return;
        };
        let usd = price.cost(p, c.cached_tokens.unwrap_or(0), o);
        c.cost_usd = Some(usd);
        c.spent_usd = self.ledger.as_ref().map(|l| l.add(usd));
    }

    /// One request. The error says which retry, if any, could help.
    async fn once(&self, body: &Value) -> Result<Completion, (Failure, String)> {
        let mut req = self.http.post(&self.url).json(body);
        if let Some(k) = &self.key {
            req = if self.api_key_auth {
                req.header(reqwest::header::AUTHORIZATION, format!("Api-Key {k}"))
            } else {
                req.bearer_auth(k)
            };
        }
        let sent = Instant::now();
        let resp = req
            .send()
            .await
            .map_err(|e| (Failure::Transient, format!("request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let failure = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // Seconds; an HTTP date is not parsed. A day bounds it.
                let after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|s| s.is_finite())
                    .map(|s| Duration::from_secs_f64(s.clamp(0.0, 86_400.0)));
                Failure::RateLimited(after)
            } else if status.is_server_error() {
                Failure::Transient
            } else {
                Failure::Fatal
            };
            let text = resp.text().await.unwrap_or_default();
            return Err((
                failure,
                format!(
                    "HTTP {status}: {}",
                    text.chars().take(400).collect::<String>()
                ),
            ));
        }
        let mut c = Completion::default();
        let mut first: Option<Instant> = None;
        let mut buf = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| (Failure::Transient, format!("stream: {e}")))?;
            buf.extend_from_slice(&chunk);
            while let Some(end) = find(&buf, b"\n\n") {
                let event: Vec<u8> = buf.drain(..end + 2).collect();
                for line in String::from_utf8_lossy(&event).lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    // Reasoning streams before the content; the first token
                    // of either starts decode.
                    let delta = &v["choices"][0]["delta"];
                    if ["reasoning_content", "reasoning"]
                        .iter()
                        .any(|k| delta[k].as_str().is_some_and(|t| !t.is_empty()))
                    {
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
                        c.reasoning_tokens =
                            u["completion_tokens_details"]["reasoning_tokens"].as_u64();
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
            return Err((Failure::Transient, "empty completion".into()));
        }
        Ok(c)
    }
}

/// Why a request failed, which picks its retry.
#[derive(Debug)]
enum Failure {
    /// 429: wait out the provider's cooldown, with the server's
    /// `Retry-After` when it sent one.
    RateLimited(Option<Duration>),
    /// 5xx, transport, or an empty reply: retry a few times.
    Transient,
    /// Anything else, such as a 400: a retry would fail the same way.
    Fatal,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Phases, Run, action_schema};
    use crate::desk::Desk;
    use axum::http::{StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    fn msd(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_burst_of_429s_is_one_round() {
        let b = Backoff {
            first: msd(100),
            max: msd(1000),
        };
        let t0 = Instant::now();
        let at = |n| t0 + msd(n);
        let mut c = Cooldown {
            until: t0,
            level: 0,
        };
        // Three requests sent before the first 429 came back: one round.
        c.limited(at(0), at(10), None, b, 0.5);
        c.limited(at(0), at(11), None, b, 0.5);
        c.limited(at(1), at(12), None, b, 0.5);
        assert_eq!((c.level, c.until), (1, at(60)));
        // A request sent once it ended and limited again starts round two,
        // with twice the ceiling.
        c.limited(at(60), at(70), None, b, 0.5);
        assert_eq!((c.level, c.until), (2, at(170)));
        // A Retry-After holds even when its request was already in flight.
        c.limited(at(65), at(80), Some(msd(300)), b, 0.0);
        assert_eq!((c.level, c.until), (2, at(380)));
        // Only a request sent after the cooldown, once served, resets it.
        c.served(at(100));
        assert_eq!(c.level, 2);
        c.served(at(380));
        assert_eq!(c.level, 0);
    }

    #[test]
    fn backoff_ceiling_doubles_to_the_max() {
        let b = Backoff {
            first: msd(500),
            max: msd(60_000),
        };
        let ceilings: Vec<u128> = (1..=9).map(|n| b.wait(n, 1.0).as_millis()).collect();
        assert_eq!(
            ceilings,
            [500, 1000, 2000, 4000, 8000, 16_000, 32_000, 60_000, 60_000]
        );
        assert_eq!(b.wait(1000, 1.0), msd(60_000));
        assert_eq!(b.wait(3, 0.0), Duration::ZERO);
        let u: Vec<f64> = (0..1000).map(|_| uniform()).collect();
        assert!(u.iter().all(|u| (0.0..1.0).contains(u)));
        assert!(u.windows(2).all(|w| w[0] != w[1]));
    }

    /// What the mock model answers.
    #[derive(Debug, Clone, Copy)]
    enum Policy {
        /// Serve `n` requests per fixed `window`; 429 the rest, with no
        /// Retry-After.
        Window { n: usize, window: Duration },
        /// 429 the first request with this Retry-After; serve the rest.
        FirstLimited(&'static str),
        /// Answer every request with this status.
        Always(u16),
    }

    #[derive(Debug)]
    struct Mock {
        policy: Policy,
        started: Instant,
        /// Every request's arrival and status, in order.
        seen: Mutex<Vec<(Instant, u16)>>,
    }

    impl Mock {
        fn count(&self, status: u16) -> usize {
            let seen = self.seen.lock().unwrap();
            seen.iter().filter(|s| s.1 == status).count()
        }
    }

    /// A mock OpenAI-compatible server, and its base URL.
    async fn serve(policy: Policy) -> (Arc<Mock>, String) {
        let mock = Arc::new(Mock {
            policy,
            started: Instant::now(),
            seen: Mutex::default(),
        });
        let app = axum::Router::new()
            .route("/v1/chat/completions", axum::routing::post(completions))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (mock, url)
    }

    async fn completions(
        axum::extract::State(m): axum::extract::State<Arc<Mock>>,
        body: String,
    ) -> Response {
        let now = Instant::now();
        let status = {
            let mut seen = m.seen.lock().unwrap();
            let status = match m.policy {
                Policy::Window { n, window } => {
                    let w = |t: Instant| (t - m.started).as_nanos() / window.as_nanos();
                    let used = seen.iter().filter(|s| w(s.0) == w(now)).count();
                    if used < n { 200 } else { 429 }
                }
                Policy::FirstLimited(_) if seen.is_empty() => 429,
                Policy::FirstLimited(_) => 200,
                Policy::Always(s) => s,
            };
            seen.push((now, status));
            status
        };
        match (status, m.policy) {
            (200, _) => {}
            (429, Policy::FirstLimited(after)) => {
                let h = [(header::RETRY_AFTER, after)];
                return (StatusCode::TOO_MANY_REQUESTS, h, "slow down").into_response();
            }
            (s, _) => return (StatusCode::from_u16(s).unwrap(), "mock").into_response(),
        }
        // The prompt is system, task, then two messages per step taken:
        // act twice, then say done on the third step.
        let req: Value = serde_json::from_str(&body).unwrap();
        let reply = if req["messages"].as_array().map_or(0, Vec::len) >= 7 {
            r#"{"actions":[],"done":true,"note":"done"}"#
        } else {
            r#"{"actions":[{"tool":"wait_settled","args":{}}],"done":false,"note":""}"#
        };
        let chunk = json!({ "choices": [{ "delta": { "content": reply } }] });
        let sse = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        ([(header::CONTENT_TYPE, "text/event-stream")], sse).into_response()
    }

    /// Serve a desk's line protocol in memory: every call succeeds, and
    /// every tree is one quiet page.
    async fn fake_desk(stream: UnixStream) {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let id = serde_json::from_str::<Value>(&l).map_or(Value::Null, |v| v["id"].clone());
            let reply = json!({ "id": id, "ok": true,
                "result": { "tree": "[1] RootWebArea \"Desk\"\n" },
                "timing": { "total_ms": 0.0 } });
            let line = format!("{reply}\n");
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    }

    /// A client for `url`: 16 in flight, short backoff, then `over`.
    fn client(url: &str, over: Value) -> Model {
        let mut p = json!({ "base_url": url, "model": "mock", "max_concurrency": 16,
                            "backoff_ms": 10, "backoff_max_ms": 100 });
        for (k, v) in over.as_object().unwrap() {
            p[k] = v.clone();
        }
        Model::new(&serde_json::from_value(p).unwrap(), None).unwrap()
    }

    fn hi() -> Vec<Value> {
        vec![json!({ "role": "user", "content": "hi" })]
    }

    /// 100 step loops share one client against a mock that serves 30
    /// requests per 100 ms and 429s the rest. Each loop drives an in-memory
    /// desk, not Chromium. Every agent must finish its three steps.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_hundred_agents_finish_under_a_rate_limit() {
        let (mock, url) = serve(Policy::Window {
            n: 30,
            window: msd(100),
        })
        .await;
        let model = Arc::new(client(&url, json!({ "rate_limit_wait_s": 30 })));
        let phases = Arc::new(Phases::new(&BTreeMap::new(), "http://erp.invalid"));
        let acting = Arc::new(Semaphore::new(100));
        let schema = Arc::new(action_schema());
        let t = Instant::now();
        let mut runs = Vec::new();
        for i in 0..100 {
            let (desk, server) = UnixStream::pair().unwrap();
            tokio::spawn(fake_desk(server));
            let (model, phases) = (model.clone(), phases.clone());
            let (acting, schema) = (acting.clone(), schema.clone());
            runs.push(tokio::spawn(async move {
                let mut desk = Desk::connected(desk);
                Run {
                    desk: &mut desk,
                    model: &model,
                    system: "system",
                    schema: &schema,
                    task: format!("task {i}"),
                    start_url: None,
                    step_cap: 10,
                    ax_max_bytes: 10_000,
                    history_full: 3,
                    phases: &phases,
                    acting: &acting,
                    cost_cap_usd: None,
                }
                .go(|_| {})
                .await
            }));
        }
        let mut outcomes = Vec::new();
        for r in runs {
            outcomes.push(r.await.unwrap());
        }
        let failed: Vec<_> = outcomes.iter().filter(|o| !o.done).collect();
        assert!(failed.is_empty(), "{} failed: {failed:?}", failed.len());
        assert!(outcomes.iter().all(|o| o.steps == 3));
        let waited = outcomes.iter().filter(|o| o.rate_wait_ms > 0.0).count();
        let peak = model.rate_waiting_peak();
        eprintln!(
            "100 agents in {:?}: {} served, {} 429s, {waited} agents waited, peak {peak} waiting",
            t.elapsed(),
            mock.count(200),
            mock.count(429)
        );
        assert_eq!(mock.count(200), 300);
        assert!(mock.count(429) > 0);
        // More calls waited at once than could be in flight: calls that
        // never got a 429 held for another's.
        assert!(peak > 16, "peak {peak}");
    }

    /// One 429 with Retry-After holds every call on the provider, including
    /// one that never got a 429 itself.
    #[tokio::test]
    async fn a_429_holds_every_call_for_its_retry_after() {
        let (mock, url) = serve(Policy::FirstLimited("0.3")).await;
        let model = Arc::new(client(&url, json!({})));
        let first = tokio::spawn({
            let model = model.clone();
            async move { model.complete(&hi(), &json!({})).await }
        });
        // Start the second call once the first one's 429 set the cooldown.
        let t = Instant::now();
        while model.cooldown().until <= Instant::now() {
            assert!(t.elapsed() < Duration::from_secs(5), "no cooldown");
            tokio::time::sleep(msd(1)).await;
        }
        let second = model.complete(&hi(), &json!({})).await.unwrap();
        let first = first.await.unwrap().unwrap();
        let seen = mock.seen.lock().unwrap().clone();
        let statuses: Vec<u16> = seen.iter().map(|s| s.1).collect();
        assert_eq!(statuses, [429, 200, 200]);
        assert!(seen[1..].iter().all(|s| s.0 >= seen[0].0 + msd(300)));
        assert_eq!((first.attempts, second.attempts), (2, 1));
        assert!(second.rate_wait_ms > 200.0, "{}", second.rate_wait_ms);
    }

    /// 429s retry until rate_limit_wait_s, not a fixed count, and the call
    /// fails once its next attempt could not go out in time.
    #[tokio::test]
    async fn gives_up_on_429s_at_the_rate_limit_wait() {
        let (mock, url) = serve(Policy::Always(429)).await;
        let model = client(
            &url,
            json!({ "rate_limit_wait_s": 1, "backoff_ms": 50, "backoff_max_ms": 200 }),
        );
        let t = Instant::now();
        let e = model.complete(&hi(), &json!({})).await.unwrap_err();
        let took = t.elapsed();
        assert!(e.error.contains("HTTP 429"), "{}", e.error);
        // The give-up still reports the time spent waiting out 429s.
        assert!(e.rate_wait_ms > 0.0, "{}", e.rate_wait_ms);
        // Waits of at most 50, 100, then 200 ms fit at least six attempts
        // in a second.
        assert!(mock.count(429) >= 6, "{} attempts", mock.count(429));
        assert!(took >= msd(800) && took < msd(1300), "{took:?}");
    }

    /// A 5xx retries `retries` attempts in all; a 400 does not retry.
    #[tokio::test]
    async fn retries_a_5xx_a_fixed_number_of_times() {
        let (mock, url) = serve(Policy::Always(503)).await;
        let e = client(&url, json!({ "retries": 3 }))
            .complete(&hi(), &json!({}))
            .await
            .unwrap_err();
        assert!(e.error.contains("HTTP 503"), "{}", e.error);
        assert_eq!(mock.count(503), 3);
        let (mock, url) = serve(Policy::Always(400)).await;
        let e = client(&url, json!({ "retries": 3 }))
            .complete(&hi(), &json!({}))
            .await
            .unwrap_err();
        assert!(e.error.contains("HTTP 400"), "{}", e.error);
        assert_eq!(mock.count(400), 1);
    }

    /// `panoptes top` reads model.json for its rate-limited count.
    #[tokio::test]
    async fn publishes_the_rate_waiting_count() {
        let dir = std::env::temp_dir().join(format!("panoptes-loop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.json");
        let model = Arc::new(client("http://127.0.0.1:9/v1", json!({})));
        model.rate_waiting.store(5, Ordering::Relaxed);
        tokio::spawn(model.clone().publish(path.clone()));
        let t = Instant::now();
        let text = loop {
            if let Ok(s) = std::fs::read_to_string(&path) {
                break s;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "no model.json");
            tokio::time::sleep(msd(10)).await;
        };
        std::fs::remove_dir_all(&dir).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v, json!({ "rate_waiting": 5 }));
    }
}
