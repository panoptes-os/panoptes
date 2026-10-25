//! panoptes-loop: many agents in one process, each driving its own
//! panoptes-desk, all sharing one model client.
//!
//! ```text
//! panoptes-loop CONFIG [--invoices inv-0001,inv-0002 | --first N]
//! ```
//!
//! Each agent takes one invoice from the demo's ground truth, starts its
//! desk, and runs the step loop in `agent`. The run writes, under
//! `out_dir/run_name/`:
//!
//! ```text
//! trace.jsonl     one line per step: queue, ttft, decode, action, settle, observe
//! results.jsonl   one line per agent as it finishes: done (claimed), verified, attached
//! assigned.txt    the invoice ids, for verify.sh
//! summary.json    medians and means over the steps, claimed done vs verified
//! ```
//!
//! An agent's `done` is only its claim. After its run the loop checks the
//! invoice over ERPNext's REST API (`erp`), and `verified` is that check.

mod agent;
mod config;
mod desk;
mod erp;
mod model;

use config::Config;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct Task {
    file: String,
    text: String,
    truth: Value,
}

fn load_tasks(cfg: &Config, pick: &[String]) -> Result<Vec<Task>, String> {
    let creds = std::fs::read_to_string(&cfg.tasks.creds)
        .map_err(|e| format!("{}: {e}", cfg.tasks.creds.display()))?;
    // slug  url  user_field  user  password
    let creds: HashMap<&str, Vec<&str>> = creds
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 5).then(|| (f[0], f))
        })
        .collect();
    let truth = std::fs::read_to_string(&cfg.tasks.truth)
        .map_err(|e| format!("{}: {e}", cfg.tasks.truth.display()))?;
    let mut by_file = HashMap::new();
    let mut order = Vec::new();
    for l in truth.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(l).map_err(|e| format!("truth: {e}"))?;
        let file = v["file"].as_str().unwrap_or("").to_owned();
        order.push(file.clone());
        by_file.insert(file, v);
    }
    let files: Vec<String> = if pick.is_empty() { cfg.tasks.invoices.clone() } else { pick.to_vec() };
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
            Ok(Task { file, text, truth: v.clone() })
        })
        .collect()
}

fn args() -> Result<(String, Vec<String>, Option<usize>), String> {
    let mut it = std::env::args().skip(1);
    let cfg = it.next().ok_or("usage: panoptes-loop CONFIG [--invoices a,b | --first N]")?;
    let mut pick = Vec::new();
    let mut first = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--invoices" => {
                pick = it.next().ok_or("--invoices needs a list")?.split(',').map(str::to_owned).collect();
            }
            "--first" => first = Some(it.next().ok_or("--first needs N")?.parse().map_err(|_| "--first N")?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok((cfg, pick, first))
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn summarize(trace: &[Value]) -> Value {
    let steps: Vec<&Value> = trace.iter().filter(|t| t["step"].as_u64().unwrap_or(0) > 0).collect();
    let mut out = serde_json::Map::new();
    out.insert("steps".into(), json!(steps.len()));
    for k in ["phase_wait_ms", "acting_wait_ms", "model_ms", "harness_ms", "queue_ms", "ttft_ms", "decode_ms", "action_ms", "desk_ms", "settle_ms", "observe_ms", "step_ms"] {
        let mut v: Vec<f64> = steps.iter().filter_map(|t| t[k].as_f64()).collect();
        let mean = if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
        let p50 = median(&mut v);
        let p95 = v.get((v.len() * 95 / 100).min(v.len().saturating_sub(1))).copied().unwrap_or(0.0);
        out.insert(k.into(), json!({ "mean": (mean * 10.0).round() / 10.0, "p50": p50, "p95": p95 }));
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
    let (cfg_path, pick, first) = args()?;
    let cfg: Config = serde_json::from_str(
        &std::fs::read_to_string(&cfg_path).map_err(|e| format!("{cfg_path}: {e}"))?,
    )
    .map_err(|e| format!("{cfg_path}: {e}"))?;
    let mut tasks = load_tasks(&cfg, &pick)?;
    if let Some(n) = first {
        tasks.truncate(n);
    }
    let provider = cfg.providers.get(&cfg.provider).ok_or_else(|| format!("no provider {}", cfg.provider))?;
    let model = Arc::new(model::Model::new(provider, cfg.budget.as_ref())?);
    let out = cfg.out_dir.join(&cfg.run_name);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let assigned: String = tasks.iter().map(|t| format!("{}\n", t.file)).collect();
    std::fs::write(out.join("assigned.txt"), assigned).map_err(|e| e.to_string())?;
    let trace_file = Arc::new(Mutex::new(
        std::fs::File::create(out.join("trace.jsonl")).map_err(|e| e.to_string())?,
    ));
    let all_trace = Arc::new(Mutex::new(Vec::<Value>::new()));
    let system = Arc::new(agent::system_prompt(&cfg.tasks.erp_url, &cfg.tasks.erp_user, &cfg.tasks.erp_password));
    let schema = Arc::new(agent::action_schema());
    let phases = Arc::new(agent::Phases::new(&cfg.phase_caps, &cfg.tasks.erp_url));
    let acting = Arc::new(tokio::sync::Semaphore::new(cfg.acting_cap.max(1)));
    let results_file = Arc::new(Mutex::new(
        std::fs::File::create(out.join("results.jsonl")).map_err(|e| e.to_string())?,
    ));
    let period = std::time::Duration::from_secs_f64(1.0 / cfg.start_per_s.max(0.01));
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let starts = Arc::new(tokio::sync::Mutex::new(ticks));
    let erp = match erp::Erp::login(&cfg.tasks.erp_url, &cfg.tasks.erp_user, &cfg.tasks.erp_password).await {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            eprintln!("panoptes-loop: no verification: {e}");
            None
        }
    };
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
        let (model, system, schema, cfg) = (model.clone(), system.clone(), schema.clone(), cfg.clone());
        let (trace_file, all_trace) = (trace_file.clone(), all_trace.clone());
        let (phases, erp) = (phases.clone(), erp.clone());
        let (acting, starts, results_file) = (acting.clone(), starts.clone(), results_file.clone());
        let run_dir = Path::new(&cfg.out_dir).join(&cfg.run_name).join(format!("agent-{i}"));
        handles.push(tokio::spawn(async move {
            starts.lock().await.tick().await;
            let record = |r: Value| {
                if let Ok(mut f) = results_file.lock() {
                    let _ = writeln!(f, "{r}");
                }
                r
            };
            if cfg.stop_file.as_ref().is_some_and(|p| p.exists()) {
                return record(json!({ "agent": i, "invoice": task.file, "done": false, "error": "not started: stop file" }));
            }
            let t0 = Instant::now();
            // A starting kiosk is thawed, so a start takes an acting slot.
            let permit = acting.acquire().await;
            let desk = desk::Desk::start(&cfg.desk, i, &run_dir).await;
            drop(permit);
            let mut desk = match desk {
                Ok(d) => d,
                Err(e) => {
                    return record(json!({ "agent": i, "invoice": task.file, "done": false, "error": format!("desk: {e}") }));
                }
            };
            let start_ms = t0.elapsed().as_millis();
            let file = task.file.clone();
            let outcome = agent::Run {
                desk: &mut desk,
                model: &model,
                system: &system,
                schema: &schema,
                task: task.text,
                step_cap: cfg.step_cap,
                ax_max_bytes: cfg.ax_max_bytes,
                history_full: cfg.history_full,
                phases: &phases,
                acting: &acting,
                cost_cap_usd: cfg.invoice_cost_cap_usd,
            }
            .go(|st| {
                let line = st.to_json(i, &file);
                if let Ok(mut f) = trace_file.lock() {
                    let _ = writeln!(f, "{line}");
                }
                if let Ok(mut a) = all_trace.lock() {
                    a.push(line);
                }
            })
            .await;
            desk.stop().await;
            let mut r = json!({ "agent": i, "invoice": task.file, "done": outcome.done, "steps": outcome.steps,
                    "error": outcome.error, "note": outcome.note, "reloads": outcome.reloads,
                    "retypes": outcome.retypes, "phase_wait_ms": outcome.phase_wait_ms,
                    "acting_wait_ms": outcome.acting_wait_ms,
                    "cost_usd": (outcome.cost_usd * 1e6).round() / 1e6,
                    "desk_start_ms": start_ms, "wall_ms": t0.elapsed().as_millis() });
            if let Some(erp) = erp {
                for (k, v) in erp.check(&task.truth).await.as_object().into_iter().flatten() {
                    r[k] = v.clone();
                }
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
    let mut failed: std::collections::BTreeMap<String, usize> = Default::default();
    for f in results.iter().filter_map(|r| r["failed"].as_array()).flatten().filter_map(Value::as_str) {
        *failed.entry(f.to_owned()).or_default() += 1;
    }
    let mut summary = summarize(&trace);
    summary["agents"] = json!(results.len());
    summary["claimed_done"] = json!(count(&|r| r["done"] == true));
    summary["verified"] = json!(count(&|r| r["verified"] == true));
    summary["attached"] = json!(count(&|r| r["attached"] == true));
    summary["verified_and_attached"] = json!(count(&|r| r["verified"] == true && r["attached"] == true));
    summary["claimed_not_verified"] = json!(count(&|r| r["done"] == true && r["verified"] != true));
    summary["failed_checks"] = json!(failed);
    summary["reloads"] = json!(sum("reloads"));
    summary["retypes"] = json!(sum("retypes"));
    summary["wall_s"] = json!((wall * 10.0).round() / 10.0);
    summary["steps_per_agent"] = json!(steps);
    std::fs::write(out.join("summary.json"), serde_json::to_string_pretty(&summary).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
    Ok(())
}
