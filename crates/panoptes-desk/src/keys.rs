//! Key names to `Input.dispatchKeyEvent` parameters.

use serde_json::{Value, json};

/// One key press: the keyDown and keyUp events for `spec`, such as `Enter`,
/// `Tab`, `a`, or `Control+a`.
pub fn press(spec: &str) -> Result<[Value; 2], String> {
    let mut parts: Vec<&str> = spec.split('+').collect();
    let key = parts.pop().filter(|k| !k.is_empty()).ok_or("empty key")?;
    let mut modifiers = 0;
    for m in parts {
        modifiers |= match m.to_ascii_lowercase().as_str() {
            "alt" => 1,
            "control" | "ctrl" => 2,
            "meta" | "cmd" => 4,
            "shift" => 8,
            other => return Err(format!("unknown modifier {other}")),
        };
    }
    let (key, code, vk, text): (String, String, u32, Option<String>) = match key {
        "Enter" => ("Enter".into(), "Enter".into(), 13, Some("\r".into())),
        "Tab" => ("Tab".into(), "Tab".into(), 9, None),
        "Escape" => ("Escape".into(), "Escape".into(), 27, None),
        "Backspace" => ("Backspace".into(), "Backspace".into(), 8, None),
        "Delete" => ("Delete".into(), "Delete".into(), 46, None),
        "Space" | " " => (" ".into(), "Space".into(), 32, Some(" ".into())),
        "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => {
            let vk = match key {
                "ArrowLeft" => 37,
                "ArrowUp" => 38,
                "ArrowRight" => 39,
                _ => 40,
            };
            (key.into(), key.into(), vk, None)
        }
        "Home" => ("Home".into(), "Home".into(), 36, None),
        "End" => ("End".into(), "End".into(), 35, None),
        "PageUp" => ("PageUp".into(), "PageUp".into(), 33, None),
        "PageDown" => ("PageDown".into(), "PageDown".into(), 34, None),
        k if k.chars().count() == 1 => {
            let c = k.chars().next().unwrap_or(' ');
            let up = c.to_ascii_uppercase();
            let code = if c.is_ascii_alphabetic() {
                format!("Key{up}")
            } else if c.is_ascii_digit() {
                format!("Digit{c}")
            } else {
                String::new()
            };
            (k.into(), code, up as u32, Some(k.into()))
        }
        other => return Err(format!("unknown key {other}")),
    };
    // A chord with Control, Alt, or Meta types no character.
    let text = text.filter(|_| modifiers & 0b111 == 0);
    let mut down = json!({
        "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
        "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers,
    });
    if let Some(t) = text {
        down["text"] = json!(t);
    }
    let up = json!({
        "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers,
    });
    Ok([down, up])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_types_return() {
        let [down, up] = press("Enter").unwrap();
        assert_eq!(down["type"], "keyDown");
        assert_eq!(down["text"], "\r");
        assert_eq!(up["type"], "keyUp");
    }

    #[test]
    fn chords_type_nothing() {
        let [down, _] = press("Control+a").unwrap();
        assert_eq!(down["type"], "rawKeyDown");
        assert_eq!(down["modifiers"], 2);
        assert!(down.get("text").is_none());
        assert_eq!(down["code"], "KeyA");
    }

    #[test]
    fn unknown_keys_fail() {
        assert!(press("Hyper+x").is_err());
        assert!(press("F13").is_err());
    }
}
