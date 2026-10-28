//! panoptes-loop: many agents in one process, each driving its own
//! panoptes-desk, all sharing one model client.
//!
//! ```text
//! panoptes-loop CONFIG [--tasks FILE | --invoices inv-0001,...] [--first N]
//! ```
//!
//! Each agent takes one job, starts its desk, and runs the step loop in
//! `agent`. The jobs are the lines of a `--tasks` file
//! (`config::TaskLine`), or else invoices from the demo's ground truth. The
//! run writes, under `out_dir/run_name/`:
//!
//! ```text
//! trace.jsonl     one line per step: queue, ttft, decode, action, settle, observe
//! results.jsonl   one line per agent as it finishes: done (claimed), verified
//! assigned.txt    one line per agent: invoice id (for verify.sh) or task line
//! summary.json    medians and means over the steps, claimed done vs verified
//! model.json      live: model calls waiting out a rate-limit cooldown
//! ```
//!
//! An agent's `done` is only its claim. After an invoice agent's run the
//! loop checks the invoice over ERPNext's REST API (`erp`); after a task
//! agent claims done the loop runs the line's `check` (`check`). `verified`
//! is that check. A task agent's result line:
//!
//! ```json
//! {"agent": 0, "line": 1, "status": "verified", "done": true, "steps": 6,
//!  "cost_usd": 0.0041, "url": "https://books.toscrape.com/...", "note": "...",
//!  "error": null, "verified": true, "check_exit": 0, "check_error": null,
//!  "reloads": 0, "retypes": 0, "phase_wait_ms": 0.0, "acting_wait_ms": 3.1,
//!  "desk_start_ms": 1183, "wall_ms": 21877}
//! ```
//!
//! `status` is `verified` or `check_failed` when a check ran after the done
//! claim, `claimed_done` when the line has no check, and `error` when the
//! run ended without the claim, for the reason in `error`.

mod agent;
mod check;
mod config;
mod desk;
mod erp;
mod model;

use config::Config;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One agent's job: the task message its prompt carries, and where it came
/// from.
struct Task {
    text: String,
    kind: Kind,
}

enum Kind {
    /// An invoice from the demo's ground truth, checked over ERPNext's API
    /// after the run.
    Invoice { file: String, truth: Value },
    /// A line of a task file: the agent starts at `url`, and `check`
    /// verifies its done claim.
    Line {
        line: usize,
        url: String,
        check: Option<String>,
    },
}

impl Kind {
    /// The field that ties the agent's trace and result lines to its job.
    fn tag(&self) -> (&'static str, Value) {
        match self {
            Kind::Invoice { file, .. } => ("invoice", json!(file)),
            Kind::Line { line, .. } => ("line", json!(line)),
        }
    }
}

/// A task agent's result in one word, from its result line. `verified` is
/// there only when a check ran.
fn status(r: &Value) -> &'static str {
    match (r["done"] == true, r["verified"].as_bool()) {
        (false, _) => "error",
        (true, None) => "claimed_done",
        (true, Some(true)) => "verified",
        (true, Some(false)) => "check_failed",
    }
}

fn load_task_file(path: &Path) -> Result<Vec<Task>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let lines = config::parse_tasks(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(lines
        .into_iter()
        .map(|(line, t)| Task {
            text: format!("{}\n\nStart page: {}", t.prompt, t.url),
            kind: Kind::Line {
                line,
                url: t.url,
                check: t.check,
            },
        })
        .collect())
}

fn load_invoices(tasks: &config::Tasks, pick: &[String]) -> Result<Vec<Task>, String> {
    let creds = std::fs::read_to_string(&tasks.creds)
        .map_err(|e| format!("{}: {e}", tasks.creds.display()))?;
    // slug  url  user_field  user  password
    let creds: HashMap<&str, Vec<&str>> = creds
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 5).then(|| (f[0], f))
        })
        .collect();
    let truth = std::fs::read_to_string(&tasks.truth)
        .map_err(|e| format!("{}: {e}", tasks.truth.display()))?;
    let mut by_file = HashMap::new();
    let mut order = Vec::new();
    for l in truth.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(l).map_err(|e| format!("truth: {e}"))?;
        let file = v["file"].as_str().unwrap_or("").to_owned();
        order.push(file.clone());
        by_file.insert(file, v);
    }
    let files: Vec<String> = if pick.is_empty() {
        tasks.invoices.clone()
    } else {
        pick.to_vec()
    };
    let files = if files.is_empty() { order } else { files };
    files
        .into_iter()
        .map(|file| {
            let v = by_file.get(&file).ok_or_else(|| format!("{file} is not in the truth file"))?;
            let slug = v["slug"].as_str().unwrap_or("");
            let c = creds.get(slug).ok_or_else(|| format!("no portal credentials for {slug}"))?;
            // Only what a clerk is handed: the number on the portal's list,
            // the portal, and its credentials. The rest is on the PDF.
            let text = format!(
                "Invoice number: {no}\nSupplier portal: {login}\nPortal user: {user}\nPortal password: {pass}",
                no = v["invoice_no"].as_str().unwrap_or(""),
                login = c[1],
                user = c[3],
                pass = c[4],
            );
            Ok(Task {
                text,
                kind: Kind::Invoice {
                    file,
                    truth: v.clone(),
                },
            })
        })
        .collect()
}

struct Args {
    config: String,
    /// The task file; without one the run takes invoices.
    tasks: Option<PathBuf>,
    invoices: Vec<String>,
    first: Option<usize>,
}

fn args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let config = it
        .next()
        .ok_or("usage: panoptes-loop CONFIG [--tasks FILE | --invoices a,b] [--first N]")?;
    let mut tasks = None;
    let mut pick = Vec::new();
    let mut first = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tasks" => tasks = Some(PathBuf::from(it.next().ok_or("--tasks needs a file")?)),
            "--invoices" => {
                pick = it
                    .next()
                    .ok_or("--invoices needs a list")?
                    .split(',')
                    .map(str::to_owned)
                    .collect();
            }
            "--first" => {
                first = Some(
                    it.next()
                        .ok_or("--first needs N")?
                        .parse()
                        .map_err(|_| "--first N")?,
                )
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if tasks.is_some() && !pick.is_empty() {
        return Err("--tasks and --invoices do not mix".into());
    }
    Ok(Args {
        config,
        tasks,
        invoices: pick,
        first,
    })
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn summarize(trace: &[Value]) -> Value {
    let steps: Vec<&Value> = trace
        .iter()
        .filter(|t| t["step"].as_u64().unwrap_or(0) > 0)
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("steps".into(), json!(steps.len()));
    for k in [
        "phase_wait_ms",
        "acting_wait_ms",
        "rate_wait_ms",
        "model_ms",
        "harness_ms",
        "queue_ms",
        "ttft_ms",
        "decode_ms",
        "action_ms",
        "desk_ms",
        "settle_ms",
        "observe_ms",
        "step_ms",
    ] {
        let mut v: Vec<f64> = steps.iter().filter_map(|t| t[k].as_f64()).collect();
        let mean = if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        };
        let p50 = median(&mut v);
        let p95 = v
            .get((v.len() * 95 / 100).min(v.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0.0);
        out.insert(
            k.into(),
            json!({ "mean": (mean * 10.0).round() / 10.0, "p50": p50, "p95": p95 }),
        );
    }
    Value::Object(out)
}

#[tokio::main]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("panoptes-loop: {e}");
        std::process::exit(1);
    }
}

async fn real_main() -> Result<(), String> {
    let args = args()?;
    let cfg_path = &args.config;
    let cfg: Config = serde_json::from_str(
        &std::fs::read_to_string(cfg_path).map_err(|e| format!("{cfg_path}: {e}"))?,
    )
    .map_err(|e| format!("{cfg_path}: {e}"))?;
    // Where the jobs come from decides the prompt, the phase caps, and how
    // a done claim is verified.
    let (mut tasks, system, phases, erp) = match &args.tasks {
        // The caps exist for ERPNext's few workers; with none, no agent is
        // ever in a capped phase.
        Some(file) => (
            load_task_file(file)?,
            agent::general_prompt(),
            agent::Phases::new(&BTreeMap::new(), ""),
            None,
        ),
        None => {
            let t = cfg
                .tasks
                .as_ref()
                .ok_or_else(|| format!("{cfg_path}: no tasks, and no --tasks FILE"))?;
            let tasks = load_invoices(t, &args.invoices)?;
            let erp = match erp::Erp::login(&t.erp_url, &t.erp_user, &t.erp_password).await {
                Ok(e) => Some(Arc::new(e)),
                Err(e) => {
                    eprintln!("panoptes-loop: no verification: {e}");
                    None
                }
            };
            (
                tasks,
                agent::invoice_prompt(&t.erp_url, &t.erp_user, &t.erp_password),
                agent::Phases::new(&cfg.phase_caps, &t.erp_url),
                erp,
            )
        }
    };
    if let Some(n) = args.first {
        tasks.truncate(n);
    }
    let provider = cfg
        .providers
        .get(&cfg.provider)
        .ok_or_else(|| format!("no provider {}", cfg.provider))?;
    let model = Arc::new(model::Model::new(provider, cfg.budget.as_ref())?);
    let out = cfg.out_dir.join(&cfg.run_name);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let assigned: String = tasks
        .iter()
        .map(|t| match &t.kind {
            Kind::Invoice { file, .. } => format!("{file}\n"),
            Kind::Line { line, .. } => format!("{line}\n"),
        })
        .collect();
    std::fs::write(out.join("assigned.txt"), assigned).map_err(|e| e.to_string())?;
    tokio::spawn(model.clone().publish(out.join("model.json")));
    let trace_file = Arc::new(Mutex::new(
        std::fs::File::create(out.join("trace.jsonl")).map_err(|e| e.to_string())?,
    ));
    let all_trace = Arc::new(Mutex::new(Vec::<Value>::new()));
    let system = Arc::new(system);
    let schema = Arc::new(agent::action_schema());
    let phases = Arc::new(phases);
    let acting = Arc::new(tokio::sync::Semaphore::new(cfg.acting_cap.max(1)));
    let results_file = Arc::new(Mutex::new(
        std::fs::File::create(out.join("results.jsonl")).map_err(|e| e.to_string())?,
    ));
    let period = std::time::Duration::from_secs_f64(1.0 / cfg.start_per_s.max(0.01));
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let starts = Arc::new(tokio::sync::Mutex::new(ticks));
    let cfg = Arc::new(cfg);
    eprintln!(
        "panoptes-loop: {} agents, provider {}, acting cap {}, {} starts/s",
        tasks.len(),
        cfg.provider,
        cfg.acting_cap,
        cfg.start_per_s
    );
    let started = Instant::now();
    let mut handles = Vec::new();
    for (i, task) in tasks.into_iter().enumerate() {
        let (model, system, schema, cfg) =
            (model.clone(), system.clone(), schema.clone(), cfg.clone());
        let (trace_file, all_trace) = (trace_file.clone(), all_trace.clone());
        let (phases, erp) = (phases.clone(), erp.clone());
        let (acting, starts, results_file) = (acting.clone(), starts.clone(), results_file.clone());
        let run_dir = Path::new(&cfg.out_dir)
            .join(&cfg.run_name)
            .join(format!("agent-{i}"));
        handles.push(tokio::spawn(async move {
            starts.lock().await.tick().await;
            let (key, val) = task.kind.tag();
            let from_file = matches!(task.kind, Kind::Line { .. });
            let record = |mut r: Value| {
                r[key] = val.clone();
                if from_file {
                    r["status"] = json!(status(&r));
                }
                if let Ok(mut f) = results_file.lock() {
                    let _ = writeln!(f, "{r}");
                }
                r
            };
            if cfg.stop_file.as_ref().is_some_and(|p| p.exists()) {
                return record(
                    json!({ "agent": i, "done": false, "error": "not started: stop file" }),
                );
            }
            let t0 = Instant::now();
            // A starting kiosk is thawed, so a start takes an acting slot.
            let permit = acting.acquire().await;
            let desk = desk::Desk::start(&cfg.desk, i, &run_dir).await;
            drop(permit);
            let mut desk = match desk {
                Ok(d) => d,
                Err(e) => {
                    return record(
                        json!({ "agent": i, "done": false, "error": format!("desk: {e}") }),
                    );
                }
            };
            let start_ms = t0.elapsed().as_millis();
            let start_url = match &task.kind {
                Kind::Line { url, .. } => Some(url.as_str()),
                Kind::Invoice { .. } => None,
            };
            let outcome = agent::Run {
                desk: &mut desk,
                model: &model,
                system: &system,
                schema: &schema,
                task: task.text,
                start_url,
                step_cap: cfg.step_cap,
                ax_max_bytes: cfg.ax_max_bytes,
                history_full: cfg.history_full,
                phases: &phases,
                acting: &acting,
                cost_cap_usd: cfg.invoice_cost_cap_usd,
            }
            .go(|st| {
                let mut line = st.to_json(i);
                line[key] = val.clone();
                if let Ok(mut f) = trace_file.lock() {
                    let _ = writeln!(f, "{line}");
                }
                if let Ok(mut a) = all_trace.lock() {
                    a.push(line);
                }
            })
            .await;
            desk.stop().await;
            let mut r = json!({ "agent": i, "done": outcome.done, "steps": outcome.steps,
                    "error": outcome.error, "note": outcome.note, "url": outcome.url,
                    "reloads": outcome.reloads,
                    "retypes": outcome.retypes, "phase_wait_ms": outcome.phase_wait_ms,
                    "acting_wait_ms": outcome.acting_wait_ms, "rate_wait_ms": outcome.rate_wait_ms,
                    "cost_usd": (outcome.cost_usd * 1e6).round() / 1e6,
                    "desk_start_ms": start_ms, "wall_ms": t0.elapsed().as_millis() });
            let verdict = match &task.kind {
                Kind::Invoice { truth, .. } => match &erp {
                    Some(erp) => erp.check(truth).await,
                    None => Value::Null,
                },
                Kind::Line { check: Some(c), .. } if outcome.done => {
                    check::run(c, &run_dir, &outcome.url, &outcome.note, check::TIMEOUT).await
                }
                Kind::Line { .. } => Value::Null,
            };
            for (k, v) in verdict.as_object().into_iter().flatten() {
                r[k] = v.clone();
            }
            record(r)
        }));
    }
    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.map_err(|e| e.to_string())?);
    }
    let wall = started.elapsed().as_secs_f64();
    let trace = all_trace.lock().map(|t| t.clone()).unwrap_or_default();
    let count = |f: &dyn Fn(&Value) -> bool| results.iter().filter(|r| f(r)).count();
    let steps: Vec<u64> = results.iter().filter_map(|r| r["steps"].as_u64()).collect();
    let sum = |k: &str| results.iter().filter_map(|r| r[k].as_u64()).sum::<u64>();
    let mut summary = summarize(&trace);
    summary["agents"] = json!(results.len());
    summary["claimed_done"] = json!(count(&|r| r["done"] == true));
    summary["verified"] = json!(count(&|r| r["verified"] == true));
    summary["claimed_not_verified"] = json!(count(&|r| r["done"] == true && r["verified"] != true));
    if args.tasks.is_some() {
        let mut by_status: BTreeMap<&str, usize> = BTreeMap::new();
        for r in &results {
            *by_status
                .entry(r["status"].as_str().unwrap_or("?"))
                .or_default() += 1;
        }
        summary["status"] = json!(by_status);
    } else {
        let mut failed: BTreeMap<String, usize> = BTreeMap::new();
        for f in results
            .iter()
            .filter_map(|r| r["failed"].as_array())
            .flatten()
            .filter_map(Value::as_str)
        {
            *failed.entry(f.to_owned()).or_default() += 1;
        }
        summary["attached"] = json!(count(&|r| r["attached"] == true));
        summary["verified_and_attached"] =
            json!(count(&|r| r["verified"] == true && r["attached"] == true));
        summary["failed_checks"] = json!(failed);
    }
    // Agents whose model calls waited out a rate-limit cooldown, and the
    // most calls waiting at once.
    let waited = |r: &Value| r["rate_wait_ms"].as_f64().is_some_and(|w| w > 0.0);
    summary["rate_limited_agents"] = json!(count(&waited));
    summary["rate_waiting_peak"] = json!(model.rate_waiting_peak());
    summary["reloads"] = json!(sum("reloads"));
    summary["retypes"] = json!(sum("retypes"));
    summary["wall_s"] = json!((wall * 10.0).round() / 10.0);
    summary["steps_per_agent"] = json!(steps);
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    Ok(())
}
