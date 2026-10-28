//! panoptesctl: Panoptes command line.
//!
//! ```text
//! panoptesctl                          print this process's cgroup
//! panoptesctl desk SOCK TOOL [ARGS]    call one desk tool, print the reply line
//! panoptesctl resd SOCK JSON           send one request to panoptes-resd
//! panoptesctl spawn --cgroup DIR [--owner UID:GID] [--log FILE] -- ARGV...
//!                                      start ARGV inside a new cgroup and return
//! panoptesctl kill --cgroup DIR        kill everything in DIR and remove it
//! ```
//!
//! `spawn` and `kill` launch desks where there is no systemd, as inside the
//! container: `spawn` stands in for `systemd-run -p Delegate=yes` and `kill`
//! for `systemctl stop`.

use panoptes_cgroup::Cgroup;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => whoami(),
        Some("desk") if (3..=4).contains(&args.len()) => {
            desk(&args[1], &args[2], args.get(3).map_or("{}", String::as_str))
        }
        Some("resd") if args.len() == 3 => line(&args[1], &args[2]),
        Some("spawn") => report(spawn(&args[1..])),
        Some("kill") if args.len() == 3 && args[1] == "--cgroup" => {
            report(kill(Path::new(&args[2])))
        }
        _ => {
            eprintln!(
                "usage: panoptesctl [desk SOCK TOOL [ARGS_JSON] | resd SOCK JSON\n\
                 \x20 | spawn --cgroup DIR [--owner UID:GID] [--log FILE] -- ARGV... | kill --cgroup DIR]"
            );
            2
        }
    };
    std::process::exit(code);
}

fn whoami() -> i32 {
    match panoptes_cgroup::Cgroup::current() {
        Ok(cg) => {
            println!(
                "panoptesctl {}: running in {}",
                env!("CARGO_PKG_VERSION"),
                cg.path().display()
            );
            0
        }
        Err(e) => {
            eprintln!("panoptesctl: {e}");
            1
        }
    }
}

/// One request on the desk's internal protocol. ARGS is passed through as
/// raw JSON; the desk rejects it if it does not parse.
fn desk(sock: &str, tool: &str, args: &str) -> i32 {
    let tool = tool.replace('\\', "\\\\").replace('"', "\\\"");
    line(
        sock,
        &format!(r#"{{"id":1,"tool":"{tool}","args":{args}}}"#),
    )
}

/// Send one request line and print the reply; exit 0 when it says ok.
fn line(sock: &str, req: &str) -> i32 {
    let run = || -> std::io::Result<String> {
        let mut s = UnixStream::connect(sock)?;
        writeln!(s, "{req}")?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line)?;
        Ok(line)
    };
    match run() {
        Ok(line) if line.contains(r#""ok":true"#) => {
            print!("{line}");
            0
        }
        Ok(line) => {
            print!("{line}");
            1
        }
        Err(e) => {
            eprintln!("panoptesctl: {sock}: {e}");
            1
        }
    }
}

fn report(r: Result<(), String>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("panoptesctl: {e}");
            1
        }
    }
}

/// The files of a delegated cgroup its owner writes, as systemd's
/// `Delegate=yes` hands them over.
const DELEGATED: &[&str] = &[
    "",
    "cgroup.procs",
    "cgroup.subtree_control",
    "cgroup.threads",
];

/// Create DIR, hand it to OWNER, and start ARGV in it with clone3, so the
/// process's first page is charged there. ARGV starts as root; to run as
/// OWNER, start it through `setpriv`. Returns once the process has started.
fn spawn(args: &[String]) -> Result<(), String> {
    let (mut dir, mut owner, mut log) = (None, None, None);
    let mut it = args.iter();
    let argv: Vec<&String> = loop {
        match it.next().map(String::as_str) {
            Some("--cgroup") => dir = it.next(),
            Some("--owner") => owner = it.next(),
            Some("--log") => log = it.next(),
            Some("--") => break it.collect(),
            other => return Err(format!("spawn: unexpected {other:?}")),
        }
    };
    let dir = Path::new(dir.ok_or("spawn: --cgroup DIR is required")?);
    if argv.is_empty() {
        return Err("spawn: nothing to run after --".into());
    }
    std::fs::create_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if let Some(o) = owner {
        let (uid, gid) = o
            .split_once(':')
            .and_then(|(u, g)| Some((u.parse().ok()?, g.parse().ok()?)))
            .ok_or("spawn: --owner wants UID:GID")?;
        for f in DELEGATED {
            let p = dir.join(f);
            std::os::unix::fs::chown(&p, Some(uid), Some(gid))
                .map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let cg = Cgroup::open(dir).map_err(|e| e.to_string())?;
    let null = std::fs::File::open("/dev/null").map_err(|e| format!("/dev/null: {e}"))?;
    let out = match log {
        Some(l) => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(l)
            .map_err(|e| format!("{l}: {e}"))?,
        None => std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .map_err(|e| e.to_string())?,
    };
    let fds = [(null.as_fd(), 0), (out.as_fd(), 1), (out.as_fd(), 2)];
    let child = cg
        .spawn_with_fds(&argv, &[], &fds)
        .map_err(|e| e.to_string())?;
    child.detach();
    Ok(())
}

/// SIGKILL everything under DIR, wait for it to empty, and remove DIR and
/// its children, deepest first. A missing DIR is already stopped.
fn kill(dir: &Path) -> Result<(), String> {
    let Ok(cg) = Cgroup::open(dir) else {
        return Ok(());
    };
    cg.kill_all().map_err(|e| e.to_string())?;
    cg.wait_empty(Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    remove_tree(dir)
}

fn remove_tree(dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in entries.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            remove_tree(&e.path())?;
        }
    }
    std::fs::remove_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))
}
