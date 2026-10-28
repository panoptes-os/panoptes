//! Fleets: one container per fleet, running panoptes-loop over a task file
//! with its own panoptes-resd.
//!
//! ```text
//! host                          container panoptes-NAME
//! dirname(TASKS)/   ro bind ->  /tasks/
//! RESULTS/          rw bind ->  /results/NAME/{trace,results}.jsonl, agent-N/
//! loop config, key  cp ->       /etc/panoptes/{loop.json,key}, root only
//! ```
//!
//! The runtime holds the only state: a fleet is a container labelled
//! `panoptes.fleet=NAME`, so `ls` and `down` ask the runtime and keep no
//! files of their own.
//!
//! The key goes in with `cp` from a tar on stdin, between `create` and
//! `start`. It never lands on a command line, in the environment, or in a
//! mount, so `ps` and `inspect` cannot show it. It does sit in the
//! container's writable layer, root only, until `down` removes the
//! container.

mod check;

pub use check::{Check, check};

use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The image `up` runs unless told otherwise. publish-image.yml pushes it
/// on every push to main.
// SPONGE: Pin this to the CLI's own version once releases tag images?
//         Today only :latest and commit tags exist, so a CLI and the image
//         it starts can drift apart.
pub const DEFAULT_IMAGE: &str = "ghcr.io/panoptes-os/panoptes:latest";

/// What the engine needs from the runtime, and why.
const RUN_FLAGS: &[&str] = &[
    // panoptes-resd owns the container's cgroup subtree: it freezes,
    // reclaims, and weights each agent's kiosk there.
    "--privileged",
    "--cgroupns=private",
    // The loop and the desks reach services on the host's loopback: a local
    // model server, the demo's ERPNext and portals.
    "--network=host",
    // Chromium shares memory through /dev/shm; containers get 64 MiB.
    "--shm-size=4g",
    // Each desk takes about 155 pids; Podman's default limit of 2048 stops
    // Chromium's threads at about 13 desks.
    "--pids-limit=-1",
    // The loop holds a socket per desk and per model connection. The
    // 700-agent run had 65536 (bench/resd/scale.sh:21); runtimes differ in
    // what a container gets otherwise.
    "--ulimit=nofile=65536:65536",
];

// Paths inside the container.
const TASKS_DIR: &str = "/tasks";
const RESULTS_DIR: &str = "/results";
/// The cgroup panoptes-entry hands to panoptes-resd (nix/entry.sh).
const SLICE: &str = "/sys/fs/cgroup/panoptes.slice";
const RESD_SOCKET: &str = "/run/panoptes/resd.sock";
/// The image's unprivileged user; Chromium's sandbox refuses root.
const DESK_UID: u32 = 1000;
/// `cp` extracts the secrets archive here: `panoptes/` is root only, since
/// the desks run as DESK_UID and drive a browser that could be pointed at
/// `file:///etc/panoptes/key`.
const SECRETS_PARENT: &str = "/etc";
const CONFIG_PATH: &str = "/etc/panoptes/loop.json";
const KEY_PATH: &str = "/etc/panoptes/key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Podman,
    Docker,
}

/// The container runtime and how to call it.
#[derive(Debug, Clone)]
pub struct Runtime {
    pub program: PathBuf,
    pub kind: Kind,
}

impl Runtime {
    /// `choice` is `podman`, `docker`, or a path to either. Without one:
    /// podman if it is on PATH, else docker.
    pub fn find(choice: Option<&str>) -> Result<Runtime, String> {
        let program = match choice {
            Some(c) if c.contains('/') => PathBuf::from(c),
            Some(c) => on_path(c).ok_or_else(|| format!("{c} is not on PATH"))?,
            None => on_path("podman")
                .or_else(|| on_path("docker"))
                .ok_or("neither podman nor docker is on PATH")?,
        };
        let kind = match program.file_name().and_then(OsStr::to_str) {
            Some("podman") => Kind::Podman,
            Some("docker") => Kind::Docker,
            _ => {
                return Err(format!(
                    "{}: the runtime must be podman or docker",
                    program.display()
                ));
            }
        };
        Ok(Runtime { program, kind })
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// A model key. Debug never prints it.
pub struct Key(String);

impl Key {
    pub fn new(bytes: Vec<u8>) -> Result<Key, String> {
        let key = String::from_utf8(bytes).map_err(|_| "the key is not UTF-8")?;
        if key.trim().is_empty() {
            return Err("the key is empty".into());
        }
        Ok(Key(key))
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

/// A fleet to start. Paths are absolute, on the host.
#[derive(Debug)]
pub struct Up {
    pub name: String,
    pub image: String,
    /// The task file; its directory is mounted read-only at /tasks.
    pub tasks: PathBuf,
    /// The results root; the run writes `results/NAME/`.
    pub results: PathBuf,
    /// A panoptes-loop config to start from, `{}` for none, with paths as
    /// the container sees them. `up` sets `desk`, `out_dir`, `run_name`,
    /// and the provider's `key_file`.
    pub config: Value,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub key: Option<Key>,
}

/// The runtime commands `up` runs, in order, and the loop config it
/// copies in. `--dry-run` prints this; `up` runs it.
#[derive(Debug)]
pub struct Plan {
    pub container: String,
    pub create: Vec<String>,
    /// Reads the secrets archive on stdin.
    pub copy: Vec<String>,
    pub start: Vec<String>,
    pub config: Value,
}

/// A fleet as the runtime reports it.
#[derive(Debug, Clone)]
pub struct Fleet {
    pub name: String,
    pub container: String,
    /// The runtime's word: created, running, exited.
    pub state: String,
    /// Set once the fleet has exited.
    pub exit_code: Option<i64>,
    pub image: String,
    pub tasks: PathBuf,
    /// The results root; the run is under `results/NAME/`.
    pub results: PathBuf,
}

pub fn plan(spec: &Up) -> Result<Plan, String> {
    check_name(&spec.name)?;
    for p in [&spec.tasks, &spec.results] {
        let s = p
            .to_str()
            .ok_or_else(|| format!("{}: not UTF-8", p.display()))?;
        if !p.is_absolute() {
            return Err(format!("{s}: not an absolute path"));
        }
        // `--volume SRC:DST` cannot carry a colon in SRC.
        if s.contains(':') {
            return Err(format!("{s}: a path with ':' cannot be mounted"));
        }
    }
    let (Some(dir), Some(file)) = (
        spec.tasks.parent().and_then(Path::to_str),
        spec.tasks.file_name().and_then(OsStr::to_str),
    ) else {
        return Err(format!("{}: not a file path", spec.tasks.display()));
    };
    let results = spec.results.to_str().expect("checked above");
    let tasks = spec.tasks.to_str().expect("checked above");
    let container = container(&spec.name);

    let mut create: Vec<String> = vec!["create".into(), "--name".into(), container.clone()];
    for (k, v) in [
        ("panoptes.fleet", spec.name.as_str()),
        ("panoptes.tasks", tasks),
        ("panoptes.results", results),
    ] {
        create.push(format!("--label={k}={v}"));
    }
    create.extend(RUN_FLAGS.iter().map(|f| f.to_string()));
    create.push(format!("--volume={dir}:{TASKS_DIR}:ro"));
    create.push(format!("--volume={results}:{RESULTS_DIR}"));
    create.push(spec.image.clone());
    let tasks_in = format!("{TASKS_DIR}/{file}");
    create.extend(["panoptes-loop", CONFIG_PATH, "--tasks", &tasks_in].map(String::from));

    Ok(Plan {
        copy: vec![
            "cp".into(),
            "-".into(),
            format!("{container}:{SECRETS_PARENT}"),
        ],
        start: vec!["start".into(), container.clone()],
        container,
        create,
        config: loop_config(spec)?,
    })
}

/// The loop config: the caller's, with what only `up` knows set over it.
fn loop_config(spec: &Up) -> Result<Value, String> {
    let mut cfg = spec.config.clone();
    let obj = cfg.as_object_mut().ok_or("config: not a JSON object")?;
    let provider = obj
        .entry("provider")
        .or_insert_with(|| "default".into())
        .as_str()
        .ok_or("config: provider is not a string")?
        .to_owned();
    let providers = obj
        .entry("providers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("config: providers is not an object")?;
    // A key_file names a host path the container cannot see.
    if let Some(name) = providers
        .iter()
        .find_map(|(n, p)| p.get("key_file").map(|_| n))
    {
        return Err(format!(
            "config: providers.{name}.key_file: pass the key with --key-file instead"
        ));
    }
    let p = providers
        .entry(provider.as_str())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("config: providers.{provider} is not an object"))?;
    if let Some(u) = &spec.base_url {
        p.insert("base_url".into(), u.as_str().into());
    }
    if let Some(m) = &spec.model {
        p.insert("model".into(), m.as_str().into());
    }
    if spec.key.is_some() {
        p.insert("key_file".into(), KEY_PATH.into());
    }
    for f in ["base_url", "model"] {
        if !p.contains_key(f) {
            return Err(format!(
                "provider {provider} has no {f}: pass --base-url and --model, \
                 or a --config that sets them"
            ));
        }
    }
    obj.insert("desk".into(), desk());
    obj.insert("out_dir".into(), RESULTS_DIR.into());
    obj.insert("run_name".into(), spec.name.as_str().into());
    Ok(cfg)
}

/// How the loop starts and stops agent `{i}`'s desk in the container, where
/// there is no systemd: `panoptesctl spawn` stands in for `systemd-run -p
/// Delegate=yes`. The cgroup takes the unit name panoptes-top maps to loop
/// agent `{i}`. The loop makes `{run}` as root, so the desk's user gets it
/// first. The desk flags are the 700-agent run's (bench/resd/scale.sh:28).
fn desk() -> Value {
    let u = DESK_UID;
    let cgroup = format!("{SLICE}/pdesk-loop-{{i}}.service");
    let launch = format!(
        "chown {u}:{u} {{run}} && exec panoptesctl spawn --cgroup {cgroup} \
         --owner {u}:{u} --log {{run}}/desk.log -- \
         setpriv --reuid={u} --regid={u} --clear-groups -- \
         panoptes-desk --run-dir {{run}} --size 1280x800 --resd {RESD_SOCKET} --thumb-ms 1000"
    );
    json!({
        "launch": ["sh", "-c", launch],
        "stop": ["panoptesctl", "kill", "--cgroup", cgroup],
        "start_timeout_s": 120,
    })
}

/// Start a fleet: create its container, copy in the loop config and key,
/// start it. Run `check` first; `up` does not look at the host.
pub fn up(rt: &Runtime, spec: &Up) -> Result<(), String> {
    let plan = plan(spec)?;
    if ls(rt)?.iter().any(|f| f.name == spec.name) {
        return Err(format!(
            "fleet {0} exists; remove it with: panoptes down --name {0}",
            spec.name
        ));
    }
    std::fs::create_dir_all(&spec.results)
        .map_err(|e| format!("{}: {e}", spec.results.display()))?;
    pull_missing(rt, &spec.image)?;
    run(rt, &plan.create, None)?;
    let config = serde_json::to_vec_pretty(&plan.config).expect("a Value serializes");
    let mut files = vec![("loop.json", 0o600, config.as_slice())];
    if let Some(k) = &spec.key {
        files.push(("key", 0o400, k.0.as_bytes()));
    }
    let archive = archive("panoptes", &files);
    let started = run(rt, &plan.copy, Some(&archive)).and_then(|_| run(rt, &plan.start, None));
    if let Err(e) = started {
        let _ = run(rt, &["rm", "--force", plan.container.as_str()], None);
        return Err(e);
    }
    Ok(())
}

pub fn ls(rt: &Runtime) -> Result<Vec<Fleet>, String> {
    let ids = run(
        rt,
        &[
            "ps",
            "--all",
            "--quiet",
            "--no-trunc",
            "--filter",
            "label=panoptes.fleet",
        ],
        None,
    )?;
    let ids: Vec<&str> = ids.split_whitespace().collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let out = run(rt, &[&["container", "inspect"][..], &ids].concat(), None)?;
    let v: Value = serde_json::from_str(&out).map_err(|e| format!("inspect: {e}"))?;
    let mut fleets: Vec<Fleet> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let labels = &c["Config"]["Labels"];
            let label = |k: &str| labels[k].as_str().unwrap_or("").to_owned();
            let state = c["State"]["Status"].as_str().unwrap_or("").to_owned();
            Some(Fleet {
                name: labels["panoptes.fleet"].as_str()?.to_owned(),
                container: c["Name"].as_str()?.trim_start_matches('/').to_owned(),
                exit_code: (state == "exited")
                    .then(|| c["State"]["ExitCode"].as_i64())
                    .flatten(),
                state,
                image: c["Config"]["Image"].as_str().unwrap_or("").to_owned(),
                tasks: label("panoptes.tasks").into(),
                results: label("panoptes.results").into(),
            })
        })
        .collect();
    fleets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(fleets)
}

/// Stop a fleet and remove its container. Its results stay on the host.
pub fn down(rt: &Runtime, name: &str) -> Result<(), String> {
    check_name(name)?;
    let Some(f) = ls(rt)?.into_iter().find(|f| f.name == name) else {
        return Err(format!("no fleet named {name}"));
    };
    run(rt, &["rm", "--force", f.container.as_str()], None).map(|_| ())
}

pub fn container(name: &str) -> String {
    format!("panoptes-{name}")
}

/// panoptes-top on a running fleet. It runs inside the container, because
/// resd's socket is in the container's /run.
pub fn top(rt: &Runtime, name: &str) -> Result<Command, String> {
    check_name(name)?;
    let mut c = Command::new(&rt.program);
    c.args(["exec", "-it", &container(name), "panoptes-top", "--run"])
        .arg(format!("{RESULTS_DIR}/{name}"));
    Ok(c)
}

/// A fleet name becomes part of a container name, of `{run}` in the
/// desk's shell line, and of a results directory, so it stays plain.
fn check_name(name: &str) -> Result<(), String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_.-".contains(c);
    let first_ok = name.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !first_ok || !name.chars().all(plain) || name.len() > 64 {
        return Err(format!(
            "fleet name {name:?}: use up to 64 letters, digits, '_', '.', or '-', \
             starting with a letter or digit"
        ));
    }
    Ok(())
}

fn pull_missing(rt: &Runtime, image: &str) -> Result<(), String> {
    if run(rt, &["image", "inspect", image], None).is_ok() {
        return Ok(());
    }
    // Progress goes to stderr: a caller may speak a protocol on stdout.
    let st = Command::new(&rt.program)
        .args(["pull", image])
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .status()
        .map_err(|e| format!("{}: {e}", rt.program.display()))?;
    if !st.success() {
        return Err(format!(
            "could not pull {image}; for an image built from this tree, pass --image"
        ));
    }
    Ok(())
}

/// Run the runtime with `args` and `input` on stdin; return its stdout.
fn run(rt: &Runtime, args: &[impl AsRef<OsStr>], input: Option<&[u8]>) -> Result<String, String> {
    let what = || {
        let sub = args
            .first()
            .map_or("".into(), |a| a.as_ref().to_string_lossy());
        format!("{} {sub}", rt.program.display())
    };
    let mut child = Command::new(&rt.program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", what()))?;
    // The archive is a few KiB, well under a pipe's buffer, so writing all
    // of it before reading stdout cannot deadlock. A runtime that exits
    // early breaks the pipe; its own error, read below, says why.
    let wrote = input.map_or(Ok(()), |bytes| {
        child.stdin.take().expect("piped").write_all(bytes)
    });
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{}: {e}", what()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{}: {}", what(), err.trim()));
    }
    wrote.map_err(|e| format!("{}: {e}", what()))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A ustar archive (POSIX.1-1988) of directory `dir` and its `files` as
/// (name, mode, contents), owned by root, for `cp -`.
fn archive(dir: &str, files: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    header(&mut out, &format!("{dir}/"), 0o700, 0, b'5');
    for &(name, mode, data) in files {
        header(&mut out, &format!("{dir}/{name}"), mode, data.len(), b'0');
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(512), 0);
    }
    // Two zero blocks end the archive.
    out.resize(out.len() + 1024, 0);
    out
}

fn header(out: &mut Vec<u8>, name: &str, mode: u32, size: usize, typeflag: u8) {
    // Numeric fields are zero-padded octal ending in NUL.
    fn octal(field: &mut [u8], v: u64) {
        let w = field.len() - 1;
        let s = format!("{v:0w$o}");
        assert_eq!(s.len(), w, "{v} does not fit a {w}-digit tar field");
        field[..w].copy_from_slice(s.as_bytes());
    }
    assert!(name.len() < 100, "tar name {name} needs the prefix field");
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], mode.into());
    octal(&mut h[108..116], 0); // uid
    octal(&mut h[116..124], 0); // gid
    octal(&mut h[124..136], size as u64);
    octal(&mut h[136..148], mtime);
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    // The checksum counts its own field as spaces, and is stored as six
    // octal digits, NUL, space.
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out.extend_from_slice(&h);
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMMY_KEY: &str = "sk-dummy-0123456789";

    fn spec(config: Value, key: bool) -> Up {
        Up {
            name: "research".into(),
            image: "localhost/panoptes:0.1.0".into(),
            tasks: "/home/u/work/tasks.jsonl".into(),
            results: "/home/u/results".into(),
            config,
            base_url: Some("http://127.0.0.1:8000/v1".into()),
            model: Some("m".into()),
            key: key.then(|| Key::new(DUMMY_KEY.into()).unwrap()),
        }
    }

    #[test]
    fn plan_mounts_tasks_and_results_and_keeps_the_key_off_the_command_line() {
        let p = plan(&spec(json!({}), true)).unwrap();
        for f in RUN_FLAGS {
            assert!(p.create.iter().any(|a| a == f), "missing {f}");
        }
        assert_eq!(
            p.create[p.create.len() - 7..],
            [
                "--volume=/home/u/work:/tasks:ro",
                "--volume=/home/u/results:/results",
                "localhost/panoptes:0.1.0",
                "panoptes-loop",
                "/etc/panoptes/loop.json",
                "--tasks",
                "/tasks/tasks.jsonl",
            ]
        );
        assert_eq!(p.copy, ["cp", "-", "panoptes-research:/etc"]);
        let everything = format!("{:?} {:?} {:?} {}", p.create, p.copy, p.start, p.config);
        assert!(!everything.contains(DUMMY_KEY));
    }

    #[test]
    fn config_gets_the_key_path_and_the_container_desk() {
        let p = plan(&spec(json!({}), true)).unwrap();
        let c = &p.config;
        assert_eq!(c["provider"], "default");
        assert_eq!(c["providers"]["default"]["key_file"], KEY_PATH);
        assert_eq!(c["out_dir"], "/results");
        assert_eq!(c["run_name"], "research");
        let launch = c["desk"]["launch"][2].as_str().unwrap();
        assert!(launch.contains("--cgroup /sys/fs/cgroup/panoptes.slice/pdesk-loop-{i}.service"));
        assert!(launch.contains("--run-dir {run}"));
        // Without a key the provider gets no key_file.
        let p = plan(&spec(json!({}), false)).unwrap();
        assert!(p.config["providers"]["default"].get("key_file").is_none());
    }

    #[test]
    fn config_from_the_caller_keeps_its_fields_but_not_its_paths() {
        let base = json!({
            "provider": "oracle",
            "providers": { "oracle": { "base_url": "http://x/v1", "model": "o", "auth": "api-key" } },
            "out_dir": "/var/tmp/somewhere",
            "step_cap": 5,
        });
        let mut s = spec(base, true);
        (s.base_url, s.model) = (None, None);
        let c = plan(&s).unwrap().config;
        assert_eq!(c["providers"]["oracle"]["auth"], "api-key");
        assert_eq!(c["providers"]["oracle"]["key_file"], KEY_PATH);
        assert_eq!(c["step_cap"], 5);
        assert_eq!(c["out_dir"], "/results");

        let base = json!({ "providers": { "default": { "key_file": "/home/u/.key" } } });
        assert!(plan(&spec(base, false)).unwrap_err().contains("--key-file"));
        let mut s = spec(json!({}), false);
        s.model = None;
        assert!(plan(&s).unwrap_err().contains("has no model"));
    }

    #[test]
    fn names_and_paths_are_checked() {
        for bad in ["", "a b", "../x", "-x", "x;rm", &"n".repeat(65)] {
            let mut s = spec(json!({}), false);
            s.name = bad.into();
            assert!(plan(&s).is_err(), "{bad:?} passed");
        }
        let mut s = spec(json!({}), false);
        s.results = "results".into();
        assert!(plan(&s).unwrap_err().contains("absolute"));
        s.results = "/a:b".into();
        assert!(plan(&s).unwrap_err().contains("':'"));
    }

    /// tar itself is the oracle: what it extracts, with which modes.
    #[test]
    fn archive_extracts_with_its_modes() {
        use std::os::unix::fs::PermissionsExt;
        let files: [(&str, u32, &[u8]); 2] = [("loop.json", 0o600, b"{}"), ("key", 0o400, b"k\n")];
        let a = archive("panoptes", &files);
        assert_eq!(a.len() % 512, 0);
        let dir = std::env::temp_dir().join(format!("panoptes-tar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut tar = Command::new("tar")
            .args(["-x", "-p", "-f", "-", "-C"])
            .arg(&dir)
            .stdin(Stdio::piped())
            .spawn()
            .expect("tar");
        tar.stdin.take().unwrap().write_all(&a).unwrap();
        assert!(tar.wait().unwrap().success());
        let mode = |p: &str| std::fs::metadata(dir.join(p)).unwrap().permissions().mode() & 0o777;
        let got = (
            mode("panoptes"),
            mode("panoptes/loop.json"),
            mode("panoptes/key"),
            std::fs::read(dir.join("panoptes/key")).unwrap(),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(got, (0o700, 0o600, 0o400, b"k\n".to_vec()));
    }

    #[test]
    fn top_runs_inside_the_fleet_container() {
        let rt = Runtime {
            program: "podman".into(),
            kind: Kind::Podman,
        };
        let c = top(&rt, "research").unwrap();
        let args: Vec<_> = c.get_args().map(|a| a.to_str().unwrap()).collect();
        let want = ["exec", "-it", "panoptes-research", "panoptes-top", "--run"];
        assert_eq!(args[..5], want);
        assert_eq!(args[5], "/results/research");
        assert!(top(&rt, "../x").is_err());
    }
}
