//! One agent's step loop:
//!
//! ```text
//! observe (ax_tree) ─> model ─> parse {"actions":[...],"done":..} ─> desk calls ─> observe ...
//! ```
//!
//! The prompt keeps a stable prefix first so every provider's prefix cache
//! hits across steps and agents:
//!
//! ```text
//! [system: rules + tools + action format]   identical for every agent
//! [task]                                    identical for this agent's run
//! [assistant, observation] ...              older observations cut to one line
//! [latest observation]                      whole
//! ```

use crate::desk::Desk;
use crate::model::Model;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const TOOLS: &[&str] = &[
    "navigate",
    "click",
    "type",
    "key",
    "scroll",
    "wait_settled",
    "download_list",
    "upload",
    "read_pdf",
    "reload",
    "screenshot",
];

/// Brackets a PDF's text in an observation, so shortening keeps it.
const PDF_OPEN: &str = "--- PDF text";
const PDF_CLOSE: &str = "--- end of PDF text ---";

/// Calls after which the page may be changing, so the loop waits for it to
/// settle before it reads the tree.
const MUTATING: &[&str] = &[
    "navigate", "click", "key", "type", "scroll", "upload", "reload",
];

/// Reloads of an error page before the loop hands it to the model.
const RELOADS: u32 = 3;
/// Retypes of a field whose value does not read back as typed.
const RETYPES: usize = 2;

/// The job, the tools, and the action format: the stable prefix every
/// agent's prompt starts with. It holds no invoice's data.
pub fn invoice_prompt(erp: &str, erp_user: &str, erp_password: &str) -> String {
    format!(
        "You are an accounts-payable clerk operating one web browser. {SEES}\
The job: enter one supplier invoice into ERPNext.\n\
1. Sign in to the supplier's portal with the credentials in the task. Find the invoice \
whose number the task gives in the portal's invoice list, open it, and download its PDF. \
Check download_list until the file is complete; the saved name may replace / with _.\n\
2. Read the PDF with read_pdf. Note in your note: invoice number, invoice date, the \
purchase order number (PUR-ORD-...; it may be a labelled field or only mentioned in a \
sentence), each line's item, quantity and unit rate, the tax rate and amount, and the \
total. Dates may be day-first (13/10/2026, 03.09.2026) or month-first (10/13/2026); \
decide from the layout and from values that cannot be months.\n\
3. Sign in to ERPNext at {erp}/login as {erp_user} with password {erp_password}.\n\
4. Open that purchase order at {erp}/app/purchase-order/<PO number> and use its \
Create > Purchase Invoice. Never create a purchase invoice from scratch.\n\
5. On the new purchase invoice set Supplier Invoice No exactly as printed on the PDF, \
and Supplier Invoice Date. ERPNext's date fields take mm-dd-yyyy, e.g. 10-13-2026.\n\
6. Check every line's item, quantity and rate, the taxes, and the grand total against \
the PDF and fix what differs. Taxes come from the field \"Purchase Taxes and Charges \
Template\" (templates are named like \"US ST 6%\"); when the PDF has no tax, clear that \
field and delete every row of the taxes table.\n\
7. Save (Control+s). Then Submit and confirm with Yes.\n\
8. Attach the PDF to the submitted invoice: click the sidebar's \"Add attachment\" \
button and end the turn; in the dialog that opens, do not click My Device (it opens a \
system file chooser you cannot see); instead use upload with {{\"path\": <file>, \
\"selector\": \".modal.show input[type=file]\"}} and end the turn; once the file's name \
shows in the dialog, click the dialog's Upload button. The file then shows as a link in \
the sidebar. When the form shows Submitted and the PDF is listed there, reply with done \
true and no actions.\n\n{}",
        format_and_tools()
    )
}

/// The same prefix for a task file's agents: the task in the first message
/// is the job, and the tools, rules, and action format are the invoice
/// prompt's.
pub fn general_prompt() -> String {
    format!(
        "You are operating one web browser for a user. {SEES}\
The job is the task in the first message, which names the page you start on. When the \
task asks for an answer, give it in the note of the reply that sets done.\n\n{}",
        format_and_tools()
    )
}

/// What a turn shows, said right after each prompt's first sentence.
const SEES: &str = "Each turn you see the results of your last actions and the page's \
accessibility tree, one node per line:\n[node_id] role \"name\" value=\"...\" @(x,y wxh)\n\n";

/// The action format, the tools, and the rules, which end every prompt.
fn format_and_tools() -> String {
    format!(
        "Reply with one JSON object and nothing else:\n\
{{\"actions\": [{{\"tool\": \"click\", \"args\": {{\"node_id\": 42}}}}], \"done\": false, \"note\": \"...\"}}\n\n\
Tools:\n\
- navigate {{url}}: open a URL\n\
- click {{node_id}} or {{x, y}}: click a node's center or a point\n\
- type {{text, node_id?}}: focus a node and insert text\n\
- key {{key}}: Enter, Tab, Escape, Backspace, ArrowDown, Control+a, Control+s, ...\n\
- scroll {{dy}}: scroll by pixels, positive is down\n\
- wait_settled {{}}: wait for the page to finish loading\n\
- download_list {{}}: list downloaded files\n\
- upload {{path, node_id? | selector?}}: put a downloaded file (a name from download_list) into a file input, given by node or CSS selector\n\
- read_pdf {{path, max_bytes?}}: the text of a downloaded PDF (a name from download_list)\n\
- reload {{}}: reload the page\n\n\
Rules: use node ids from the latest tree only; node ids change whenever a page loads or a \
dialog opens, so end your turn after any action that does so (navigate, a click on a link, \
a menu or a button that opens something, Enter on a login form). Several actions per turn \
are fine only within one unchanged page, such as filling fields. Set done to true, with no \
actions, only after the latest page shows the task is complete. Put anything you need to \
remember in note, because older pages are shortened. \
Allowed tools: {}.",
        TOOLS.join(", ")
    )
}

pub fn action_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["actions", "done", "note"],
        "properties": {
            "actions": { "type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "required": ["tool", "args"],
                "properties": {
                    "tool": { "type": "string", "enum": TOOLS },
                    "args": { "type": "object" } } } },
            "done": { "type": "boolean" },
            "note": { "type": "string" }
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub tool: String,
    pub args: Value,
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub done: bool,
    pub note: String,
}

/// Parse a model reply: the first JSON object in it, checked against the
/// action format. Text around the object, such as a code fence, is ignored.
pub fn parse(text: &str) -> Result<Plan, String> {
    let start = text.find('{').ok_or("no JSON object in the reply")?;
    let mut de = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
    let v = de
        .next()
        .ok_or("no JSON object in the reply")?
        .map_err(|e| format!("bad JSON: {e}"))?;
    let actions = v["actions"].as_array().ok_or("actions must be an array")?;
    let mut out = Vec::new();
    for a in actions {
        let tool = a["tool"].as_str().ok_or("each action needs a tool")?;
        if !TOOLS.contains(&tool) {
            return Err(format!("unknown tool {tool}"));
        }
        let args = if a["args"].is_null() {
            json!({})
        } else {
            a["args"].clone()
        };
        if !args.is_object() {
            return Err(format!("{tool}: args must be an object"));
        }
        out.push(Action {
            tool: tool.to_owned(),
            args,
        });
    }
    Ok(Plan {
        actions: out,
        done: v["done"].as_bool().unwrap_or(false),
        note: v["note"].as_str().unwrap_or("").to_owned(),
    })
}

/// The prompt for the next model call. Observations older than the last
/// `full` shrink to their first line, which names the page.
pub fn prompt(system: &str, task: &str, turns: &[(String, String)], full: usize) -> Vec<Value> {
    let mut m = vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": task }),
    ];
    let cut = turns.len().saturating_sub(full);
    for (k, (assistant, obs)) in turns.iter().enumerate() {
        if !assistant.is_empty() {
            m.push(json!({ "role": "assistant", "content": assistant }));
        }
        let content = if k < cut { summary(obs) } else { obs.clone() };
        m.push(json!({ "role": "user", "content": content }));
    }
    m
}

fn summary(obs: &str) -> String {
    let page = obs
        .lines()
        .find(|l| l.contains("RootWebArea"))
        .map(str::trim)
        .unwrap_or("(no page)");
    let mut s = format!("(earlier observation, shortened) {page}");
    // A PDF's text is read once and needed until the end.
    if let Some(at) = obs.find(PDF_OPEN) {
        let end = obs[at..]
            .find(PDF_CLOSE)
            .map_or(obs.len(), |e| at + e + PDF_CLOSE.len());
        s.push('\n');
        s.push_str(&obs[at..end]);
    }
    s
}

/// One step's timings, written as a line of the run's trace.
#[derive(Debug, Default)]
pub struct StepTrace {
    pub step: usize,
    pub queue_ms: f64,
    /// Time the model call waited out the provider's rate-limit cooldown.
    pub rate_wait_ms: f64,
    pub ttft_ms: f64,
    pub decode_ms: f64,
    pub model_ms: f64,
    pub action_ms: f64,
    /// The desk's own time for this step's actions, as it reports it.
    pub desk_ms: f64,
    pub settle_ms: f64,
    pub observe_ms: f64,
    pub step_ms: f64,
    pub actions: usize,
    pub prompt_bytes: usize,
    pub reply_bytes: usize,
    pub prompt_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub spent_usd: Option<f64>,
    /// What the model said, what its actions returned, and the page it
    /// then saw, for reading a run back.
    pub reply: String,
    pub results: Vec<String>,
    pub page: String,
    /// The URL of the page it saw, which the model is not shown.
    pub url: String,
    /// Error pages the loop reloaded and fields it retyped this step.
    pub reloads: u32,
    pub retypes: usize,
    /// Capped phase the agent was in, and how long it waited for a slot.
    pub phase: Option<String>,
    pub phase_wait_ms: f64,
    /// Time waiting for one of the run's acting slots.
    pub acting_wait_ms: f64,
    pub error: Option<String>,
}

impl StepTrace {
    pub fn to_json(&self, agent: usize) -> Value {
        json!({
            "agent": agent, "step": self.step,
            "queue_ms": self.queue_ms, "rate_wait_ms": self.rate_wait_ms,
            "ttft_ms": self.ttft_ms, "decode_ms": self.decode_ms,
            "model_ms": self.model_ms, "action_ms": self.action_ms,
            "desk_ms": round2(self.desk_ms), "settle_ms": self.settle_ms,
            "observe_ms": self.observe_ms, "step_ms": self.step_ms,
            "harness_ms": round2(self.step_ms - self.model_ms),
            "actions": self.actions, "prompt_bytes": self.prompt_bytes,
            "reply_bytes": self.reply_bytes, "prompt_tokens": self.prompt_tokens,
            "cached_tokens": self.cached_tokens, "completion_tokens": self.completion_tokens,
            "reasoning_tokens": self.reasoning_tokens, "cost_usd": self.cost_usd,
            "spent_usd": self.spent_usd, "reply": self.reply,
            "results": self.results.iter().map(|r| clip(r, 600)).collect::<Vec<_>>(),
            "page": self.page, "url": self.url,
            "reloads": self.reloads, "retypes": self.retypes, "phase": self.phase,
            "phase_wait_ms": self.phase_wait_ms, "acting_wait_ms": self.acting_wait_ms,
            "error": self.error,
        })
    }
}

fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_owned(),
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn ms(t: Instant) -> f64 {
    round2(t.elapsed().as_secs_f64() * 1e3)
}

/// The outcome of one agent's run.
#[derive(Debug)]
pub struct Outcome {
    pub done: bool,
    pub steps: usize,
    pub error: Option<String>,
    pub note: String,
    /// The URL of the page the model saw last.
    pub url: String,
    pub reloads: u32,
    pub retypes: usize,
    pub phase_wait_ms: f64,
    pub acting_wait_ms: f64,
    pub rate_wait_ms: f64,
    pub cost_usd: f64,
}

/// Per-phase concurrency caps, shared by every agent in the run. A plan's
/// phase is the `phase=` its note declares, else `erp` when it navigates to
/// the ERP, else no capped phase when it navigates elsewhere, else the
/// agent's current one. A declared phase falls under the cap whose key it
/// starts with: `erp_login` and `erp_fill` both count against `erp`.
pub struct Phases {
    caps: Vec<(String, Arc<Semaphore>)>,
    erp: String,
}

impl Phases {
    pub fn new(caps: &BTreeMap<String, usize>, erp_url: &str) -> Self {
        Phases {
            caps: caps
                .iter()
                .filter(|(_, n)| **n > 0)
                .map(|(k, n)| (k.clone(), Arc::new(Semaphore::new(*n))))
                .collect(),
            erp: erp_url.trim_end_matches('/').to_owned(),
        }
    }

    fn cap(&self, phase: &str) -> Option<&(String, Arc<Semaphore>)> {
        self.caps
            .iter()
            .find(|(k, _)| phase.starts_with(k.as_str()))
    }

    fn of(&self, plan: &Plan, current: Option<&str>) -> Option<String> {
        if let Some(p) = plan
            .note
            .split([';', ' ', ','])
            .find_map(|w| w.strip_prefix("phase="))
        {
            return self.cap(p).map(|c| c.0.clone());
        }
        let url = plan
            .actions
            .iter()
            .rev()
            .find(|a| a.tool == "navigate")
            .and_then(|a| a.args["url"].as_str());
        match url {
            Some(u) if u.starts_with(&self.erp) => self.cap("erp").map(|c| c.0.clone()),
            Some(_) => None,
            None => current.map(str::to_owned),
        }
    }
}

const ERROR_PAGES: &[&str] = &[
    "Server Error",
    "Something went wrong",
    "There was an error building this page",
    "Internal Server Error",
    "Bad Gateway",
    "Service Unavailable",
    "Gateway Time",
    "This site can\u{2019}t be reached",
    "This page isn\u{2019}t working",
];

/// Why `tree` is an error page: its title or a heading outside any dialog
/// matches a known error text, or its title is an HTTP 5xx status. A
/// dialog's "Server Error" is the app reporting a failed call; the page
/// behind it is still up, so it is left to the model.
pub fn error_page(tree: &str) -> Option<String> {
    let mut dialog: Option<usize> = None;
    for line in tree.lines() {
        let l = line.trim_start();
        let depth = line.len() - l.len();
        let Some((_, rest)) = l.strip_prefix('[').and_then(|r| r.split_once("] ")) else {
            continue;
        };
        let (role, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        if dialog.is_some_and(|d| depth <= d) {
            dialog = None;
        }
        if role == "dialog" || role == "alertdialog" {
            dialog.get_or_insert(depth);
        }
        if dialog.is_some() {
            continue;
        }
        let name = rest
            .strip_prefix('"')
            .map(|r| quoted(r).0)
            .unwrap_or_default();
        let known = ERROR_PAGES.iter().any(|p| name.contains(p));
        let hit = match role {
            "RootWebArea" => known || http_5xx(&name),
            "heading" => known,
            _ => false,
        };
        if hit {
            return Some(format!("{role} {name:?}"));
        }
    }
    None
}

/// A bare 5xx code, or one beside "Error": "503", "Error 500", "500 Error".
/// The known texts already cover "502 Bad Gateway" and its kind.
fn http_5xx(title: &str) -> bool {
    let w: Vec<&str> = title.split_whitespace().collect();
    let code = w
        .iter()
        .any(|t| t.parse::<u16>().is_ok_and(|c| (500..600).contains(&c)));
    code && (w.len() == 1 || w.iter().any(|t| t.trim_matches(':') == "Error"))
}

/// The value of node `id` in `tree`, `Some("")` when it shows none, `None`
/// when the node is gone.
pub fn node_value(tree: &str, id: i64) -> Option<String> {
    let head = format!("[{id}] ");
    let l = tree
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with(&head))?;
    Some(
        l.split_once(" value=\"")
            .map(|(_, r)| quoted(r).0)
            .unwrap_or_default(),
    )
}

/// Whether a field reads back as `want`: equal up to whitespace, a prefix
/// when the tree clipped it, or the same length of bullets for a password.
fn reads_as(got: &str, want: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let (g, w) = (norm(got), norm(want));
    if g == w {
        return true;
    }
    if let Some(head) = g.strip_suffix('…') {
        return w.starts_with(head);
    }
    !got.is_empty() && got.chars().all(|c| c == '•') && got.chars().count() == want.chars().count()
}

/// One `{:?}` string after its opening quote: the text and the rest of the
/// line after the closing quote.
fn quoted(s: &str) -> (String, &str) {
    let mut out = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '"' => return (out, &s[i + 1..]),
            '\\' => match it.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, 'u')) => {
                    let hex: String = it
                        .by_ref()
                        .map(|(_, h)| h)
                        .take_while(|h| *h != '}')
                        .filter(|h| *h != '{')
                        .collect();
                    out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                Some((_, c)) => out.push(c),
                None => {}
            },
            c => out.push(c),
        }
    }
    (out, "")
}

struct StepEnd {
    /// `Some(Ok)` when the plan said done, `Some(Err)` when the run ends on an
    /// error, `None` to keep going.
    end: Option<Result<(), String>>,
    note: Option<String>,
}

pub struct Run<'a> {
    pub desk: &'a mut Desk,
    pub model: &'a Model,
    pub system: &'a str,
    pub schema: &'a Value,
    pub task: String,
    /// The page to open before the first observation; without one the
    /// agent starts on whatever the desk shows.
    pub start_url: Option<&'a str>,
    pub step_cap: usize,
    pub ax_max_bytes: usize,
    pub history_full: usize,
    pub phases: &'a Phases,
    /// The run's acting slots: held from a step's first desk call through
    /// its observation, never across a model call.
    pub acting: &'a Semaphore,
    /// End the run once its model calls cost more than this.
    pub cost_cap_usd: Option<f64>,
}

impl Run<'_> {
    pub async fn go(mut self, mut trace: impl FnMut(StepTrace)) -> Outcome {
        let mut turns: Vec<(String, String)> = Vec::new();
        let first = Instant::now();
        let mut out = Outcome {
            done: false,
            steps: 0,
            error: None,
            note: String::new(),
            url: String::new(),
            reloads: 0,
            retypes: 0,
            phase_wait_ms: 0.0,
            acting_wait_ms: 0.0,
            rate_wait_ms: 0.0,
            cost_usd: 0.0,
        };
        let mut st = StepTrace::default();
        let permit = self.acting.acquire().await;
        let obs = self.start(&mut st).await;
        drop(permit);
        let obs = match obs {
            Ok(o) => o,
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        };
        st.step_ms = ms(first);
        out.url.clone_from(&st.url);
        trace(st);
        turns.push((String::new(), obs));
        let mut phase: Option<String> = None;
        let mut slot: Option<OwnedSemaphorePermit> = None;
        for step in 1..=self.step_cap {
            out.steps = step;
            let t0 = Instant::now();
            let mut st = StepTrace {
                step,
                ..StepTrace::default()
            };
            let r = self.step(&mut st, &mut turns, &mut phase, &mut slot).await;
            st.step_ms = ms(t0);
            out.reloads += st.reloads;
            out.retypes += st.retypes;
            out.phase_wait_ms = round2(out.phase_wait_ms + st.phase_wait_ms);
            out.acting_wait_ms = round2(out.acting_wait_ms + st.acting_wait_ms);
            out.rate_wait_ms = round2(out.rate_wait_ms + st.rate_wait_ms);
            out.cost_usd += st.cost_usd.unwrap_or(0.0);
            if let Some(n) = r.note {
                out.note = n;
            }
            // A step that ends before it observes leaves the last page.
            if !st.url.is_empty() {
                out.url.clone_from(&st.url);
            }
            trace(st);
            match r.end {
                Some(Ok(())) => {
                    out.done = true;
                    return out;
                }
                Some(Err(e)) => {
                    out.error = Some(e);
                    return out;
                }
                None => {}
            }
            if let Some(cap) = self.cost_cap_usd
                && out.cost_usd > cap
            {
                out.error = Some(format!("cost cap: ${:.4} over ${cap}", out.cost_usd));
                return out;
            }
        }
        out.error = Some("step cap reached".into());
        out
    }

    /// Step 0: open the start page, if there is one, and read the page. The
    /// navigate's result line is shown with the page, as any action's is.
    async fn start(&mut self, st: &mut StepTrace) -> Result<String, String> {
        let t = Instant::now();
        let Some(url) = self.start_url else {
            let obs = self.observe(&[], st).await?;
            st.observe_ms = ms(t);
            return Ok(obs);
        };
        let a = Action {
            tool: "navigate".into(),
            args: json!({ "url": url }),
        };
        let r = self.desk.call(&a.tool, &a.args).await?;
        st.actions = 1;
        st.action_ms = ms(t);
        st.desk_ms = r.total_ms;
        let results = vec![result_line(&a, &r)];
        st.results.clone_from(&results);
        let to = Instant::now();
        let (obs, settle_ms) = self.observe_after(true, &results, st).await?;
        st.settle_ms = settle_ms;
        st.observe_ms = round2(ms(to) - settle_ms);
        Ok(obs)
    }

    async fn step(
        &mut self,
        st: &mut StepTrace,
        turns: &mut Vec<(String, String)>,
        phase: &mut Option<String>,
        slot: &mut Option<OwnedSemaphorePermit>,
    ) -> StepEnd {
        let fail = |st: &mut StepTrace, e: String, note: Option<String>| {
            st.error = Some(e.clone());
            StepEnd {
                end: Some(Err(e)),
                note,
            }
        };
        let messages = prompt(self.system, &self.task, turns, self.history_full);
        st.prompt_bytes = messages
            .iter()
            .map(|m| m["content"].as_str().map_or(0, str::len))
            .sum();
        let tm = Instant::now();
        let reply = self.model.complete(&messages, self.schema).await;
        st.model_ms = ms(tm);
        let c = match reply {
            Ok(c) => c,
            Err(e) => {
                st.rate_wait_ms = e.rate_wait_ms;
                return fail(st, e.error, None);
            }
        };
        st.queue_ms = c.queue_ms;
        st.rate_wait_ms = c.rate_wait_ms;
        st.ttft_ms = c.ttft_ms;
        st.decode_ms = c.decode_ms;
        st.reply_bytes = c.text.len();
        st.prompt_tokens = c.prompt_tokens;
        st.cached_tokens = c.cached_tokens;
        st.completion_tokens = c.completion_tokens;
        st.reasoning_tokens = c.reasoning_tokens;
        st.cost_usd = c.cost_usd;
        st.spent_usd = c.spent_usd;
        st.reply.clone_from(&c.text);
        let plan = match parse(&c.text) {
            Ok(p) => p,
            Err(e) => {
                // A malformed reply is the model's mistake: tell it and go on.
                st.error = Some(e.clone());
                let last = turns.last().map(|t| t.1.clone()).unwrap_or_default();
                turns.push((c.text, format!("Your reply was rejected: {e}\n\n{last}")));
                return StepEnd {
                    end: None,
                    note: None,
                };
            }
        };
        let note = Some(plan.note.clone());
        // Entering a capped phase waits for a slot with the desk idle, so
        // under resd the kiosk stays frozen while it waits.
        let want = self.phases.of(&plan, phase.as_deref());
        if want != *phase {
            drop(slot.take());
            if let Some((_, sem)) = want.as_deref().and_then(|p| self.phases.cap(p)) {
                let tw = Instant::now();
                *slot = sem.clone().acquire_owned().await.ok();
                st.phase_wait_ms = ms(tw);
            }
            *phase = want;
        }
        st.phase.clone_from(phase);
        st.actions = plan.actions.len();
        let tw = Instant::now();
        let _acting = self.acting.acquire().await;
        st.acting_wait_ms = ms(tw);
        let mut results = Vec::new();
        let ta = Instant::now();
        for a in &plan.actions {
            match self.desk.call(&a.tool, &a.args).await {
                Ok(r) => {
                    st.desk_ms += r.total_ms;
                    results.push(result_line(a, &r));
                    if r.ok && a.tool == "type" {
                        match self.read_back(a).await {
                            Ok(n) => st.retypes += n,
                            Err(e) => return fail(st, e, note),
                        }
                    }
                }
                Err(e) => return fail(st, e, note),
            }
        }
        st.action_ms = ms(ta);
        st.results.clone_from(&results);
        // Done is a claim about a page the model has seen, so a reply that
        // still acts is not done: show it what its actions did first.
        if plan.done && plan.actions.is_empty() {
            return StepEnd {
                end: Some(Ok(())),
                note,
            };
        }
        if plan.done {
            results.push(
                "- the loop did not accept done: a reply that sets done must have no actions. \
Check this page; if the task is complete, reply with no actions and done true."
                    .to_owned(),
            );
        }
        let mutated = plan
            .actions
            .iter()
            .any(|a| MUTATING.contains(&a.tool.as_str()));
        let to = Instant::now();
        match self.observe_after(mutated, &results, st).await {
            Ok((obs, settle_ms)) => {
                st.settle_ms = settle_ms;
                st.observe_ms = round2(ms(to) - settle_ms);
                st.page = obs
                    .lines()
                    .find(|l| l.contains("RootWebArea"))
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                turns.push((c.text, obs));
                StepEnd { end: None, note }
            }
            Err(e) => fail(st, e, note),
        }
    }

    /// After `type` into a node, read the node back and retype it whole
    /// (select all, type) while it differs. Returns the retypes.
    async fn read_back(&mut self, a: &Action) -> Result<usize, String> {
        let (Some(id), Some(want)) = (a.args["node_id"].as_i64(), a.args["text"].as_str()) else {
            return Ok(0);
        };
        for n in 0..=RETYPES {
            let r = self
                .desk
                .call("ax_tree", &json!({ "max_bytes": self.ax_max_bytes }))
                .await?;
            let got = node_value(r.result["tree"].as_str().unwrap_or(""), id);
            // A node that is gone was replaced by the app; nothing to check.
            if got.as_deref().is_none_or(|g| reads_as(g, want)) || n == RETYPES {
                return Ok(n);
            }
            self.desk.call("click", &json!({ "node_id": id })).await?;
            self.desk
                .call("key", &json!({ "key": "Control+a" }))
                .await?;
            self.desk.call("type", &json!({ "text": want })).await?;
        }
        Ok(RETYPES)
    }

    /// Wait for the page to settle, read it, and reload an error page with
    /// backoff (0.5 s, 1 s, 2 s) up to [`RELOADS`] times. Reload time counts
    /// as settle time.
    async fn observe_after(
        &mut self,
        mutated: bool,
        results: &[String],
        st: &mut StepTrace,
    ) -> Result<(String, f64), String> {
        let t = Instant::now();
        if mutated {
            self.desk
                .call(
                    "wait_settled",
                    &json!({ "quiet_ms": 300, "timeout_ms": 5000 }),
                )
                .await?;
        }
        let mut settle = ms(t);
        let mut obs = self.observe(results, st).await?;
        let mut notes = results.to_vec();
        while let Some(why) = obs
            .split_once("Page:\n")
            .and_then(|(_, tree)| error_page(tree))
        {
            if st.reloads == RELOADS {
                break;
            }
            let t = Instant::now();
            tokio::time::sleep(Duration::from_millis(500 << st.reloads)).await;
            st.reloads += 1;
            self.desk.call("reload", &json!({})).await?;
            self.desk
                .call(
                    "wait_settled",
                    &json!({ "quiet_ms": 300, "timeout_ms": 5000 }),
                )
                .await?;
            settle += ms(t);
            notes.push(format!(
                "- the loop reloaded an error page ({why}), attempt {}",
                st.reloads
            ));
            obs = self.observe(&notes, st).await?;
        }
        Ok((obs, round2(settle)))
    }

    /// Read the page into an observation, and its URL into `st`.
    async fn observe(&mut self, results: &[String], st: &mut StepTrace) -> Result<String, String> {
        let r = self
            .desk
            .call("ax_tree", &json!({ "max_bytes": self.ax_max_bytes }))
            .await?;
        if !r.ok {
            return Err(format!("ax_tree: {}", r.error.unwrap_or_default()));
        }
        r.result["url"]
            .as_str()
            .unwrap_or("")
            .clone_into(&mut st.url);
        let tree = r.result["tree"].as_str().unwrap_or("");
        let mut obs = String::new();
        if !results.is_empty() {
            obs.push_str("Results:\n");
            for l in results {
                obs.push_str(l);
                obs.push('\n');
            }
            obs.push('\n');
        }
        obs.push_str("Page:\n");
        obs.push_str(tree);
        Ok(obs)
    }
}

fn result_line(a: &Action, r: &crate::desk::Reply) -> String {
    if !r.ok {
        return format!(
            "- {} {}: error: {}",
            a.tool,
            a.args,
            r.error.as_deref().unwrap_or("?")
        );
    }
    let res = match a.tool.as_str() {
        "download_list" | "navigate" | "wait_settled" => {
            let s = r.result.to_string();
            match s.char_indices().nth(1500) {
                Some((at, _)) => format!("{}…", &s[..at]),
                None => s,
            }
        }
        "read_pdf" => {
            let text = r.result["text"].as_str().unwrap_or("");
            let cut = if r.result["truncated"] == true {
                " (cut short)"
            } else {
                ""
            };
            format!(
                "ok\n{PDF_OPEN} of {}{cut} ---\n{text}\n{PDF_CLOSE}",
                a.args["path"].as_str().unwrap_or("?")
            )
        }
        _ => "ok".to_owned(),
    };
    format!("- {} {}: {res}", a.tool, a.args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_first_object_and_ignores_fences() {
        let p = parse("```json\n{\"actions\":[{\"tool\":\"key\",\"args\":{\"key\":\"Enter\"}}],\"done\":false,\"note\":\"n\"}\n```").unwrap();
        assert_eq!(
            p.actions,
            vec![Action {
                tool: "key".into(),
                args: json!({"key": "Enter"})
            }]
        );
        assert_eq!(p.note, "n");
        assert!(!p.done);
    }

    #[test]
    fn finds_error_pages_but_not_app_dialogs() {
        assert!(error_page("[1] RootWebArea \"502 Bad Gateway\"\n").is_some());
        assert!(
            error_page("[1] RootWebArea \"Login\"\n  [2] heading \"Something went wrong\"\n")
                .is_some()
        );
        let dialog = "[1] RootWebArea \"PI-1 - Purchase Invoice\"\n  [2] dialog \"\"\n    [3] heading \"Server Error\"\n  [4] heading \"Details\"\n";
        assert_eq!(error_page(dialog), None);
        assert_eq!(error_page("[1] RootWebArea \"500 invoices\"\n"), None);
        assert!(error_page("[1] RootWebArea \"503\"\n").is_some());
        assert_eq!(error_page("[1] RootWebArea \"Invoices\"\n"), None);
    }

    #[test]
    fn reads_values_back() {
        let tree = "[1] RootWebArea \"x\"\n  [7] textbox \"Supplier Invoice No\" value=\"INV-\\\"1\" focusable @(1,2 3x4)\n  [8] textbox \"Date\" @(1,2 3x4)\n";
        assert_eq!(node_value(tree, 7).as_deref(), Some("INV-\"1"));
        assert_eq!(node_value(tree, 8).as_deref(), Some(""));
        assert_eq!(node_value(tree, 9), None);
        assert!(reads_as(" INV-1 ", "INV-1"));
        assert!(!reads_as("08-13-2026INV-1", "INV-1"));
        assert!(reads_as("•••••", "admin"));
    }

    #[test]
    fn phases_come_from_the_note_then_the_url() {
        let ph = Phases::new(
            &BTreeMap::from([("erp".to_owned(), 2)]),
            "http://127.0.0.1:8080/",
        );
        let plan = |note: &str, url: Option<&str>| Plan {
            actions: url
                .map(|u| Action {
                    tool: "navigate".into(),
                    args: json!({ "url": u }),
                })
                .into_iter()
                .collect(),
            done: false,
            note: note.into(),
        };
        assert_eq!(
            ph.of(&plan("phase=erp_fill;n=2", None), None).as_deref(),
            Some("erp")
        );
        assert_eq!(
            ph.of(&plan("phase=portal_open;n=0", None), Some("erp")),
            None
        );
        assert_eq!(
            ph.of(&plan("", Some("http://127.0.0.1:8080/login")), None)
                .as_deref(),
            Some("erp")
        );
        assert_eq!(
            ph.of(&plan("", Some("http://127.0.0.1:9001/")), Some("erp")),
            None
        );
        assert_eq!(ph.of(&plan("", None), Some("erp")).as_deref(), Some("erp"));
    }

    #[test]
    fn rejects_unknown_tools() {
        assert!(parse(r#"{"actions":[{"tool":"rm","args":{}}],"done":false}"#).is_err());
    }

    #[test]
    fn keeps_pdf_text_when_shortening() {
        let obs = format!(
            "Results:\n- read_pdf {{}}: ok\n{PDF_OPEN} of a.pdf ---\nTotal 9.00\n{PDF_CLOSE}\n\nPage:\n[1] RootWebArea \"P\"\n[2] button \"x\""
        );
        let s = summary(&obs);
        assert!(s.contains("Total 9.00") && s.ends_with(PDF_CLOSE) && !s.contains("button"));
    }

    #[test]
    fn shortens_old_observations() {
        let turns: Vec<(String, String)> = (0..5)
            .map(|i| {
                (
                    format!("a{i}"),
                    format!("Page:\n[1] RootWebArea \"P{i}\"\n[2] button \"x\""),
                )
            })
            .collect();
        let m = prompt("S", "T", &turns, 3);
        let users: Vec<&str> = m
            .iter()
            .filter(|x| x["role"] == "user")
            .filter_map(|x| x["content"].as_str())
            .collect();
        assert_eq!(users[0], "T");
        assert!(users[1].starts_with("(earlier observation, shortened) [1] RootWebArea \"P0\""));
        assert!(users[5].contains("button"));
    }
}
