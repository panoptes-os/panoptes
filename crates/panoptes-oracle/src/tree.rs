//! Read the desk's ax_tree text back into nodes.
//!
//! ```text
//!   [81884] textbox "Supplier Invoice No" value="INV-1" focusable @(340,-322 420x28)
//! ```
//!
//! Names and values are Rust `{:?}` strings.

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: Option<i64>,
    pub role: String,
    pub name: String,
    pub value: String,
    /// Nesting depth, from the line's indent.
    pub depth: usize,
}

/// Read one `{:?}` string starting after its opening quote. Returns the
/// text and the rest of the line after the closing quote.
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
                    // \u{XXXX}
                    let mut hex = String::new();
                    for (_, h) in it.by_ref() {
                        match h {
                            '{' => {}
                            '}' => break,
                            h => hex.push(h),
                        }
                    }
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some((_, c)) => out.push(c),
                None => {}
            },
            c => out.push(c),
        }
    }
    (out, "")
}

pub fn parse(text: &str) -> Vec<Node> {
    let mut nodes = Vec::new();
    for line in text.lines() {
        let l = line.trim_start();
        let depth = (line.len() - l.len()) / 2;
        let Some(rest) = l.strip_prefix('[') else { continue };
        let Some((id, rest)) = rest.split_once("] ") else { continue };
        let (role, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut name = String::new();
        let mut value = String::new();
        if let Some(r) = rest.strip_prefix('"') {
            (name, rest) = quoted(r);
        }
        if let Some(r) = rest.trim_start().strip_prefix("value=\"") {
            (value, _) = quoted(r);
        }
        nodes.push(Node { id: id.parse().ok(), role: role.to_owned(), name, value, depth });
    }
    nodes
}

/// Position and node of the first `role` named exactly `name`.
pub fn find<'a>(nodes: &'a [Node], role: &str, name: &str) -> Option<(usize, &'a Node)> {
    nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.role == role && n.name == name && n.id.is_some())
}

pub fn has(nodes: &[Node], role: &str, name: &str) -> bool {
    find(nodes, role, name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ids_roles_names_and_values() {
        let t = "[1] RootWebArea \"A \\\"b\\\"\" focused @(0,0 1x1)\n  [2] textbox \"Supplier Invoice No\" value=\"INV-1\" focusable\n  [-] StaticText \"*\"\n  [3] checkbox focusable";
        let n = parse(t);
        assert_eq!(n.len(), 4);
        assert_eq!(n[0].name, "A \"b\"");
        assert_eq!((n[1].id, n[1].value.as_str()), (Some(2), "INV-1"));
        assert_eq!((n[2].id, n[2].depth), (None, 1));
        assert_eq!((n[3].role.as_str(), n[3].name.as_str()), ("checkbox", ""));
        assert_eq!(find(&n, "textbox", "Supplier Invoice No").map(|x| x.0), Some(1));
    }
}
