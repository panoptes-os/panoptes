//! The window list, built from panoptes_window_v1 events. A policy: no I/O,
//! clock or threads. `H` is the protocol object of a window, which the table
//! keeps only to hand back.
//!
//! Each window's properties arrive one event at a time and show at its
//! `done`:
//!
//! ```text
//! Announced ─> id 3, nothing shown
//! Pid, AppId, Title, Geometry, Parent, Focused ─> pending
//! Done ─> shown = pending          (a change)
//! Closed ─> gone                   (a change)
//! ```

use std::collections::BTreeMap;

/// One window as the compositor last described it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    /// The desk's number for the window, in the order windows mapped. The
    /// compositor reuses its object ids; these are never reused.
    pub id: u32,
    pub pid: i32,
    pub app_id: String,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub focused: bool,
    /// The window this one is a dialog of.
    pub parent: Option<u32>,
}

/// The shown windows, in the order they mapped, and Xwayland's DISPLAY.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Windows {
    pub xwayland: Option<String>,
    pub list: Vec<Window>,
}

/// One protocol event for window `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Pid(i32),
    AppId(String),
    Title(String),
    Geometry {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    Parent(Option<u32>),
    Focused(bool),
    Done,
    Closed,
}

#[derive(Debug, Clone)]
pub struct Table<H> {
    xwayland: Option<String>,
    last_id: u32,
    entries: BTreeMap<u32, Entry<H>>,
}

#[derive(Debug, Clone)]
struct Entry<H> {
    handle: H,
    /// Properties as they arrive.
    pending: Window,
    /// Properties at the last done; none before the first.
    shown: Option<Window>,
}

impl<H> Default for Table<H> {
    fn default() -> Self {
        Table {
            xwayland: None,
            last_id: 0,
            entries: BTreeMap::new(),
        }
    }
}

impl<H: Clone> Table<H> {
    pub fn set_xwayland(&mut self, display: String) {
        self.xwayland = Some(display);
    }

    /// A new window; returns its id.
    pub fn announce(&mut self, handle: H) -> u32 {
        self.last_id += 1;
        let id = self.last_id;
        let pending = Window {
            id,
            ..Window::default()
        };
        self.entries.insert(
            id,
            Entry {
                handle,
                pending,
                shown: None,
            },
        );
        id
    }

    /// Apply one event; true when what the table shows changed. Events for
    /// an unknown window are ignored.
    pub fn apply(&mut self, id: u32, event: Event) -> bool {
        if event == Event::Closed {
            return self.entries.remove(&id).is_some();
        }
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };
        let w = &mut entry.pending;
        match event {
            Event::Pid(pid) => w.pid = pid,
            Event::AppId(app_id) => w.app_id = app_id,
            Event::Title(title) => w.title = title,
            Event::Geometry {
                x,
                y,
                width,
                height,
            } => (w.x, w.y, w.width, w.height) = (x, y, width, height),
            Event::Parent(parent) => w.parent = parent,
            Event::Focused(focused) => w.focused = focused,
            Event::Done => {
                entry.shown = Some(entry.pending.clone());
                return true;
            }
            Event::Closed => unreachable!("handled above"),
        }
        false
    }

    pub fn windows(&self) -> Windows {
        Windows {
            xwayland: self.xwayland.clone(),
            list: self
                .entries
                .values()
                .filter_map(|e| e.shown.clone())
                .collect(),
        }
    }

    /// The protocol object of a shown window.
    pub fn handle(&self, id: u32) -> Option<H> {
        let e = self.entries.get(&id)?;
        e.shown.as_ref().map(|_| e.handle.clone())
    }
}

/// What changed between two lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub opened: Vec<Window>,
    pub closed: Vec<u32>,
    pub focused: Option<u32>,
}

pub fn report(before: &Windows, now: &Windows) -> Report {
    let had = |id: u32| before.list.iter().any(|b| b.id == id);
    let has = |id: u32| now.list.iter().any(|w| w.id == id);
    Report {
        opened: now.list.iter().filter(|w| !had(w.id)).cloned().collect(),
        closed: before
            .list
            .iter()
            .filter(|b| !has(b.id))
            .map(|b| b.id)
            .collect(),
        focused: now.list.iter().find(|w| w.focused).map(|w| w.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(t: &Table<()>) -> Vec<u32> {
        t.windows().list.iter().map(|w| w.id).collect()
    }

    #[test]
    fn a_window_shows_at_its_first_done() {
        let mut t = Table::default();
        let id = t.announce(());
        assert!(!t.apply(id, Event::Title("a".into())));
        assert_eq!(shown(&t), Vec::<u32>::new());
        assert_eq!(t.handle(id), None);
        assert!(t.apply(id, Event::Done));
        assert_eq!(t.windows().list[0].title, "a");
        assert_eq!(t.handle(id), Some(()));
    }

    #[test]
    fn changes_show_together_at_done() {
        let mut t = Table::default();
        let id = t.announce(());
        t.apply(id, Event::Done);
        t.apply(id, Event::Title("b".into()));
        t.apply(id, Event::Focused(true));
        assert_eq!(t.windows().list[0].title, "");
        t.apply(id, Event::Done);
        assert!(t.windows().list[0].focused);
    }

    #[test]
    fn ids_are_never_reused() {
        let mut t = Table::default();
        let a = t.announce(());
        assert!(t.apply(a, Event::Closed));
        let b = t.announce(());
        assert!(b > a);
        assert!(!t.apply(a, Event::Done));
    }

    #[test]
    fn the_xwayland_display_shows_in_the_list() {
        let mut t: Table<()> = Table::default();
        t.set_xwayland(":1".into());
        assert_eq!(t.windows().xwayland.as_deref(), Some(":1"));
    }

    #[test]
    fn events_for_unknown_windows_are_ignored() {
        let mut t: Table<()> = Table::default();
        assert!(!t.apply(7, Event::Done));
        assert!(!t.apply(7, Event::Closed));
    }

    /// Every sequence of up to 5 steps, each an announcement or a Title,
    /// Done or Closed for window 1, 2 or 3, against a model of which
    /// windows are open and have had a done. The shown list, the handles
    /// and each change flag must match the model.
    #[test]
    fn exhaustive_up_to_5_events() {
        #[derive(Clone, Default)]
        struct Model {
            open: Vec<u32>,
            done: Vec<u32>,
            last: u32,
        }
        fn walk(t: &Table<u32>, m: &Model, depth: u32) {
            let want: Vec<u32> = m
                .open
                .iter()
                .copied()
                .filter(|i| m.done.contains(i))
                .collect();
            assert_eq!(
                t.windows().list.iter().map(|w| w.id).collect::<Vec<_>>(),
                want
            );
            for id in 1..=3 {
                assert_eq!(t.handle(id), want.contains(&id).then_some(id));
            }
            if depth == 0 {
                return;
            }
            let mut steps = vec![None];
            for id in 1..=3 {
                for e in [Event::Title("t".into()), Event::Done, Event::Closed] {
                    steps.push(Some((id, e)));
                }
            }
            for step in steps {
                let (mut t2, mut m2) = (t.clone(), m.clone());
                match step {
                    None => {
                        m2.last += 1;
                        let id = t2.announce(m2.last);
                        assert_eq!(id, m2.last);
                        m2.open.push(id);
                    }
                    Some((id, e)) => {
                        let open = m2.open.contains(&id);
                        let want = open && matches!(e, Event::Done | Event::Closed);
                        match e {
                            Event::Done if open && !m2.done.contains(&id) => m2.done.push(id),
                            Event::Closed => {
                                m2.open.retain(|&i| i != id);
                                m2.done.retain(|&i| i != id);
                            }
                            _ => {}
                        }
                        assert_eq!(t2.apply(id, e), want);
                    }
                }
                walk(&t2, &m2, depth - 1);
            }
        }
        walk(&Table::default(), &Model::default(), 5);
    }

    #[test]
    fn a_report_names_opened_closed_and_focused() {
        let w = |id, focused| Window {
            id,
            focused,
            ..Window::default()
        };
        let before = Windows {
            xwayland: None,
            list: vec![w(1, true), w(2, false)],
        };
        let now = Windows {
            xwayland: None,
            list: vec![w(2, false), w(3, true)],
        };
        let r = report(&before, &now);
        assert_eq!(r.opened, vec![w(3, true)]);
        assert_eq!(r.closed, vec![1]);
        assert_eq!(r.focused, Some(3));
    }
}
