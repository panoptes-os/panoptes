//! The desk's keyboard: a keymap the desk writes itself, so it knows which
//! keycode and modifiers produce every key it presses.
//!
//! The base layout is US QWERTY on evdev keycodes, as a real keyboard
//! sends, so X11 apps and anything else that reads keycodes see what they
//! expect. A character the layout lacks, such as é or 中, goes on a spare
//! keycode; typing text that needs new ones uploads the keymap again first.
//!
//! wlroots drops the desk's whole connection when a keymap fails to compile,
//! so every keysym written here comes from the tables below or is a Unicode
//! keysym (`U00E9`) for a printable character.
//!
//! The compositor does not derive modifier state from virtual key events,
//! so pressing a chord sends the new modifier mask after each modifier key:
//!
//! ```text
//! ctrl+shift+t   Control_L down, mods=Control
//!                Shift_L down,   mods=Control|Shift
//!                t down, t up
//!                Shift_L up,     mods=Control
//!                Control_L up,   mods=0
//! ```

use std::fmt::Write;

/// Masks of the real modifiers; xkbcommon numbers the eight core modifiers
/// first, in this order.
const SHIFT: u32 = 1 << 0;
const CONTROL: u32 = 1 << 2;
const MOD1: u32 = 1 << 3;
const MOD4: u32 = 1 << 6;

/// The modifiers a chord can hold, outermost first: mask, evdev code, names.
const MODIFIERS: &[(u32, u32, &[&str])] = &[
    (CONTROL, 29, &["ctrl", "control"]),
    (MOD1, 56, &["alt"]),
    (SHIFT, 42, &["shift"]),
    (MOD4, 125, &["super", "meta", "win", "cmd", "logo"]),
];

/// US keys that type a character: evdev code, plain, with Shift.
#[rustfmt::skip]
const PRINTABLE: &[(u32, char, char)] = &[
    (41, '`', '~'), (2, '1', '!'), (3, '2', '@'), (4, '3', '#'), (5, '4', '$'),
    (6, '5', '%'), (7, '6', '^'), (8, '7', '&'), (9, '8', '*'), (10, '9', '('),
    (11, '0', ')'), (12, '-', '_'), (13, '=', '+'),
    (16, 'q', 'Q'), (17, 'w', 'W'), (18, 'e', 'E'), (19, 'r', 'R'), (20, 't', 'T'),
    (21, 'y', 'Y'), (22, 'u', 'U'), (23, 'i', 'I'), (24, 'o', 'O'), (25, 'p', 'P'),
    (26, '[', '{'), (27, ']', '}'), (43, '\\', '|'),
    (30, 'a', 'A'), (31, 's', 'S'), (32, 'd', 'D'), (33, 'f', 'F'), (34, 'g', 'G'),
    (35, 'h', 'H'), (36, 'j', 'J'), (37, 'k', 'K'), (38, 'l', 'L'), (39, ';', ':'),
    (40, '\'', '"'),
    (44, 'z', 'Z'), (45, 'x', 'X'), (46, 'c', 'C'), (47, 'v', 'V'), (48, 'b', 'B'),
    (49, 'n', 'N'), (50, 'm', 'M'), (51, ',', '<'), (52, '.', '>'), (53, '/', '?'),
];

/// Keys without a character: the names agents use for them (xkb keysym
/// names and common aliases, lowercase), evdev code, and keysyms.
#[rustfmt::skip]
const NAMED: &[(&[&str], u32, &str)] = &[
    (&["return", "enter"], 28, "Return"),
    (&["escape", "esc"], 1, "Escape"),
    (&["backspace"], 14, "BackSpace"),
    (&["tab"], 15, "Tab, ISO_Left_Tab"),
    (&["space"], 57, "space"),
    (&["delete", "del"], 111, "Delete"),
    (&["insert", "ins"], 110, "Insert"),
    (&["home"], 102, "Home"),
    (&["end"], 107, "End"),
    (&["page_up", "pageup", "prior"], 104, "Prior"),
    (&["page_down", "pagedown", "next"], 109, "Next"),
    (&["left", "arrowleft"], 105, "Left"),
    (&["right", "arrowright"], 106, "Right"),
    (&["up", "arrowup"], 103, "Up"),
    (&["down", "arrowdown"], 108, "Down"),
    (&["menu", "contextmenu"], 127, "Menu"),
    (&["print", "printscreen"], 99, "Print"),
    (&["pause"], 119, "Pause"),
    (&["caps_lock", "capslock"], 58, "Caps_Lock"),
    (&["num_lock", "numlock"], 69, "Num_Lock"),
    (&["scroll_lock", "scrolllock"], 70, "Scroll_Lock"),
    (&["control_l", "ctrl_l"], 29, "Control_L"),
    (&["control_r", "ctrl_r"], 97, "Control_R"),
    (&["alt_l"], 56, "Alt_L"),
    (&["alt_r"], 100, "Alt_R"),
    (&["shift_l"], 42, "Shift_L"),
    (&["shift_r"], 54, "Shift_R"),
    (&["super_l"], 125, "Super_L"),
    (&["super_r"], 126, "Super_R"),
    (&["kp_enter"], 96, "KP_Enter"),
    (&["kp_add"], 78, "KP_Add"),
    (&["kp_subtract"], 74, "KP_Subtract"),
    (&["kp_multiply"], 55, "KP_Multiply"),
    (&["kp_divide"], 98, "KP_Divide"),
    (&["kp_decimal"], 83, "KP_Decimal"),
    (&["kp_0"], 82, "KP_0"), (&["kp_1"], 79, "KP_1"), (&["kp_2"], 80, "KP_2"),
    (&["kp_3"], 81, "KP_3"), (&["kp_4"], 75, "KP_4"), (&["kp_5"], 76, "KP_5"),
    (&["kp_6"], 77, "KP_6"), (&["kp_7"], 71, "KP_7"), (&["kp_8"], 72, "KP_8"),
    (&["kp_9"], 73, "KP_9"),
];

/// xkb keysym names of ASCII punctuation, for chords like `ctrl+plus`.
#[rustfmt::skip]
const CHAR_NAMES: &[(&str, char)] = &[
    ("plus", '+'), ("minus", '-'), ("equal", '='), ("comma", ','), ("period", '.'),
    ("slash", '/'), ("backslash", '\\'), ("semicolon", ';'), ("colon", ':'),
    ("apostrophe", '\''), ("quotedbl", '"'), ("grave", '`'), ("asciitilde", '~'),
    ("bracketleft", '['), ("bracketright", ']'), ("braceleft", '{'), ("braceright", '}'),
    ("less", '<'), ("greater", '>'), ("question", '?'), ("exclam", '!'), ("at", '@'),
    ("numbersign", '#'), ("dollar", '$'), ("percent", '%'), ("asciicircum", '^'),
    ("ampersand", '&'), ("asterisk", '*'), ("parenleft", '('), ("parenright", ')'),
    ("underscore", '_'), ("bar", '|'),
];

/// Evdev code of function key `n`, 1 to 24.
fn function_key(n: u32) -> u32 {
    match n {
        1..=10 => 58 + n,
        11 | 12 => 76 + n,
        _ => 170 + n,
    }
}

/// The mask and evdev code of a modifier, by any of its names.
fn modifier(name: &str) -> Option<(u32, u32)> {
    let &(mask, code, _) = MODIFIERS
        .iter()
        .find(|(_, _, names)| names.contains(&name))?;
    Some((mask, code))
}

/// Evdev codes the base layout leaves free. X11 keycodes, evdev plus 8,
/// stop at 255.
const SPARE_FIRST: u32 = 195;
const SPARE_KEYS: usize = 53;
const _: () = assert!(SPARE_FIRST + SPARE_KEYS as u32 - 1 + 8 <= 255);

/// One key press: an evdev code and the modifiers held around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    code: u32,
    mods: u32,
}

impl Stroke {
    fn plain(code: u32) -> Stroke {
        Stroke { code, mods: 0 }
    }
}

/// The stroke that types `c` on the base layout.
fn base_stroke(c: char) -> Option<Stroke> {
    match c {
        '\n' => return Some(Stroke::plain(28)),
        '\t' => return Some(Stroke::plain(15)),
        ' ' => return Some(Stroke::plain(57)),
        _ => {}
    }
    for &(code, lo, hi) in PRINTABLE {
        if c == lo {
            return Some(Stroke::plain(code));
        }
        if c == hi {
            return Some(Stroke { code, mods: SHIFT });
        }
    }
    None
}

/// What the desk sends the virtual keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Key { code: u32, pressed: bool },
    Modifiers(u32),
}

/// One step of typing: replace the keymap, or press keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Keymap(String),
    Keys(Vec<KeyEvent>),
}

/// The keymap the compositor holds: the base layout plus the characters
/// bound to spare codes, in code order.
#[derive(Debug, Default)]
pub struct Keymap {
    spare: Vec<char>,
}

impl Keymap {
    /// The stroke that types `c` with this keymap.
    fn stroke(&self, c: char) -> Option<Stroke> {
        base_stroke(c).or_else(|| {
            let i = self.spare.iter().position(|&s| s == c)?;
            Some(Stroke::plain(SPARE_FIRST + i as u32))
        })
    }

    /// Steps that type `text`, rebinding spare codes where it needs
    /// characters the current keymap lacks.
    pub fn type_text(&mut self, text: &str) -> Result<Vec<Step>, String> {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let chars: Vec<char> = text.chars().collect();
        if let Some(c) = chars
            .iter()
            .find(|&&c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(format!("cannot type control character U+{:04X}", *c as u32));
        }
        let mut steps = Vec::new();
        let mut keys = Vec::new();
        for (i, &c) in chars.iter().enumerate() {
            if self.stroke(c).is_none() {
                // Bind the characters ahead that the base layout lacks, as
                // many as fit.
                self.spare.clear();
                for &d in &chars[i..] {
                    if self.spare.len() == SPARE_KEYS {
                        break;
                    }
                    if base_stroke(d).is_none() && !self.spare.contains(&d) {
                        self.spare.push(d);
                    }
                }
                if !keys.is_empty() {
                    steps.push(Step::Keys(std::mem::take(&mut keys)));
                }
                steps.push(Step::Keymap(self.text()));
            }
            let stroke = self.stroke(c).expect("bound above");
            keys.extend(press(stroke));
        }
        if !keys.is_empty() {
            steps.push(Step::Keys(keys));
        }
        Ok(steps)
    }

    /// Key events for a chord such as `Return`, `ctrl+shift+t`, or `alt+F4`.
    pub fn chord(&self, spec: &str) -> Result<Vec<KeyEvent>, String> {
        let spec = spec.trim();
        // A lone character is that key, and a trailing "++" names the plus
        // key itself.
        let (mods, key) = if spec.chars().count() == 1 {
            ("", spec)
        } else if let Some(m) = spec.strip_suffix("++") {
            (m, "+")
        } else {
            spec.rsplit_once('+').unwrap_or(("", spec))
        };
        let mut held = 0;
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            let (mask, _) = modifier(&m.to_ascii_lowercase())
                .ok_or_else(|| format!("unknown modifier {m:?} in {spec:?}"))?;
            held |= mask;
        }
        let mut stroke = self.key(key).ok_or_else(|| {
            format!(
                "unknown key {key:?}: use a character, an xkb keysym name such as \
                 Return or Page_Down, F1 to F24, or a modifier"
            )
        })?;
        stroke.mods |= held;
        Ok(press(stroke))
    }

    /// The stroke for one key name in a chord.
    fn key(&self, name: &str) -> Option<Stroke> {
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return self.stroke(c);
        }
        let lower = name.to_ascii_lowercase();
        let named = NAMED
            .iter()
            .find(|(names, _, _)| names.contains(&lower.as_str()));
        if let Some(&(_, code, _)) = named {
            return Some(Stroke::plain(code));
        }
        if let Some((_, code)) = modifier(&lower) {
            return Some(Stroke::plain(code));
        }
        if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse().ok()) {
            if (1..=24).contains(&n) {
                return Some(Stroke::plain(function_key(n)));
            }
        }
        let &(_, c) = CHAR_NAMES.iter().find(|(n, _)| *n == lower)?;
        self.stroke(c)
    }

    /// The keymap as xkb text.
    pub fn text(&self) -> String {
        let mut codes = String::new();
        let mut symbols = String::new();
        // Key names must fit X11's four characters, so `K` and the evdev
        // code, at most 247.
        let mut key = |code: u32, syms: &str| {
            let _ = writeln!(codes, "  <K{code}> = {};", code + 8);
            let _ = writeln!(symbols, "  key <K{code}> {{ [ {syms} ] }};");
        };
        for &(code, lo, hi) in PRINTABLE {
            key(code, &format!("{}, {}", keysym(lo), keysym(hi)));
        }
        for &(_, code, syms) in NAMED {
            key(code, syms);
        }
        for n in 1..=24 {
            key(function_key(n), &format!("F{n}"));
        }
        for (i, &c) in self.spare.iter().enumerate() {
            key(SPARE_FIRST + i as u32, &keysym(c));
        }
        format!(
            "xkb_keymap {{
xkb_keycodes \"panoptes\" {{
  minimum = 8;
  maximum = 255;
{codes}}};
xkb_types \"panoptes\" {{ include \"complete\" }};
xkb_compatibility \"panoptes\" {{ include \"complete\" }};
xkb_symbols \"panoptes\" {{
{symbols}  modifier_map Shift {{ <K42>, <K54> }};
  modifier_map Lock {{ <K58> }};
  modifier_map Control {{ <K29>, <K97> }};
  modifier_map Mod1 {{ <K56>, <K100> }};
  modifier_map Mod2 {{ <K69> }};
  modifier_map Mod4 {{ <K125>, <K126> }};
}};
}};
"
        )
    }
}

/// The Unicode keysym name for a printable character.
fn keysym(c: char) -> String {
    format!("U{:04X}", c as u32)
}

/// Key events for one stroke: each held modifier pressed with its mask
/// sent, the key pressed and released, then the modifiers released in
/// reverse. A modifier key pressed alone holds its own mask while down.
fn press(stroke: Stroke) -> Vec<KeyEvent> {
    let held: Vec<(u32, u32)> = MODIFIERS
        .iter()
        .filter(|(mask, code, _)| stroke.mods & mask != 0 && *code != stroke.code)
        .map(|&(mask, code, _)| (mask, code))
        .collect();
    let own = MODIFIERS
        .iter()
        .find(|(_, code, _)| *code == stroke.code)
        .map_or(0, |(mask, _, _)| *mask);
    let mut events = Vec::new();
    let mut mask = 0;
    for &(m, code) in &held {
        events.push(KeyEvent::Key {
            code,
            pressed: true,
        });
        mask |= m;
        events.push(KeyEvent::Modifiers(mask));
    }
    events.push(KeyEvent::Key {
        code: stroke.code,
        pressed: true,
    });
    if own != 0 {
        events.push(KeyEvent::Modifiers(mask | own));
    }
    events.push(KeyEvent::Key {
        code: stroke.code,
        pressed: false,
    });
    if own != 0 {
        events.push(KeyEvent::Modifiers(mask));
    }
    for &(m, code) in held.iter().rev() {
        events.push(KeyEvent::Key {
            code,
            pressed: false,
        });
        mask &= !m;
        events.push(KeyEvent::Modifiers(mask));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyEvent::{Key, Modifiers};

    fn down(code: u32) -> KeyEvent {
        Key {
            code,
            pressed: true,
        }
    }
    fn up(code: u32) -> KeyEvent {
        Key {
            code,
            pressed: false,
        }
    }

    #[test]
    fn chords_hold_modifiers_outermost_first() {
        let k = Keymap::default();
        assert_eq!(k.chord("Return").unwrap(), [down(28), up(28)]);
        assert_eq!(
            k.chord("ctrl+shift+t").unwrap(),
            [
                down(29),
                Modifiers(CONTROL),
                down(42),
                Modifiers(CONTROL | SHIFT),
                down(20),
                up(20),
                up(42),
                Modifiers(CONTROL),
                up(29),
                Modifiers(0),
            ]
        );
        // An uppercase letter or a shifted symbol brings its own Shift.
        assert_eq!(k.chord("ctrl+T").unwrap(), k.chord("ctrl+shift+t").unwrap());
        assert_eq!(k.chord("ctrl++").unwrap(), k.chord("ctrl+plus").unwrap());
        assert_eq!(k.chord("+").unwrap(), k.chord("shift+equal").unwrap());
        assert_eq!(k.chord("alt+F4").unwrap()[2], down(62));
        assert_eq!(k.chord("F12").unwrap()[0], down(88));
        assert_eq!(k.chord("F13").unwrap()[0], down(183));
        assert_eq!(
            k.chord("super").unwrap(),
            [down(125), Modifiers(MOD4), up(125), Modifiers(0)]
        );
        assert!(k.chord("hyper+a").is_err());
        assert!(k.chord("XF86Explode").is_err());
    }

    #[test]
    fn text_on_the_base_layout_needs_no_keymap() {
        let mut k = Keymap::default();
        let steps = k.type_text("Hi!\r\n").unwrap();
        let [Step::Keys(keys)] = &steps[..] else {
            panic!("{steps:?}")
        };
        assert_eq!(
            keys.iter()
                .filter(|e| matches!(e, Key { pressed: true, .. }))
                .count(),
            6
        );
        assert!(k.type_text("bell\u{7}").is_err());
    }

    #[test]
    fn other_characters_go_on_spare_keys() {
        let mut k = Keymap::default();
        let steps = k.type_text("café 中").unwrap();
        assert!(matches!(
            &steps[..],
            [Step::Keys(_), Step::Keymap(_), Step::Keys(_)]
        ));
        assert_eq!(k.spare, ['é', '中']);
        let Step::Keymap(text) = &steps[1] else {
            unreachable!()
        };
        assert!(text.contains("key <K195> { [ U00E9 ] };"));
        assert!(text.contains("key <K196> { [ U4E2D ] };"));
        // The bound keymap types them again without a new upload.
        assert!(matches!(&k.type_text("é").unwrap()[..], [Step::Keys(_)]));
    }

    #[test]
    fn long_text_rebinds_when_spare_keys_run_out() {
        let mut k = Keymap::default();
        let text: String = (0..60)
            .map(|i| char::from_u32(0x4E00 + i).unwrap())
            .collect();
        let steps = k.type_text(&text).unwrap();
        let uploads = steps
            .iter()
            .filter(|s| matches!(s, Step::Keymap(_)))
            .count();
        assert_eq!(uploads, 2);
    }

    #[test]
    fn key_names_fit_x11() {
        let text = Keymap::default().text();
        for name in text.split('<').skip(1).filter_map(|s| s.split('>').next()) {
            assert!(name.len() <= 4, "{name}");
        }
    }
}
