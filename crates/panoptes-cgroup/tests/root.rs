//! Kernel tests. They need root inside a delegated cgroup that holds only
//! this test binary; `tests/run-root.sh` builds one with systemd-run. Every
//! group they create lives under that delegated group and is removed.

use panoptes_cgroup::{Cgroup, Reclaim};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SEC: Duration = Duration::from_secs(1);
const MIB: u64 = 1 << 20;

/// The delegated group. This process moves into `runner/` so the delegated
/// group itself holds no processes and can enable controllers.
fn base() -> &'static Cgroup {
    static BASE: OnceLock<Cgroup> = OnceLock::new();
    BASE.get_or_init(|| {
        let me = Cgroup::current().expect("own cgroup");
        let runner = me.child("runner").expect("runner group");
        runner.attach(std::process::id()).expect("move into runner");
        me.enable_controllers(&["memory", "cpu", "pids"])
            .expect("enable controllers; is the group delegated?");
        me
    })
}

/// A fresh child of the delegated group, removed again on drop.
struct TestGroup(Option<Cgroup>);

impl TestGroup {
    fn new(name: &str) -> Self {
        TestGroup(Some(base().child(name).expect("test group")))
    }
}

impl std::ops::Deref for TestGroup {
    type Target = Cgroup;
    fn deref(&self) -> &Cgroup {
        self.0.as_ref().unwrap()
    }
}

impl Drop for TestGroup {
    fn drop(&mut self) {
        let cg = self.0.take().unwrap();
        let _ = cg.thaw(SEC);
        let _ = cg.wait_empty(5 * SEC);
        cg.remove().expect("remove test group");
    }
}

/// Wait until `f` holds, checking every 10 ms. Only for conditions the
/// kernel gives no event for, such as memory charge growing.
fn eventually(timeout: Duration, f: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

/// When `PANOPTES_HOG_MIB` is set, this "test" becomes the allocator that
/// the reclaim test parks: it touches every page of N MiB, a quarter of each
/// page random and the rest a repeated byte, then sleeps. Otherwise it does
/// nothing.
#[test]
#[ignore = "helper, run through PANOPTES_HOG_MIB"]
fn hog_helper() {
    let Ok(mib) = std::env::var("PANOPTES_HOG_MIB") else {
        return;
    };
    let mut v = vec![0u8; mib.parse::<usize>().unwrap() << 20];
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for (i, page) in v.chunks_mut(4096).enumerate() {
        let (random, rest) = page.split_at_mut(1024);
        for b in random {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        rest.fill(i as u8 | 1);
    }
    std::hint::black_box(&v);
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn spawn_hog(cg: &Cgroup, mib: u64) -> panoptes_cgroup::Child {
    let exe = std::env::current_exe().unwrap();
    let child = cg
        .spawn(
            &[exe.as_os_str(), "--ignored".as_ref(), "--exact".as_ref(), "hog_helper".as_ref()],
            &[("PANOPTES_HOG_MIB", &mib.to_string())],
        )
        .expect("spawn hog");
    assert!(
        eventually(30 * SEC, || cg.memory_current().unwrap() >= mib * MIB),
        "hog never reached {mib} MiB"
    );
    child
}

#[test]
#[ignore = "needs root in a delegated cgroup; see tests/run-root.sh"]
fn spawn_charges_the_child_from_the_start() {
    let cg = TestGroup::new("t-spawn");
    let child = cg.spawn(&["sleep", "30"], &[]).expect("spawn");
    assert_eq!(cg.procs().unwrap(), vec![child.pid()]);
    assert!(eventually(SEC, || cg.memory_current().unwrap() > 0));
    assert!(cg.events().unwrap().populated);
    // The parent's group never held the child.
    let runner = base().child("runner").unwrap();
    assert!(!runner.procs().unwrap().contains(&child.pid()));
    eprintln!(
        "spawn: pid {} charged {} KiB in its own group",
        child.pid(),
        cg.memory_current().unwrap() / 1024
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
#[ignore = "needs root in a delegated cgroup; see tests/run-root.sh"]
fn freeze_stops_cpu_and_thaw_resumes_it() {
    let cg = TestGroup::new("t-freeze");
    let child = cg.spawn(&["sh", "-c", "exec yes > /dev/null"], &[]).unwrap();
    assert!(eventually(SEC, || cg.cpu_usage_us().unwrap() > 10_000));

    let froze = cg.freeze(5 * SEC).unwrap();
    assert!(cg.events().unwrap().frozen);
    let before = cg.cpu_usage_us().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let frozen_cpu = cg.cpu_usage_us().unwrap() - before;

    let thawed = cg.thaw(5 * SEC).unwrap();
    assert!(!cg.events().unwrap().frozen);
    let before = cg.cpu_usage_us().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let thawed_cpu = cg.cpu_usage_us().unwrap() - before;

    eprintln!(
        "freeze: {froze:?} to frozen, {frozen_cpu} µs CPU in 300 ms frozen; \
         thaw: {thawed:?}, {thawed_cpu} µs CPU in 300 ms after"
    );
    assert!(frozen_cpu < 1_000, "frozen group used {frozen_cpu} µs");
    assert!(thawed_cpu > 100_000, "thawed group used only {thawed_cpu} µs");
    drop(child);
}

#[test]
#[ignore = "needs root in a delegated cgroup; see tests/run-root.sh"]
fn reclaim_parks_a_frozen_group_in_zswap() {
    let cg = TestGroup::new("t-reclaim");
    let hog = spawn_hog(&cg, 200);
    cg.set_zswap_writeback(false).unwrap();
    assert!(!cg.zswap_writeback().unwrap());

    let before = cg.memory_current().unwrap();
    let stat = cg.memory_stat().unwrap();
    let froze = cg.freeze(5 * SEC).unwrap();
    let start = Instant::now();
    let outcome = cg.reclaim(stat.reclaimable()).unwrap();
    let took = start.elapsed();
    let after = cg.memory_current().unwrap();
    let parked = cg.memory_stat().unwrap();
    let thawed = cg.thaw(5 * SEC).unwrap();

    eprintln!(
        "reclaim: {} MiB to {} MiB in {took:?} ({outcome:?}); zswap {} MiB holds \
         {} MiB; zswap written back to disk {} pages; freeze {froze:?}, thaw {thawed:?}",
        before / MIB,
        after / MIB,
        parked.get("zswap").unwrap_or(0) / MIB,
        parked.get("zswapped").unwrap_or(0) / MIB,
        parked.get("zswpwb").unwrap_or(0),
    );
    assert!(after < before / 2, "charge only fell from {before} to {after}");
    assert!(parked.get("zswapped").unwrap_or(0) > 100 * MIB);
    assert_eq!(parked.get("zswpwb").unwrap_or(0), 0, "writeback was off");
    assert_eq!(cg.procs().unwrap(), vec![hog.pid()], "hog survived the park");
    assert!(matches!(outcome, Reclaim::Full | Reclaim::Partial));
    drop(hog);
}

#[test]
#[ignore = "needs root in a delegated cgroup; see tests/run-root.sh"]
fn weights_and_pressure_round_trip() {
    let cg = TestGroup::new("t-weight");
    cg.set_cpu_weight(10_000).unwrap();
    assert_eq!(cg.cpu_weight().unwrap(), 10_000);
    assert!(cg.set_cpu_weight(0).is_err());
    let mem = cg.memory_pressure().unwrap();
    let cpu = cg.cpu_pressure().unwrap();
    assert!(mem.full.is_some());
    eprintln!("pressure: memory some total {} µs, cpu some total {} µs", mem.some.total_us, cpu.some.total_us);
    assert!(base().controllers().unwrap().iter().any(|c| c == "memory"));
}
