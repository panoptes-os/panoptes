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

/// One line of a PSI file: the share of wall time some (or all) tasks
/// stalled, averaged over 10, 60, and 300 s, and the total stall in µs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PressureLine {
    pub avg10: f32,
    pub avg60: f32,
    pub avg300: f32,
    pub total_us: u64,
}

/// A PSI file such as `memory.pressure`. `full` is absent on kernels that
/// report only `some` for a resource.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pressure {
    pub some: PressureLine,
    pub full: Option<PressureLine>,
}

impl MemoryStat {
    pub fn get(&self, key: &str) -> Option<u64> {
        self.0.get(key).copied()
    }

    /// Bytes reclaim can move out of the group: anonymous memory and shmem
    /// go to zswap or swap, file pages are dropped or written back.
    pub fn reclaimable(&self) -> u64 {
        ["anon", "file", "shmem"]
            .iter()
            .filter_map(|k| self.get(k))
            .sum()
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

pub(crate) fn pressure(text: &str) -> Option<Pressure> {
    let (mut some, mut full) = (None, None);
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (kind, rest) = line.split_once(' ')?;
        let parsed = pressure_line(rest)?;
        match kind {
            "some" => some = Some(parsed),
            "full" => full = Some(parsed),
            _ => return None,
        }
    }
    Some(Pressure { some: some?, full })
}

fn pressure_line(rest: &str) -> Option<PressureLine> {
    let mut fields = [None; 4];
    for kv in rest.split_whitespace() {
        let (k, v) = kv.split_once('=')?;
        let i = ["avg10", "avg60", "avg300", "total"]
            .iter()
            .position(|n| *n == k)?;
        fields[i] = Some(v);
    }
    Some(PressureLine {
        avg10: fields[0]?.parse().ok()?,
        avg60: fields[1]?.parse().ok()?,
        avg300: fields[2]?.parse().ok()?,
        total_us: fields[3]?.parse().ok()?,
    })
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
        assert_eq!(s.reclaimable(), 12288);
        assert!(memory_stat("anon x\n").is_none());
    }

    #[test]
    fn pressure_parse() {
        let p = pressure(
            "some avg10=0.12 avg60=1.50 avg300=0.00 total=42\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=7\n",
        )
        .unwrap();
        assert_eq!(p.some.total_us, 42);
        assert!((p.some.avg60 - 1.5).abs() < f32::EPSILON);
        assert_eq!(p.full.unwrap().total_us, 7);
        let only_some = pressure("some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n").unwrap();
        assert!(only_some.full.is_none());
        assert!(pressure("some avg10=0.00 total=0\n").is_none());
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
