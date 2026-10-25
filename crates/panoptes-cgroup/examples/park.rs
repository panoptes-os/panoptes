//! Park a cgroup the way E4 did: freeze it, keep its reclaimed pages in
//! zswap, reclaim everything reclaimable, hold, thaw, and report.
//!
//! ```text
//! park PATH [HOLD_SECS]      park an existing group
//! park --hog MIB [HOLD_SECS] start an allocator in a new child of this
//!                            process's (delegated) group and park that
//! ```

use panoptes_cgroup::Cgroup;
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;
const TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(mib) = std::env::var("PANOPTES_HOG_MIB") {
        hog(mib.parse()?);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, mib, rest @ ..] if flag == "--hog" => {
            let hold = hold_secs(rest)?;
            let me = Cgroup::current()?;
            me.child("runner")?.attach(std::process::id())?;
            me.enable_controllers(&["memory", "cpu"])?;
            let cg = me.child("parked")?;
            let mib: u64 = mib.parse()?;
            let exe = std::env::current_exe()?;
            let child = cg.spawn(&[exe], &[("PANOPTES_HOG_MIB", &mib.to_string())])?;
            let start = Instant::now();
            while cg.memory_current()? < mib * MIB {
                if start.elapsed() > Duration::from_secs(30) {
                    return Err("allocator never reached its size".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let result = park(&cg, hold);
            drop(child);
            cg.wait_empty(TIMEOUT)?;
            cg.remove()?;
            result
        }
        [path, rest @ ..] => park(&Cgroup::open(path)?, hold_secs(rest)?),
        _ => Err("usage: park PATH [HOLD_SECS] | park --hog MIB [HOLD_SECS]".into()),
    }
}

fn hold_secs(rest: &[String]) -> Result<Duration, Box<dyn std::error::Error>> {
    Ok(Duration::from_secs(rest.first().map_or(Ok(20), |s| s.parse())?))
}

fn park(cg: &Cgroup, hold: Duration) -> Result<(), Box<dyn std::error::Error>> {
    let before = cg.memory_current()?;
    let stat = cg.memory_stat()?;
    cg.set_zswap_writeback(false)?;
    let froze = cg.freeze(TIMEOUT)?;
    let start = Instant::now();
    let outcome = cg.reclaim(stat.reclaimable())?;
    let reclaimed = start.elapsed();
    let after = cg.memory_current()?;
    let parked = cg.memory_stat()?;
    std::thread::sleep(hold);
    let held = cg.memory_current()?;
    let thawed = cg.thaw(TIMEOUT)?;
    println!("group\t{}", cg.path().display());
    println!("before_mib\t{:.1}", before as f64 / MIB as f64);
    println!("freeze_ms\t{:.2}", froze.as_secs_f64() * 1e3);
    println!("reclaim_ms\t{:.1}\t{outcome:?}", reclaimed.as_secs_f64() * 1e3);
    println!("parked_mib\t{:.1}", after as f64 / MIB as f64);
    println!("after_hold_mib\t{:.1}\t{}s", held as f64 / MIB as f64, hold.as_secs());
    println!(
        "zswap_mib\t{:.1}\tholds {:.1} MiB",
        parked.get("zswap").unwrap_or(0) as f64 / MIB as f64,
        parked.get("zswapped").unwrap_or(0) as f64 / MIB as f64
    );
    println!("thaw_ms\t{:.2}", thawed.as_secs_f64() * 1e3);
    Ok(())
}

/// Touch every page of `mib` MiB, a quarter of each page random and the rest
/// one repeated byte, then sleep until killed.
fn hog(mib: usize) -> ! {
    let mut v = vec![0u8; mib << 20];
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
