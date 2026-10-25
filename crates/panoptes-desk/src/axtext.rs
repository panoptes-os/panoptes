//! A compact text form of the accessibility tree for a model's prompt.
//!
//! ```text
//! [42] textbox "Supplier" value="Acme" focused @(412,188 220x32)
//!   [57] option "Acme Supply"
//! ```
//!
//! The bracketed number is the node's backend DOM id, which `click` and
//! `type` accept as `node_id`. Roles that only carry layout (generic, none,
//! inline text boxes) are skipped and their children lifted one level.

use panoptes_cdp::cmd::AxNode;
use std::collections::HashMap;
use std::fmt::Write;

const SKIP: &[&str] = &["none", "generic", "InlineTextBox", "LineBreak", "ignored", ""];

pub struct Rendered {
    pub text: String,
    pub nodes: usize,
    pub truncated: bool,
}

pub fn render(nodes: &[AxNode], boxes: &HashMap<i64, [f64; 4]>, max_bytes: usize) -> Rendered {
    let by_id: HashMap<&str, &AxNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let root = nodes.iter().find(|n| n.parent.is_none());
    let mut out = Rendered {
        text: String::new(),
        nodes: 0,
        truncated: false,
    };
    if let Some(root) = root {
        walk(root, 0, &by_id, boxes, max_bytes, &mut out);
    }
    if out.truncated {
        out.text.push_str("... truncated\n");
    }
    out
}

fn walk(
    n: &AxNode,
    depth: usize,
    by_id: &HashMap<&str, &AxNode>,
    boxes: &HashMap<i64, [f64; 4]>,
    max: usize,
    out: &mut Rendered,
) {
    if out.truncated {
        return;
    }
    let shown = !n.ignored
        && !SKIP.contains(&n.role.as_str())
        && !(n.role == "StaticText" && n.name.trim().is_empty())
        && !redundant_text(n, by_id);
    let next = if shown { depth + 1 } else { depth };
    if shown {
        let mut line = String::new();
        let _ = write!(line, "{:indent$}", "", indent = depth * 2);
        match n.backend_id {
            Some(b) => {
                let _ = write!(line, "[{b}] ");
            }
            None => line.push_str("[-] "),
        }
        line.push_str(&n.role);
        if !n.name.is_empty() {
            let _ = write!(line, " {:?}", clip(&n.name, 120));
        } else if let Some(label) = label_for(n, by_id) {
            let _ = write!(line, " {:?}", clip(&label, 120));
        }
        if !n.value.is_empty() {
            let _ = write!(line, " value={:?}", clip(&n.value, 120));
        }
        for s in &n.states {
            let _ = write!(line, " {s}");
        }
        if let Some(b) = n.backend_id.and_then(|b| boxes.get(&b)) {
            let _ = write!(line, " @({:.0},{:.0} {:.0}x{:.0})", b[0], b[1], b[2], b[3]);
        }
        line.push('\n');
        if out.text.len() + line.len() > max {
            out.truncated = true;
            return;
        }
        out.text.push_str(&line);
        out.nodes += 1;
    }
    for c in &n.children {
        if let Some(child) = by_id.get(c.as_str()) {
            walk(child, next, by_id, boxes, max, out);
        }
    }
}

const FIELDS: &[&str] = &["combobox", "textbox", "searchbox", "spinbutton", "listbox"];

/// A form field with no accessible name, named from the nearest label
/// before it. Frappe, for one, draws a field's label as a sibling
/// `LabelText` instead of a `<label for>`:
///
/// ```text
/// LabelText              → combobox "Supplier"
///   StaticText "Supplier"
///   StaticText "*"
/// combobox
/// ```
///
/// The search climbs up to six ancestors (Frappe nests a link field five
/// deep below its label's row) and stops at an earlier field,
/// so a field never takes the label of the one before it.
fn label_for(n: &AxNode, by_id: &HashMap<&str, &AxNode>) -> Option<String> {
    if !FIELDS.contains(&n.role.as_str()) {
        return None;
    }
    let mut cur = n;
    for _ in 0..6 {
        let parent = by_id.get(cur.parent.as_deref()?)?;
        let at = parent.children.iter().position(|c| *c == cur.id)?;
        for sib in parent.children[..at].iter().rev().filter_map(|c| by_id.get(c.as_str())) {
            if let Some(l) = find_label(sib, by_id, 3) {
                return Some(l);
            }
            if has_field(sib, by_id, 3) {
                return None;
            }
        }
        cur = parent;
    }
    None
}

fn find_label(n: &AxNode, by_id: &HashMap<&str, &AxNode>, depth: usize) -> Option<String> {
    if n.role == "LabelText" {
        let mut parts = Vec::new();
        texts(n, by_id, &mut parts);
        let s = parts.join(" ");
        return (!s.is_empty()).then_some(s);
    }
    if depth == 0 || FIELDS.contains(&n.role.as_str()) {
        return None;
    }
    n.children
        .iter()
        .rev()
        .filter_map(|c| by_id.get(c.as_str()))
        .find_map(|c| find_label(c, by_id, depth - 1))
}

fn has_field(n: &AxNode, by_id: &HashMap<&str, &AxNode>, depth: usize) -> bool {
    FIELDS.contains(&n.role.as_str())
        || depth > 0
            && n.children
                .iter()
                .filter_map(|c| by_id.get(c.as_str()))
                .any(|c| has_field(c, by_id, depth - 1))
}

fn texts(n: &AxNode, by_id: &HashMap<&str, &AxNode>, out: &mut Vec<String>) {
    if n.role == "StaticText" {
        let t = n.name.trim();
        if !t.is_empty() && t != "*" {
            out.push(t.to_owned());
        }
        return;
    }
    for c in n.children.iter().filter_map(|c| by_id.get(c.as_str())) {
        texts(c, by_id, out);
    }
}

/// Static text whose parent already shows the same name adds nothing.
fn redundant_text(n: &AxNode, by_id: &HashMap<&str, &AxNode>) -> bool {
    n.role == "StaticText"
        && n.parent
            .as_deref()
            .and_then(|p| by_id.get(p))
            .is_some_and(|p| p.name == n.name)
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, children: &[&str], role: &str, name: &str) -> AxNode {
        AxNode {
            id: id.into(),
            parent: parent.map(Into::into),
            children: children.iter().map(|c| (*c).into()).collect(),
            ignored: false,
            role: role.into(),
            name: name.into(),
            value: String::new(),
            backend_id: id.parse().ok(),
            states: vec![],
        }
    }

    #[test]
    fn skips_layout_roles_and_lifts_children() {
        let nodes = vec![
            node("1", None, &["2"], "RootWebArea", "Portal"),
            node("2", Some("1"), &["3", "4"], "generic", ""),
            node("3", Some("2"), &["5"], "button", "Sign in"),
            node("5", Some("3"), &[], "StaticText", "Sign in"),
            node("4", Some("2"), &[], "textbox", "User"),
        ];
        let boxes = HashMap::from([(3, [10.0, 20.0, 80.0, 30.0])]);
        let r = render(&nodes, &boxes, 10_000);
        assert_eq!(
            r.text,
            "[1] RootWebArea \"Portal\"\n  [3] button \"Sign in\" @(10,20 80x30)\n  [4] textbox \"User\"\n"
        );
        assert_eq!(r.nodes, 3);
    }

    #[test]
    fn names_an_unnamed_field_from_the_label_before_it() {
        let nodes = vec![
            node("1", None, &["2"], "RootWebArea", "Form"),
            node("2", Some("1"), &["3", "6", "7", "8"], "form", ""),
            node("3", Some("2"), &["4", "5"], "LabelText", ""),
            node("4", Some("3"), &[], "StaticText", "Supplier"),
            node("5", Some("3"), &[], "StaticText", "*"),
            node("6", Some("2"), &[], "combobox", ""),
            node("7", Some("2"), &[], "textbox", ""),
            node("8", Some("2"), &[], "textbox", "Named"),
        ];
        let r = render(&nodes, &HashMap::new(), 10_000);
        assert!(r.text.contains("[6] combobox \"Supplier\"\n"), "{}", r.text);
        // The second field must not borrow the first field's label.
        assert!(r.text.contains("[7] textbox\n"), "{}", r.text);
        assert!(r.text.contains("[8] textbox \"Named\"\n"), "{}", r.text);
    }

    #[test]
    fn truncates_at_the_byte_limit() {
        let nodes = vec![
            node("1", None, &["2"], "RootWebArea", "Portal"),
            node("2", Some("1"), &[], "button", "Sign in"),
        ];
        let r = render(&nodes, &HashMap::new(), 25);
        assert!(r.truncated);
        assert!(r.text.ends_with("... truncated\n"));
    }
}
