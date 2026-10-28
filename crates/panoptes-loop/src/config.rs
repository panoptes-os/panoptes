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
//! logs it.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub provider: String,
    pub providers: BTreeMap<String, Provider>,
    pub tasks: Tasks,
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
    /// Most agents at once in a phase, by phase prefix: `{"erp": 25}`.
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
    #[serde(default = "default_retries")]
    pub retries: u32,
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
    BTreeMap::from([("erp".to_owned(), 25)])
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
}
