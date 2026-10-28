//! panoptes: run fleets of agents on this host, each in its own container.
//!
//! ```text
//! panoptes check               what parking needs from the host, with fixes
//! panoptes up --tasks FILE     check the host, then start a fleet
//! panoptes ls                  the fleets on this host
//! panoptes down                stop a fleet and remove its container
//! panoptes top                 watch a running fleet in panoptes-top
//! ```

use panoptes::fleet::{self, Check, Key, Runtime, Up};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

fn usage() -> String {
    format!(
        "usage: panoptes up --tasks FILE [--name NAME] [--results DIR] [--config FILE]
                   [--base-url URL] [--model NAME] [--key-file FILE] [--image REF]
                   [--runtime R] [--ignore-checks] [--dry-run]
       panoptes ls [--runtime R]
       panoptes down [--name NAME] [--runtime R]
       panoptes top [--name NAME] [--runtime R]
       panoptes check [--runtime R]

  --tasks FILE     the task file; its directory is mounted read-only at /tasks
  --name NAME      the fleet (default: default); its container is panoptes-NAME
  --results DIR    results root (default: ./results); the run writes DIR/NAME/
  --config FILE    a panoptes-loop config to start from, with paths as the
                   container sees them; up sets its desk, out_dir, run_name,
                   and key_file
  --base-url URL   the provider's OpenAI-compatible endpoint
  --model NAME     the provider's model
  --key-file FILE  the model key, or - to read it from stdin; copied into the
                   container as a root-only file, never onto a command line
  --image REF      the engine image (default: {})
  --runtime R      podman or docker, by name or path (default: podman if it
                   is on PATH, else docker)
  --ignore-checks  start even when host checks fail
  --dry-run        print the runtime commands and loop config up would use",
        fleet::DEFAULT_IMAGE
    )
}

#[derive(Debug, Default)]
struct Opts {
    name: Option<String>,
    tasks: Option<String>,
    results: Option<String>,
    config: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    key_file: Option<String>,
    image: Option<String>,
    runtime: Option<String>,
    ignore_checks: bool,
    dry_run: bool,
}

fn parse(cmd: &str, args: &[String]) -> Result<Opts, String> {
    let allowed: &[&str] = match cmd {
        "up" => &[
            "--name",
            "--tasks",
            "--results",
            "--config",
            "--base-url",
            "--model",
            "--key-file",
            "--image",
            "--runtime",
            "--ignore-checks",
            "--dry-run",
        ],
        "down" | "top" => &["--name", "--runtime"],
        _ => &["--runtime"],
    };
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !allowed.contains(&a.as_str()) {
            return Err(format!("{cmd}: unknown argument {a}"));
        }
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--name" => o.name = Some(val()?),
            "--tasks" => o.tasks = Some(val()?),
            "--results" => o.results = Some(val()?),
            "--config" => o.config = Some(val()?),
            "--base-url" => o.base_url = Some(val()?),
            "--model" => o.model = Some(val()?),
            "--key-file" => o.key_file = Some(val()?),
            "--image" => o.image = Some(val()?),
            "--runtime" => o.runtime = Some(val()?),
            "--ignore-checks" => o.ignore_checks = true,
            "--dry-run" => o.dry_run = true,
            _ => unreachable!("{a} is in the allowed list"),
        }
    }
    Ok(o)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", usage());
        std::process::exit(0);
    }
    let code = match args.first().map(String::as_str) {
        Some(cmd @ ("up" | "ls" | "down" | "top" | "check")) => {
            match parse(cmd, &args[1..]).and_then(|o| run(cmd, o)) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("panoptes: {e}");
                    1
                }
            }
        }
        _ => {
            eprintln!("{}", usage());
            2
        }
    };
    std::process::exit(code);
}

fn run(cmd: &str, o: Opts) -> Result<i32, String> {
    let name = o.name.clone().unwrap_or_else(|| "default".into());
    if cmd == "check" {
        // With no runtime found, the checks say so among the rest.
        let rt = Runtime::find(o.runtime.as_deref()).ok();
        let checks = fleet::check(rt.as_ref());
        let mut out = std::io::stdout().lock();
        for c in &checks {
            print_check(&mut out, c);
        }
        return Ok(i32::from(checks.iter().any(|c| !c.ok())));
    }
    let rt = Runtime::find(o.runtime.as_deref())?;
    match cmd {
        "up" => up(&rt, name, o),
        "ls" => {
            ls(&rt)?;
            Ok(0)
        }
        "down" => {
            fleet::down(&rt, &name)?;
            println!("panoptes: fleet {name} is down; its results stay on the host");
            Ok(0)
        }
        "top" => {
            let status = fleet::top(&rt, &name)?
                .status()
                .map_err(|e| format!("{}: {e}", rt.program.display()))?;
            Ok(status.code().unwrap_or(1))
        }
        _ => unreachable!("main matched {cmd}"),
    }
}

fn up(rt: &Runtime, name: String, o: Opts) -> Result<i32, String> {
    let tasks = o.tasks.ok_or("up needs --tasks FILE")?;
    let tasks = std::fs::canonicalize(&tasks).map_err(|e| format!("{tasks}: {e}"))?;
    if !tasks.is_file() {
        return Err(format!("{}: not a file", tasks.display()));
    }
    let results = o.results.unwrap_or_else(|| "results".into());
    let results = std::path::absolute(&results).map_err(|e| format!("{results}: {e}"))?;
    let config = match &o.config {
        None => serde_json::json!({}),
        Some(f) => {
            let text = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))?
        }
    };
    let key = o.key_file.as_deref().map(read_key).transpose()?;
    let spec = Up {
        name,
        image: o.image.unwrap_or_else(|| fleet::DEFAULT_IMAGE.into()),
        tasks,
        results,
        config,
        base_url: o.base_url,
        model: o.model,
        key,
    };

    if o.dry_run {
        let plan = fleet::plan(&spec)?;
        let rt = shell_word(&rt.program.to_string_lossy());
        let line = |args: &[String]| {
            let words: Vec<String> = args.iter().map(|a| shell_word(a)).collect();
            format!("{rt} {}", words.join(" "))
        };
        println!("{}", line(&plan.create));
        let key = if spec.key.is_some() { " and key" } else { "" };
        println!(
            "{}    # stdin: a tar of panoptes/loop.json{key}, root only",
            line(&plan.copy)
        );
        println!("{}", line(&plan.start));
        println!("# /etc/panoptes/loop.json:");
        println!(
            "{}",
            serde_json::to_string_pretty(&plan.config).expect("a Value serializes")
        );
        return Ok(0);
    }

    let checks = fleet::check(Some(rt));
    let failed: Vec<&Check> = checks.iter().filter(|c| !c.ok()).collect();
    let mut err = std::io::stderr().lock();
    for c in &failed {
        print_check(&mut err, c);
    }
    if !failed.is_empty() {
        if !o.ignore_checks {
            let _ = writeln!(
                err,
                "panoptes: {} of {} host checks failed; run the fixes above, or pass \
                 --ignore-checks",
                failed.len(),
                checks.len()
            );
            return Ok(1);
        }
        let _ = writeln!(err, "panoptes: starting anyway (--ignore-checks)");
    }
    drop(err);

    fleet::up(rt, &spec)?;
    println!(
        "panoptes: fleet {} started in container {}; results in {}",
        spec.name,
        fleet::container(&spec.name),
        spec.results.join(&spec.name).display()
    );
    println!(
        "panoptes: follow it with: {} logs -f {}",
        rt.program.display(),
        fleet::container(&spec.name)
    );
    Ok(0)
}

fn ls(rt: &Runtime) -> Result<(), String> {
    let rows: Vec<[String; 4]> = fleet::ls(rt)?
        .into_iter()
        .map(|f| {
            let state = match f.exit_code {
                Some(code) => format!("{} ({code})", f.state),
                None => f.state,
            };
            let results = f.results.join(&f.name).display().to_string();
            [f.name, state, f.image, results]
        })
        .collect();
    let header = ["NAME", "STATE", "IMAGE", "RESULTS"].map(String::from);
    let mut width = [0; 4];
    for r in std::iter::once(&header).chain(&rows) {
        for (w, cell) in width.iter_mut().zip(r) {
            *w = (*w).max(cell.len());
        }
    }
    for r in std::iter::once(&header).chain(&rows) {
        let [a, b, c, d] = r;
        let [w0, w1, w2, _] = width;
        println!("{a:w0$}  {b:w1$}  {c:w2$}  {d}");
    }
    Ok(())
}

fn print_check(out: &mut impl Write, c: &Check) {
    let _ = match &c.fix {
        None => writeln!(out, "ok    {}: {}", c.name, c.found),
        Some(fix) => {
            let mut text = format!("FAIL  {}: {}\n      {}\n", c.name, c.found, c.need);
            for l in fix.lines() {
                text += &format!("      {l}\n");
            }
            write!(out, "{text}")
        }
    };
}

/// The key from a file, or from stdin for `-`; a key typed at a terminal
/// would echo, so stdin must be a pipe or a file.
fn read_key(path: &str) -> Result<Key, String> {
    let bytes = if path == "-" {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Err("--key-file -: pipe the key in, as in: --key-file - < KEYFILE".into());
        }
        let mut b = Vec::new();
        stdin
            .read_to_end(&mut b)
            .map_err(|e| format!("stdin: {e}"))?;
        b
    } else {
        std::fs::read(PathBuf::from(path)).map_err(|e| format!("{path}: {e}"))?
    };
    Key::new(bytes).map_err(|e| format!("--key-file {path}: {e}"))
}

/// `s` as one shell word, quoted only when it needs it.
fn shell_word(s: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-./:=,@%+".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
