//! The invoice-entry flow as a phase machine. Each reply carries its state
//! in `note` (`phase=erp_fill;n=1;r=0;f=0`), so the next request picks up
//! from the conversation alone, and the latest tree decides whether the
//! phase's target has appeared yet.
//!
//! ```text
//! start ─> portal_login ─> portal_open ─> portal_download ─> portal_check
//!   ─> erp_login ─> erp_po ─> erp_create ─> erp_pick ─> erp_fill ⟲ (read back)
//!   ─> erp_tax ⟲ (clear template, delete rows, pick template) ─> erp_saved
//!   ─> erp_confirm ─> erp_done ─> erp_attach ─> erp_upload ─> erp_send
//!   ─> erp_attached ─> done
//!
//! erp_saved ─(draft differs from the invoice)─> erp_fill      f += 1
//! any phase ─(error page)─> same phase, reload                r += 1
//! ```
//!
//! `n` counts waits in a phase, `r` consecutive error-page reloads, and `f`
//! pre-submit fixes over the whole run.

use crate::tree::{Node, find, has};
use serde_json::{Value, json};

/// What the oracle knows about one agent's invoice.
#[derive(Debug, Clone)]
pub struct Truth {
    pub file: String,
    pub invoice_no: String,
    /// ISO date, yyyy-mm-dd.
    pub date: String,
    pub po: String,
    pub tax_template: String,
    /// (item_code, qty) per line.
    pub lines: Vec<(String, f64)>,
    pub total: f64,
    pub portal_login: String,
    pub portal_user: String,
    pub portal_password: String,
    pub erp: String,
    pub erp_user: String,
    pub erp_password: String,
}

/// The state a note carries.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub phase: String,
    pub n: u32,
    pub reloads: u32,
    pub fixes: u32,
}

#[derive(Debug, PartialEq)]
pub struct Reply {
    pub actions: Vec<Value>,
    pub done: bool,
    pub phase: String,
    pub n: u32,
    pub reloads: u32,
    pub fixes: u32,
}

impl Reply {
    pub fn to_json(&self) -> Value {
        json!({ "actions": self.actions, "done": self.done,
                "note": format!("phase={};n={};r={};f={}", self.phase, self.n, self.reloads, self.fixes) })
    }
}

/// Read `phase=X;n=N;r=R;f=F` back out of a note.
pub fn state_of(note: &str) -> State {
    let mut s = State {
        phase: "start".into(),
        n: 0,
        reloads: 0,
        fixes: 0,
    };
    for kv in note.split(';') {
        match kv.split_once('=') {
            Some(("phase", p)) => p.clone_into(&mut s.phase),
            Some(("n", v)) => s.n = v.parse().unwrap_or(0),
            Some(("r", v)) => s.reloads = v.parse().unwrap_or(0),
            Some(("f", v)) => s.fixes = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    s
}

fn nav(url: &str) -> Value {
    json!({ "tool": "navigate", "args": { "url": url } })
}
fn click(n: &Node) -> Value {
    json!({ "tool": "click", "args": { "node_id": n.id } })
}
fn typ(n: &Node, text: &str) -> Value {
    json!({ "tool": "type", "args": { "node_id": n.id, "text": text } })
}
fn key(k: &str) -> Value {
    json!({ "tool": "key", "args": { "key": k } })
}
fn wait() -> Value {
    json!({ "tool": "wait_settled", "args": { "quiet_ms": 400, "timeout_ms": 3000 } })
}

/// Phases after which a wait counts against the phase; past this the oracle
/// gives up so the run ends instead of spinning to the step cap.
const PATIENCE: u32 = 20;
/// Error-page reloads in a row before the phase's own handling takes over.
const RELOADS: u32 = 3;
/// Pre-submit fixes before the oracle gives up on the draft.
const FIXES: u32 = 3;
/// ERPNext's confirm text when saving a draft opened on an earlier day.
const POSTING_DATE_MOVES: &str = "Posting Date will change to today's date";

fn to(phase: &str, actions: Vec<Value>) -> Reply {
    Reply {
        actions,
        done: false,
        phase: phase.to_owned(),
        n: 0,
        reloads: 0,
        fixes: 0,
    }
}

fn stay(phase: &str, n: u32, actions: Vec<Value>) -> Reply {
    if n + 1 >= PATIENCE {
        return Reply {
            done: true,
            phase: format!("stuck_{phase}"),
            n: n + 1,
            ..to("", vec![])
        };
    }
    Reply {
        n: n + 1,
        ..to(phase, actions)
    }
}

/// yyyy-mm-dd to ERPNext's mm-dd-yyyy.
fn erp_date(iso: &str) -> String {
    let p: Vec<&str> = iso.split('-').collect();
    match p.as_slice() {
        [y, m, d] => format!("{m}-{d}-{y}"),
        _ => iso.to_owned(),
    }
}

/// A link field shows the template's title ("US ST 4%") once it loses
/// focus and its name ("US ST 4% - PDC") while it is being edited.
fn same_template(shown: &str, name: &str) -> bool {
    shown == name
        || name
            .strip_prefix(shown)
            .is_some_and(|rest| rest.starts_with(" - "))
}

/// Checkboxes of the taxes table's data rows: those after the table's
/// header checkbox and before its "Add row" button.
fn tax_rows(nodes: &[Node]) -> Vec<&Node> {
    let Some((start, _)) = find(nodes, "StaticText", "Purchase Taxes and Charges") else {
        return vec![];
    };
    nodes[start..]
        .iter()
        .take_while(|n| !(n.role == "button" && n.name == "Add row"))
        .filter(|n| n.role == "checkbox" && n.id.is_some())
        .skip(1)
        .collect()
}

/// The items table's rows as (item_code, qty), from its "Accepted Qty"
/// header to its "Add row" button. Each row is a checkbox, its number, a
/// link "CODE: name", then the qty:
///
/// ```text
/// [117892] checkbox focusable
/// [121804] StaticText "1"
/// [121780] link "DEMO-ITM-004: Stretch Wrap Film, 18 in"
/// [121806] StaticText "12"
/// ```
fn item_rows(nodes: &[Node]) -> Option<Vec<(String, f64)>> {
    let (start, _) = find(nodes, "StaticText", "Accepted Qty")?;
    let rows: Vec<&Node> = nodes[start..]
        .iter()
        .take_while(|n| !(n.role == "button" && n.name == "Add row"))
        .collect();
    let mut out = Vec::new();
    for (k, n) in rows.iter().enumerate() {
        if n.role == "link" {
            let code = n.name.split(':').next().unwrap_or("").to_owned();
            let qty = rows
                .get(k + 1)
                .and_then(|q| q.name.replace(',', "").parse().ok())
                .unwrap_or(f64::NAN);
            out.push((code, qty));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// The form's grand total: the text after its "Grand Total (USD)" label,
/// "$ 8,408.44".
fn grand_total(nodes: &[Node]) -> Option<f64> {
    let (at, _) = nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.role == "StaticText" && n.name.starts_with("Grand Total ("))?;
    let v = nodes[at + 1..].iter().find(|n| n.role == "StaticText")?;
    v.name
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect::<String>()
        .parse()
        .ok()
}

/// What a saved draft gets wrong against the invoice, before Submit. The
/// form never shows the items' purchase order; the rows came from the PO's
/// Create menu, so matching items and quantities stand in for it. A check
/// whose field is not in the tree passes: absence is not a mismatch.
fn draft_mismatch(t: &Truth, nodes: &[Node]) -> Option<String> {
    let mut bad = Vec::new();
    match find(nodes, "textbox", "Supplier Invoice No") {
        Some((_, n)) if n.value != t.invoice_no => bad.push(format!("bill_no {:?}", n.value)),
        _ => {}
    }
    match find(nodes, "textbox", "Supplier Invoice Date") {
        Some((_, n)) if n.value != erp_date(&t.date) => bad.push(format!("date {:?}", n.value)),
        _ => {}
    }
    if let Some(rows) = item_rows(nodes) {
        let mut want = t.lines.clone();
        want.sort_by(|a, b| a.0.cmp(&b.0));
        let same = rows.len() == want.len()
            && rows
                .iter()
                .zip(&want)
                .all(|(a, b)| a.0 == b.0 && (a.1 - b.1).abs() < 1e-6);
        if !same {
            bad.push(format!("lines {}/{}", rows.len(), want.len()));
        }
    }
    match grand_total(nodes) {
        Some(g) if (g - t.total).abs() >= 0.006 => bad.push(format!("total {g}")),
        _ => {}
    }
    (!bad.is_empty()).then(|| bad.join(", "))
}

/// Set a field to `want`: nothing when it already reads so, select-all
/// and type over a wrong value, type into an empty one.
fn set(a: &mut Vec<Value>, n: &Node, want: &str) {
    if n.value == want {
        return;
    }
    if !n.value.is_empty() {
        a.extend([click(n), key("Control+a")]);
    }
    a.push(typ(n, want));
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

/// Whether the page is an error page: its title or a heading outside any
/// dialog matches a known error text, or its title is a bare 5xx status.
/// A dialog's "Server Error" is the app reporting a failed call; the page
/// behind it is still up.
fn error_page(nodes: &[Node]) -> bool {
    let mut dialog: Option<usize> = None;
    for x in nodes {
        if dialog.is_some_and(|d| x.depth <= d) {
            dialog = None;
        }
        if x.role == "dialog" || x.role == "alertdialog" {
            dialog.get_or_insert(x.depth);
        }
        if dialog.is_some() || (x.role != "RootWebArea" && x.role != "heading") {
            continue;
        }
        let w: Vec<&str> = x.name.split_whitespace().collect();
        let code = w
            .iter()
            .any(|t| t.parse::<u16>().is_ok_and(|c| (500..600).contains(&c)));
        let status = x.role == "RootWebArea" && code && (w.len() == 1 || w.contains(&"Error"));
        if status || ERROR_PAGES.iter().any(|p| x.name.contains(p)) {
            return true;
        }
    }
    false
}

/// The first `role` named `name` inside the last open dialog.
fn in_dialog<'a>(nodes: &'a [Node], role: &str, name: &str) -> Option<&'a Node> {
    let (at, d) = nodes
        .iter()
        .enumerate()
        .rfind(|(_, n)| n.role == "dialog")?;
    nodes[at + 1..]
        .iter()
        .take_while(|n| n.depth > d.depth)
        .find(|n| n.role == role && n.name == name && n.id.is_some())
}

/// Whether this observation's `download_list` result shows a finished PDF.
fn downloaded_pdf(results: &str) -> bool {
    results
        .lines()
        .filter(|l| l.contains("download_list"))
        .any(|l| {
            let Some(at) = l.find(": {") else {
                return false;
            };
            match serde_json::from_str::<Value>(&l[at + 2..]) {
                Ok(v) => v["files"].as_array().is_some_and(|f| {
                    f.iter().any(|f| {
                        f["complete"] == true
                            && f["name"].as_str().is_some_and(|n| n.ends_with(".pdf"))
                    })
                }),
                // The loop clips long results; the files come first.
                Err(_) => l.contains("\"complete\":true,\"name\":") && l.contains(".pdf\""),
            }
        })
}

fn login_fields(nodes: &[Node]) -> Option<(&Node, &Node)> {
    let pass = nodes
        .iter()
        .find(|n| n.role == "textbox" && n.name == "Password" && n.id.is_some())?;
    let user = nodes
        .iter()
        .find(|n| n.role == "textbox" && n.name != "Password" && n.id.is_some())?;
    Some((user, pass))
}

/// The portal names the PDF after the invoice number, with characters a
/// file name cannot carry as `_`: `BWF/26/0115` saves as `BWF_26_0115.pdf`.
fn pdf_name(t: &Truth) -> String {
    let safe: String = t
        .invoice_no
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-._".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.pdf")
}

/// Back to "Add attachment" after a failed upload. Counts as a fix, so
/// the retries are bounded where waits in one phase alone would not be.
fn reattach() -> Reply {
    Reply {
        fixes: 1,
        ..to("erp_attach", vec![key("Escape")])
    }
}

/// The next reply. An error page is reloaded in place, with a growing
/// quiet wait first, up to [`RELOADS`] times in a row; past that, and on
/// any other page, the phase decides.
pub fn next(t: &Truth, s: &State, results: &str, nodes: &[Node]) -> Reply {
    let error = error_page(nodes);
    if error && s.reloads < RELOADS {
        let quiet = 500u64 << s.reloads;
        let wait = json!({ "tool": "wait_settled", "args": { "quiet_ms": quiet, "timeout_ms": quiet + 2000 } });
        let reload = json!({ "tool": "reload", "args": {} });
        return Reply {
            reloads: s.reloads + 1,
            fixes: s.fixes,
            n: s.n,
            ..to(&s.phase, vec![wait, reload])
        };
    }
    let mut r = phase(t, &s.phase, s.n, results, nodes);
    r.fixes += s.fixes;
    // Still an error page after the reloads: no more until a good page.
    if error {
        r.reloads = s.reloads;
    }
    if r.fixes > FIXES {
        let what = if attaching(&s.phase) || attaching(&r.phase) {
            "stuck_erp_attach"
        } else {
            "stuck_erp_verify"
        };
        return Reply {
            done: true,
            phase: what.into(),
            fixes: r.fixes,
            ..to("", vec![])
        };
    }
    r
}

fn attaching(phase: &str) -> bool {
    matches!(
        phase,
        "erp_attach" | "erp_upload" | "erp_send" | "erp_attached"
    )
}

fn phase(t: &Truth, phase: &str, n: u32, results: &str, nodes: &[Node]) -> Reply {
    let base = t.portal_login.trim_end_matches("/login");
    let page = nodes.first().map(|n| n.name.as_str()).unwrap_or("");
    match phase {
        "start" => to("portal_login", vec![nav(&t.portal_login)]),
        "portal_login" => match login_fields(nodes) {
            Some((u, p)) => to(
                "portal_open",
                vec![
                    typ(u, &t.portal_user),
                    typ(p, &t.portal_password),
                    key("Enter"),
                ],
            ),
            None => stay(phase, n, vec![wait()]),
        },
        "portal_open" => to(
            "portal_download",
            vec![nav(&format!("{base}/invoices/{}", t.file))],
        ),
        "portal_download" => match find(nodes, "link", "Download PDF") {
            Some((_, l)) => to(
                "portal_check",
                vec![
                    click(l),
                    wait(),
                    json!({ "tool": "download_list", "args": {} }),
                ],
            ),
            None if login_fields(nodes).is_some() => to("portal_login", vec![wait()]),
            None => stay(phase, n, vec![wait()]),
        },
        "portal_check" => {
            if downloaded_pdf(results) {
                to("erp_login", vec![nav(&format!("{}/login", t.erp))])
            } else {
                stay(
                    phase,
                    n,
                    vec![wait(), json!({ "tool": "download_list", "args": {} })],
                )
            }
        }
        "erp_login" => match (
            find(nodes, "textbox", "Email"),
            find(nodes, "textbox", "Password"),
        ) {
            (Some((_, u)), Some((_, p))) => to(
                "erp_po",
                vec![typ(u, &t.erp_user), typ(p, &t.erp_password), key("Enter")],
            ),
            _ => stay(phase, n, vec![wait()]),
        },
        // Under concurrent logins ERPNext answers some with HTTP 500 and
        // the form shows "Something went wrong."; the fields stay filled,
        // so pressing Continue again is the retry.
        "erp_po" => match find(nodes, "button", "Continue") {
            Some((_, b)) if has(nodes, "StaticText", "Something went wrong.") && n < 8 => {
                stay(phase, n, vec![click(b), wait()])
            }
            Some(_) if has(nodes, "textbox", "Password") && n < 8 => stay(phase, n, vec![wait()]),
            _ => to(
                "erp_create",
                vec![nav(&format!("{}/app/purchase-order/{}", t.erp, t.po))],
            ),
        },
        // Under load ERPNext sometimes answers the form route with "500 :
        // There was an error building this page"; loading it again works.
        "erp_create" => match find(nodes, "button", "Create") {
            Some((_, b)) if page.contains(&t.po) => to("erp_pick", vec![click(b)]),
            _ if has(nodes, "textbox", "Password") => {
                to("erp_login", vec![nav(&format!("{}/login", t.erp))])
            }
            _ if has(nodes, "heading", "Server Error") || n % 5 == 4 => stay(
                phase,
                n,
                vec![nav(&format!("{}/app/purchase-order/{}", t.erp, t.po))],
            ),
            _ => stay(phase, n, vec![wait()]),
        },
        "erp_pick" => match find(nodes, "menuitem", "Purchase Invoice") {
            Some((_, m)) => to("erp_fill", vec![click(m)]),
            None if n >= 2 => to("erp_create", vec![key("Escape")]),
            None => stay(phase, n, vec![wait()]),
        },
        // Type, then read back on the next tree; move on only once both
        // fields read as the invoice says. A saved draft comes back here
        // from erp_saved when it differs.
        "erp_fill" => {
            let no = find(nodes, "textbox", "Supplier Invoice No");
            let date = find(nodes, "textbox", "Supplier Invoice Date");
            let form = page.starts_with("New Purchase Invoice") || page.contains("ACC-PINV-");
            match (no, date) {
                (Some((_, no)), Some((_, date))) if form => {
                    let mut a = Vec::new();
                    set(&mut a, no, &t.invoice_no);
                    let want = erp_date(&t.date);
                    if date.value != want {
                        set(&mut a, date, &want);
                        a.push(key("Escape"));
                    }
                    if a.is_empty() {
                        return self::phase(t, "erp_tax", 0, results, nodes);
                    }
                    stay(phase, n, a)
                }
                _ => stay(phase, n, vec![wait()]),
            }
        }
        "erp_tax" => {
            let Some((_, tmpl)) = find(nodes, "combobox", "Purchase Taxes and Charges Template")
            else {
                return stay(phase, n, vec![wait()]);
            };
            let rows = tax_rows(nodes);
            if !same_template(&tmpl.value, &t.tax_template) {
                let mut a = vec![click(tmpl), key("Control+a"), key("Backspace")];
                if t.tax_template.is_empty() {
                    a.push(key("Escape"));
                    return stay(phase, n, a);
                }
                a.push(json!({ "tool": "type", "args": { "text": t.tax_template } }));
                return to("erp_tax_pick", a);
            }
            match rows.first() {
                Some(r) if t.tax_template.is_empty() => to("erp_taxdel", vec![click(r)]),
                _ => to("erp_saved", vec![key("Control+s")]),
            }
        }
        "erp_tax_pick" => {
            let opt = nodes
                .iter()
                .find(|x| x.role == "option" && x.id.is_some() && x.name.contains(&t.tax_template));
            match opt {
                Some(o) => to("erp_tax", vec![click(o)]),
                None => stay(phase, n, vec![wait()]),
            }
        }
        "erp_taxdel" => {
            match find(nodes, "button", "Delete row").or_else(|| find(nodes, "button", "Delete")) {
                Some((_, b)) => to("erp_tax", vec![click(b)]),
                None => stay(phase, n, vec![wait()]),
            }
        }
        // Saved: check the draft against the invoice before Submit.
        "erp_saved" => match find(nodes, "button", "Submit") {
            Some((_, b)) if !page.starts_with("New Purchase Invoice") => {
                match draft_mismatch(t, nodes) {
                    None => to("erp_confirm", vec![click(b)]),
                    Some(_) => Reply {
                        fixes: 1,
                        ..to("erp_fill", vec![wait()])
                    },
                }
            }
            // Past midnight in the site's timezone, ERPNext asks before it
            // moves an unsaved draft's posting date to today. Escape answers
            // No, so the save would never go through.
            _ if nodes.iter().any(|x| x.name.starts_with(POSTING_DATE_MOVES)) => {
                match in_dialog(nodes, "button", "Yes") {
                    Some(b) => stay(phase, n, vec![click(b)]),
                    None => stay(phase, n, vec![wait()]),
                }
            }
            // A save that failed (validation, a lock wait) leaves a dialog
            // up and the form unsaved: close it and save again.
            _ if n % 3 == 2 => stay(phase, n, vec![key("Escape"), key("Control+s")]),
            _ => stay(phase, n, vec![wait()]),
        },
        "erp_confirm" => match find(nodes, "button", "Yes") {
            Some((_, b)) => to("erp_done", vec![click(b)]),
            None if n >= 2 => to("erp_saved", vec![wait()]),
            None => stay(phase, n, vec![wait()]),
        },
        "erp_done" => {
            if !has(nodes, "button", "Submit") && !has(nodes, "button", "Yes") {
                self::phase(t, "erp_attach", 0, results, nodes)
            } else if let Some((_, b)) = find(nodes, "button", "Yes") {
                stay(phase, n, vec![click(b)])
            } else if n >= 3 {
                to("erp_saved", vec![wait()])
            } else {
                stay(phase, n, vec![wait()])
            }
        }
        // Attach the downloaded PDF from the form sidebar:
        // "Add attachment" opens the Upload dialog; its file input takes
        // the file; the dialog's own Upload button sends it.
        "erp_attach" => match find(nodes, "button", "Add attachment") {
            Some((_, b)) => to("erp_upload", vec![click(b)]),
            None => stay(phase, n, vec![wait()]),
        },
        "erp_upload" => match in_dialog(nodes, "button", "My Device") {
            Some(_) => to(
                "erp_send",
                vec![json!({ "tool": "upload", "args": {
                    "path": pdf_name(t), "selector": ".modal.show input[type=file]" } })],
            ),
            None if n % 5 == 4 => reattach(),
            None => stay(phase, n, vec![wait()]),
        },
        "erp_send" => {
            let named = |x: &&Node| x.name.contains(&pdf_name(t));
            match in_dialog(nodes, "button", "Upload") {
                Some(b) if nodes.iter().any(|x| named(&x)) => to("erp_attached", vec![click(b)]),
                _ if results.contains("- upload") && results.contains(": error:") => reattach(),
                _ if n >= 4 => reattach(),
                _ => stay(phase, n, vec![wait()]),
            }
        }
        "erp_attached" => {
            let open = nodes.iter().any(|x| x.role == "dialog");
            let listed = nodes
                .iter()
                .any(|x| x.role == "link" && x.name.contains(&pdf_name(t)));
            match (open, listed) {
                (false, true) => Reply {
                    done: true,
                    ..to("done", vec![])
                },
                (true, _) if n >= 6 => reattach(),
                _ => stay(phase, n, vec![wait()]),
            }
        }
        other => Reply {
            done: true,
            phase: format!("unknown_{other}"),
            ..to("", vec![])
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::parse;

    fn truth() -> Truth {
        Truth {
            file: "inv-0001".into(),
            invoice_no: "INV-00108".into(),
            date: "2026-08-13".into(),
            po: "PUR-ORD-2026-00002".into(),
            tax_template: String::new(),
            lines: vec![("DEMO-ITM-019".into(), 10.0), ("DEMO-ITM-004".into(), 12.0)],
            total: 8408.44,
            portal_login: "http://p/alderbrook/login".into(),
            portal_user: "u".into(),
            portal_password: "pw".into(),
            erp: "http://e".into(),
            erp_user: "Administrator".into(),
            erp_password: "admin".into(),
        }
    }

    fn st(phase: &str, n: u32) -> State {
        State {
            phase: phase.into(),
            n,
            reloads: 0,
            fixes: 0,
        }
    }

    const DRAFT: &str = "[1] RootWebArea \"Alderbrook Office Supply - ACC-PINV-2026-00043\"
  [2] button \"Submit\"
  [3] textbox \"Supplier Invoice No\" value=\"INV-00108\"
  [4] textbox \"Supplier Invoice Date\" value=\"08-13-2026\"
  [5] StaticText \"Accepted Qty\"
  [6] checkbox
  [7] StaticText \"1\"
  [8] link \"DEMO-ITM-004: Stretch Wrap Film, 18 in\"
  [9] StaticText \"12\"
  [10] checkbox
  [11] StaticText \"2\"
  [12] link \"DEMO-ITM-019: Disposable Coffee Cups\"
  [13] StaticText \"10\"
  [14] button \"Add row\"
  [15] StaticText \"Grand Total (USD)\"
  [16] StaticText \"$ 8,408.44\"";

    #[test]
    fn state_round_trips_through_the_note() {
        let r = Reply {
            reloads: 2,
            fixes: 1,
            ..to("erp_fill", vec![])
        };
        let note = r.to_json()["note"].as_str().unwrap().to_owned();
        assert_eq!(
            state_of(&note),
            State {
                phase: "erp_fill".into(),
                n: 0,
                reloads: 2,
                fixes: 1
            }
        );
        assert_eq!(state_of(""), st("start", 0));
        assert_eq!(state_of("phase=erp_po;n=3").n, 3);
    }

    #[test]
    fn fills_then_reads_back_before_moving_on() {
        let empty = parse(
            "[1] RootWebArea \"New Purchase Invoice - new-x\"\n[5] textbox \"Supplier Invoice No\"\n[6] textbox \"Supplier Invoice Date\"",
        );
        let r = next(&truth(), &st("erp_fill", 0), "", &empty);
        assert_eq!(r.phase, "erp_fill");
        assert_eq!(
            r.actions[0]["args"],
            json!({ "node_id": 5, "text": "INV-00108" })
        );
        assert_eq!(r.actions[1]["args"]["text"], "08-13-2026");
        let wrong = parse(
            "[1] RootWebArea \"New Purchase Invoice - new-x\"\n[5] textbox \"Supplier Invoice No\" value=\"INV-0010\"\n[6] textbox \"Supplier Invoice Date\" value=\"08-13-2026\"",
        );
        let r = next(&truth(), &st("erp_fill", 1), "", &wrong);
        assert_eq!(r.actions[1]["args"]["key"], "Control+a");
        let right = parse(
            "[1] RootWebArea \"New Purchase Invoice - new-x\"\n[5] textbox \"Supplier Invoice No\" value=\"INV-00108\"\n[6] textbox \"Supplier Invoice Date\" value=\"08-13-2026\"\n[9] combobox \"Purchase Taxes and Charges Template\"",
        );
        assert_eq!(
            next(&truth(), &st("erp_fill", 2), "", &right).phase,
            "erp_saved"
        );
    }

    #[test]
    fn confirms_the_posting_date_move_after_midnight() {
        let tree = parse(
            "[1] RootWebArea \"New Purchase Invoice - new-x\"\n  [2] dialog focusable focused modal\n    [3] heading \"Confirm\"\n    [4] paragraph\n      [5] StaticText \"Posting Date will change to today's date as Edit Posting Date and Time is unchecked. Are you sure want to proceed?\"\n    [6] button \"No\" focusable\n    [7] button \"Yes\" focusable",
        );
        let r = next(&truth(), &st("erp_saved", 2), "", &tree);
        assert_eq!(
            (r.phase.as_str(), r.actions[0]["args"]["node_id"].as_i64()),
            ("erp_saved", Some(7))
        );
    }

    #[test]
    fn checks_the_draft_before_submit() {
        let t = truth();
        let r = next(&t, &st("erp_saved", 0), "", &parse(DRAFT));
        assert_eq!(
            (r.phase.as_str(), r.actions[0]["args"]["node_id"].as_i64()),
            ("erp_confirm", Some(2))
        );
        let off = DRAFT.replace("$ 8,408.44", "$ 8,000.00");
        assert_eq!(
            draft_mismatch(&t, &parse(&off)).as_deref(),
            Some("total 8000")
        );
        let r = next(
            &t,
            &State {
                fixes: 1,
                ..st("erp_saved", 0)
            },
            "",
            &parse(&off),
        );
        assert_eq!((r.phase.as_str(), r.fixes), ("erp_fill", 2));
        let r = next(
            &t,
            &State {
                fixes: 3,
                ..st("erp_saved", 0)
            },
            "",
            &parse(&off),
        );
        assert_eq!((r.phase.as_str(), r.done), ("stuck_erp_verify", true));
        let short = DRAFT.replace("  [10] checkbox\n  [11] StaticText \"2\"\n  [12] link \"DEMO-ITM-019: Disposable Coffee Cups\"\n  [13] StaticText \"10\"\n", "");
        assert_eq!(
            draft_mismatch(&t, &parse(&short)).as_deref(),
            Some("lines 1/2")
        );
    }

    #[test]
    fn reloads_error_pages_three_times_then_lets_the_phase_decide() {
        let err = parse("[1] RootWebArea \"Login\"\n  [2] heading \"Something went wrong\"");
        let r = next(&truth(), &st("erp_login", 2), "", &err);
        assert_eq!((r.phase.as_str(), r.n, r.reloads), ("erp_login", 2, 1));
        assert_eq!(r.actions[1]["tool"], "reload");
        let r = next(
            &truth(),
            &State {
                reloads: 3,
                ..st("erp_login", 2)
            },
            "",
            &err,
        );
        assert_eq!((r.reloads, r.n), (3, 3));
        let dialog = parse(
            "[1] RootWebArea \"PI\"\n  [2] dialog\n    [3] heading \"Server Error\"\n  [4] textbox \"Email\"\n  [5] textbox \"Password\"",
        );
        assert_eq!(
            next(&truth(), &st("erp_login", 0), "", &dialog).phase,
            "erp_po"
        );
    }

    #[test]
    fn attaches_the_pdf_after_submit() {
        let t = truth();
        let submitted =
            parse("[1] RootWebArea \"Alderbrook - ACC-PINV-1\"\n  [2] button \"Add attachment\"");
        let r = next(&t, &st("erp_done", 0), "", &submitted);
        assert_eq!(
            (r.phase.as_str(), r.actions[0]["args"]["node_id"].as_i64()),
            ("erp_upload", Some(2))
        );
        let dlg = parse(
            "[1] RootWebArea \"x\"\n  [2] button \"Upload\"\n  [3] dialog\n    [4] heading \"Upload\"\n    [5] button \"My Device\"\n    [6] button \"Upload\"",
        );
        let r = next(&t, &st("erp_upload", 0), "", &dlg);
        assert_eq!(
            (r.phase.as_str(), r.actions[0]["args"]["path"].as_str()),
            ("erp_send", Some("INV-00108.pdf"))
        );
        let picked = parse(
            "[1] RootWebArea \"x\"\n  [2] button \"Upload\"\n  [3] dialog\n    [5] StaticText \"INV-00108.pdf\"\n    [6] button \"Upload\"",
        );
        let r = next(&t, &st("erp_send", 0), "", &picked);
        assert_eq!(
            (r.phase.as_str(), r.actions[0]["args"]["node_id"].as_i64()),
            ("erp_attached", Some(6))
        );
        let r = next(
            &t,
            &State {
                fixes: 3,
                ..st("erp_send", 9)
            },
            "- upload {}: error: no such file",
            &dlg,
        );
        assert_eq!((r.phase.as_str(), r.done), ("stuck_erp_attach", true));
        let slashed = Truth {
            invoice_no: "BWF/26/0115".into(),
            ..t.clone()
        };
        assert_eq!(pdf_name(&slashed), "BWF_26_0115.pdf");
        let listed = parse("[1] RootWebArea \"x\"\n  [7] link \"INV-00108.pdf\"");
        let r = next(&t, &st("erp_attached", 0), "", &listed);
        assert_eq!((r.phase.as_str(), r.done), ("done", true));
    }

    #[test]
    fn untaxed_invoice_clears_template_then_deletes_rows() {
        let t = truth();
        let with = parse(
            "[1] RootWebArea \"New Purchase Invoice\"\n[9] combobox \"Purchase Taxes and Charges Template\" value=\"US ST 6% - PDC\"\n[10] StaticText \"Purchase Taxes and Charges\"\n[11] checkbox\n[12] checkbox\n[13] button \"Add row\"",
        );
        assert_eq!(
            next(&t, &st("erp_tax", 0), "", &with).actions[0]["args"]["node_id"],
            9
        );
        let cleared = parse(
            "[9] combobox \"Purchase Taxes and Charges Template\"\n[10] StaticText \"Purchase Taxes and Charges\"\n[11] checkbox\n[12] checkbox\n[13] button \"Add row\"",
        );
        let r = next(&t, &st("erp_tax", 1), "", &cleared);
        assert_eq!(
            (r.phase.as_str(), r.actions[0]["args"]["node_id"].as_i64()),
            ("erp_taxdel", Some(12))
        );
        let empty = parse(
            "[9] combobox \"Purchase Taxes and Charges Template\"\n[10] StaticText \"Purchase Taxes and Charges\"\n[11] checkbox\n[13] button \"Add row\"",
        );
        assert_eq!(next(&t, &st("erp_tax", 2), "", &empty).phase, "erp_saved");
    }

    #[test]
    fn a_template_title_matches_its_name() {
        assert!(same_template("US ST 4%", "US ST 4% - PDC"));
        assert!(same_template("US ST 4% - PDC", "US ST 4% - PDC"));
        assert!(!same_template("US ST 4", "US ST 4% - PDC"));
        assert!(!same_template("US ST 6%", "US ST 6.25% - PDC"));
        assert!(same_template("", ""));
        assert!(!same_template("US ST 6%", ""));
    }

    #[test]
    fn retries_a_failed_erp_login_with_continue() {
        let nodes = parse(
            "[1] RootWebArea \"Login\"\n[2] StaticText \"Something went wrong.\"\n[3] textbox \"Password\" value=\"x\"\n[4] button \"Continue\"",
        );
        let r = next(&truth(), &st("erp_po", 0), "", &nodes);
        assert_eq!(
            (r.phase.as_str(), r.actions[0]["args"]["node_id"].as_i64()),
            ("erp_po", Some(4))
        );
    }

    #[test]
    fn sees_a_complete_pdf_in_download_list() {
        let r = "- download_list {}: {\"dir\":\"/d\",\"files\":[{\"name\":\"INV-00108.pdf\",\"bytes\":9,\"complete\":true}]}";
        assert!(downloaded_pdf(r));
        assert!(!downloaded_pdf("- download_list {}: {\"files\":[]}"));
    }
}
