//! Parsers for the cgroup v2 interface files. Each takes the file's text and
//! returns `None` when the text does not have the documented shape.

use std::collections::BTreeMap;

/// `cgroup.events`: whether the group or its descendants hold processes, and
/// whether the group is fully frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Events {
    pub populated: bool,
    pub frozen: bool,
}

/// `memory.stat`: flat `key value` pairs, in bytes or counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryStat(BTreeMap<String, u64>);

impl MemoryStat {
    pub fn get(&self, key: &str) -> Option<u64> {
        self.0.get(key).copied()
    }
}

pub(crate) fn events(text: &str) -> Option<Events> {
    let (mut populated, mut frozen) = (None, None);
    for line in text.lines() {
        match line.split_once(' ')? {
            ("populated", v) => populated = Some(flag(v)?),
            ("frozen", v) => frozen = Some(flag(v)?),
            _ => {}
        }
    }
    Some(Events {
        populated: populated?,
        frozen: frozen?,
    })
}

fn flag(v: &str) -> Option<bool> {
    match v.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

pub(crate) fn memory_stat(text: &str) -> Option<MemoryStat> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once(' ')?;
        map.insert(k.to_owned(), v.trim().parse().ok()?);
    }
    Some(MemoryStat(map))
}

/// The cgroup v2 path from `/proc/<pid>/cgroup`, from its `0::` line.
pub(crate) fn proc_cgroup(text: &str) -> Option<&str> {
    text.lines().find_map(|l| l.strip_prefix("0::"))
}

/// `usage_usec` from `cpu.stat`.
pub(crate) fn cpu_usage_us(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("usage_usec "))?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_parse() {
        let e = events("populated 1\nfrozen 0\n").unwrap();
        assert_eq!(
            e,
            Events {
                populated: true,
                frozen: false
            }
        );
        assert!(events("populated 1\n").is_none());
        assert!(events("populated 2\nfrozen 0\n").is_none());
    }

    #[test]
    fn memory_stat_parse() {
        let s = memory_stat("anon 4096\nfile 8192\nshmem 0\nzswap 12\n").unwrap();
        assert_eq!(s.get("zswap"), Some(12));
        assert!(memory_stat("anon x\n").is_none());
    }

    #[test]
    fn proc_cgroup_parse() {
        assert_eq!(
            proc_cgroup("0::/system.slice/x.scope\n"),
            Some("/system.slice/x.scope")
        );
        assert_eq!(proc_cgroup("1:name=systemd:/\n"), None);
    }

    #[test]
    fn cpu_usage_parse() {
        assert_eq!(cpu_usage_us("usage_usec 991\nuser_usec 500\n"), Some(991));
    }
}
