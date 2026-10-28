//! The run's configuration, one JSON file:
//!
//! ```json
//! { "provider": "oracle",
//!   "providers": { "oracle": { "base_url": "http://127.0.0.1:8099/v1", "model": "oracle" } },
//!   "tasks": { "truth": ".../truth.jsonl", "creds": ".../creds.tsv", "invoices": ["inv-0001"] },
//!   "desk": { "launch": ["sudo", "systemd-run", "--unit=pdesk-loop-{i}", "..."],
//!             "stop": ["sudo", "systemctl", "stop", "pdesk-loop-{i}"] },
//!   "out_dir": "/var/tmp/glarb-bench/panoptes-run/loop" }
//! ```
//!
//! A provider's key lives in `key_file`; the loop reads it once and never
//! logs it. With `--tasks FILE` the jobs come from that file instead
//! ([`TaskLine`]), and `tasks` may be left out.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub provider: String,
    pub providers: BTreeMap<String, Provider>,
    /// The invoice demo's jobs. A `--tasks` file replaces them.
    #[serde(default)]
    pub tasks: Option<Tasks>,
    pub desk: DeskLaunch,
    pub out_dir: PathBuf,
    #[serde(default = "default_run_name")]
    pub run_name: String,
    #[serde(default = "default_step_cap")]
    pub step_cap: usize,
    #[serde(default = "default_ax_max_bytes")]
    pub ax_max_bytes: usize,
    /// Observations kept whole in the prompt; older ones shrink to a line.
    #[serde(default = "default_history_full")]
    pub history_full: usize,
    /// Most agents at once in a phase, by phase prefix: `{"erp": 50}`.
    /// A task file runs without caps.
    #[serde(default = "default_phase_caps")]
    pub phase_caps: BTreeMap<String, usize>,
    /// Most agents with a thawed kiosk at once: starting, acting, or
    /// observing. The rest wait frozen or parked.
    #[serde(default = "default_acting_cap")]
    pub acting_cap: usize,
    /// Desks started per second, so launches don't storm the host.
    #[serde(default = "default_start_per_s")]
    pub start_per_s: f64,
    /// While this file exists, agents not yet started are skipped.
    #[serde(default)]
    pub stop_file: Option<PathBuf>,
    /// Spend across runs: refuse model calls once the ledger passes the cap.
    #[serde(default)]
    pub budget: Option<Budget>,
    /// End an agent's run once its own model calls cost more than this.
    #[serde(default)]
    pub invoice_cost_cap_usd: Option<f64>,
}

/// A ledger file holding the dollars spent so far, shared by every run
/// that names it.
#[derive(Debug, Clone, Deserialize)]
pub struct Budget {
    pub ledger: PathBuf,
    pub cap_usd: f64,
}

/// Dollars per million tokens.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cached: f64,
}

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub base_url: String,
    pub model: String,
    pub key_file: Option<PathBuf>,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    /// Attempts for a 5xx, a transport error, or an empty reply.
    #[serde(default = "default_retries")]
    pub retries: u32,
    /// How long a model call keeps retrying 429s, from the first time a
    /// rate limit holds it.
    #[serde(default = "default_rate_limit_wait")]
    pub rate_limit_wait_s: u64,
    /// Backoff ceilings: the first wait is uniform in `[0, backoff_ms]`, and
    /// the ceiling doubles per retry up to `backoff_max_ms`.
    #[serde(default = "default_backoff")]
    pub backoff_ms: u64,
    #[serde(default = "default_backoff_max")]
    pub backoff_max_ms: u64,
    /// Send the action schema as `response_format: json_schema`.
    #[serde(default)]
    pub json_schema: bool,
    /// Send `response_format: json_object`.
    #[serde(default)]
    pub json_mode: bool,
    /// `bearer` sends `Authorization: Bearer KEY`; `api-key` sends
    /// `Authorization: Api-Key KEY`, as Baseten documents.
    #[serde(default = "default_auth")]
    pub auth: String,
    #[serde(default)]
    pub price: Option<Price>,
    /// Speak HTTP/2 without TLS (h2c). Over https, HTTP/2 comes from
    /// ALPN whenever the server offers it.
    #[serde(default)]
    pub http2_prior_knowledge: bool,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Extra fields merged into every request body, such as
    /// `{"reasoning_effort": "low"}`.
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Tasks {
    pub truth: PathBuf,
    pub creds: PathBuf,
    #[serde(default)]
    pub invoices: Vec<String>,
    #[serde(default = "default_erp_url")]
    pub erp_url: String,
    #[serde(default = "default_erp_user")]
    pub erp_user: String,
    #[serde(default = "default_erp_password")]
    pub erp_password: String,
}

/// One line of a `--tasks` file, one agent's job:
///
/// ```json
/// {"prompt": "What does A Light in the Attic cost?",
///  "url": "https://books.toscrape.com/",
///  "check": "case \"$PANOPTES_NOTE\" in *51.77*) ;; *) exit 1 ;; esac"}
/// ```
///
/// `check` is run as `check::run` describes. An unknown field fails the
/// whole file, so a misspelled field never runs as if it were absent.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLine {
    pub prompt: String,
    /// The page the agent starts on.
    pub url: String,
    #[serde(default)]
    pub check: Option<String>,
}

/// Parse a task file: one JSON object per line, blank lines skipped. Each
/// task comes with its line number, counting from 1.
pub fn parse_tasks(text: &str) -> Result<Vec<(usize, TaskLine)>, String> {
    let mut out = Vec::new();
    for (k, l) in text.lines().enumerate() {
        let n = k + 1;
        if l.trim().is_empty() {
            continue;
        }
        let t: TaskLine = serde_json::from_str(l).map_err(|e| {
            // serde counts lines within this one line; say the file's.
            let msg = e.to_string();
            let msg = msg.rsplit_once(" at line ").map_or(msg.as_str(), |m| m.0);
            format!("line {n}, column {}: {msg}", e.column())
        })?;
        if t.prompt.trim().is_empty() {
            return Err(format!("line {n}: empty prompt"));
        }
        reqwest::Url::parse(&t.url).map_err(|e| format!("line {n}: url {:?}: {e}", t.url))?;
        if t.check.as_ref().is_some_and(|c| c.trim().is_empty()) {
            return Err(format!("line {n}: empty check"));
        }
        out.push((n, t));
    }
    if out.is_empty() {
        return Err("no tasks".into());
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct DeskLaunch {
    /// Command that starts agent `{i}`'s desk with run directory `{run}`.
    pub launch: Vec<String>,
    /// Command that stops it.
    pub stop: Vec<String>,
    #[serde(default = "default_socket")]
    pub socket: String,
    #[serde(default = "default_start_timeout")]
    pub start_timeout_s: u64,
}

fn default_run_name() -> String {
    "run".into()
}
fn default_step_cap() -> usize {
    60
}
fn default_ax_max_bytes() -> usize {
    80_000
}
fn default_history_full() -> usize {
    3
}
fn default_phase_caps() -> BTreeMap<String, usize> {
    BTreeMap::from([("erp".to_owned(), 50)])
}
fn default_acting_cap() -> usize {
    40
}
fn default_start_per_s() -> f64 {
    10.0
}
fn default_concurrency() -> usize {
    16
}
fn default_timeout() -> u64 {
    120
}
fn default_retries() -> u32 {
    3
}
fn default_rate_limit_wait() -> u64 {
    600
}
fn default_backoff() -> u64 {
    500
}
fn default_backoff_max() -> u64 {
    60_000
}
fn default_auth() -> String {
    "bearer".into()
}
fn default_erp_url() -> String {
    "http://127.0.0.1:8080".into()
}
fn default_erp_user() -> String {
    "Administrator".into()
}
fn default_erp_password() -> String {
    "admin".into()
}
fn default_socket() -> String {
    "{run}/desk.sock".into()
}
fn default_start_timeout() -> u64 {
    40
}

/// Replace `{i}` and `{run}` in each word of a command.
pub fn expand(words: &[String], i: usize, run: &str) -> Vec<String> {
    words
        .iter()
        .map(|w| w.replace("{i}", &i.to_string()).replace("{run}", run))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_placeholders_in_every_word() {
        let cmd = vec![
            "--unit=pdesk-{i}".to_owned(),
            "exec x --run-dir {run}".to_owned(),
        ];
        assert_eq!(
            expand(&cmd, 7, "/r/7"),
            vec!["--unit=pdesk-7", "exec x --run-dir /r/7"]
        );
    }

    #[test]
    fn parses_task_lines_and_rejects_what_it_cannot_run() {
        let ok = "{\"prompt\": \"p\", \"url\": \"https://a.example/\"}\n\n\
{\"prompt\": \"q\", \"url\": \"data:text/html,x\", \"check\": \"true\"}\n";
        let t = parse_tasks(ok).unwrap();
        assert_eq!(t.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(t[1].1.check.as_deref(), Some("true"));
        assert!(t[0].1.check.is_none());

        let err = |text: &str| parse_tasks(text).unwrap_err();
        let line =
            |rest: &str| format!("{{\"prompt\": \"p\", \"url\": \"https://a.example/\"{rest}}}");
        let unknown = ": unknown field `chek`, expected one of `prompt`, `url`, `check`";
        let e = err(&line(", \"chek\": \"true\""));
        assert!(
            e.starts_with("line 1, column ") && e.ends_with(unknown),
            "{e}"
        );
        let e = err(&format!("\n{}", line(", \"timeout\": 5")));
        assert!(
            e.starts_with("line 2, column ") && e.contains("`timeout`"),
            "{e}"
        );
        assert!(err("{\"prompt\": \"p\"}").ends_with(": missing field `url`"));
        assert!(
            err("{\"prompt\": \"p\", \"url\": \"books.toscrape.com\"}").contains("line 1: url")
        );
        assert!(
            err("{\"prompt\": \" \", \"url\": \"https://a.example/\"}").contains("empty prompt")
        );
        assert!(err(&line(", \"check\": \"\"")).contains("empty check"));
        assert_eq!(err("\n\n"), "no tasks");
    }
}
