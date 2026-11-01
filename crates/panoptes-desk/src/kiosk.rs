//! One agent's kiosk: the Cage fork with no client of its own, charged to
//! the agent's `kiosk/` cgroup, and every app the agent launches into it.
//!
//! ```text
//! <agent cgroup>
//! ├─ desk/    panoptes-desk; never frozen, so it can thaw the kiosk
//! └─ kiosk/   cage -r WxH, Xwayland, and the launched apps
//! ```
//!
//! The desk thread owns the [`Kiosk`] and launches apps; `main` keeps the
//! [`Cage`] to shut everything down.

use panoptes_cgroup::{Cgroup, Child};
use std::fs::File;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Config {
    pub run_dir: PathBuf,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct Kiosk {
    pub cgroup: Cgroup,
    /// The compositor's Wayland socket.
    pub socket: PathBuf,
    /// What every launched app gets on top of the desk's environment.
    env: Vec<(String, String)>,
    /// Each launched app's stdout and stderr, as `<n>-<program>.log`.
    logs: PathBuf,
    launched: u32,
}

/// Cage itself, for shutting the kiosk down.
#[derive(Debug)]
pub struct Cage {
    cgroup: Cgroup,
    child: Child,
}

/// How long Cage may take to open its socket.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Move this process into `<agent>/desk` and create `<agent>/kiosk`, with
/// the agent's controllers handed down. Call before starting any threads
/// that should stay with the desk; cgroup v2 forbids processes in a group
/// that enables controllers for its children.
pub fn prepare_cgroups(agent: &Cgroup) -> panoptes_cgroup::Result<Cgroup> {
    let desk = agent.child("desk")?;
    desk.attach(std::process::id())?;
    let have = agent.controllers()?;
    let want: Vec<&str> = ["cpu", "memory", "pids"]
        .into_iter()
        .filter(|c| have.iter().any(|h| h == c))
        .collect();
    agent.enable_controllers(&want)?;
    agent.child("kiosk")
}

/// Start Cage at the configured size in the kiosk group and wait for its
/// socket.
pub fn start(cgroup: Cgroup, cfg: &Config) -> Result<(Kiosk, Cage), String> {
    let xdg = cfg.run_dir.join("xdg");
    let logs = cfg.run_dir.join("logs");
    // A desk that ran here before leaves its Wayland socket, which refuses
    // connections and would pass for this Cage's.
    let _ = std::fs::remove_dir_all(&xdg);
    for d in [&xdg, &logs] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&xdg, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", xdg.display()))?;
    }
    let xdg_s = xdg.display().to_string();
    let home = cfg.run_dir.display().to_string();
    let size = format!("{}x{}", cfg.width, cfg.height);
    let env = [
        ("XDG_RUNTIME_DIR", xdg_s.as_str()),
        ("HOME", home.as_str()),
        ("WLR_BACKENDS", "headless"),
        ("WLR_RENDERER", "pixman"),
        ("WLR_LIBINPUT_NO_DEVICES", "1"),
    ];
    // Cage logs to stderr, which it shares with the desk. Its stdout goes
    // there too: with --mcp-stdio the desk's stdout is the MCP stream.
    let null = File::open("/dev/null").map_err(|e| format!("/dev/null: {e}"))?;
    let stderr = std::io::stderr();
    let child = cgroup
        .spawn_with_fds(
            &["cage", "-r", size.as_str()],
            &env,
            &[(null.as_fd(), 0), (stderr.as_fd(), 1)],
        )
        .map_err(|e| format!("spawn cage: {e}"))?;

    let deadline = Instant::now() + START_TIMEOUT;
    let socket = loop {
        if let Some(s) = wayland_socket(&xdg) {
            break s;
        }
        if Instant::now() > deadline {
            return Err(format!(
                "cage opened no Wayland socket in {} within {START_TIMEOUT:?}",
                xdg.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let display = socket
        .file_name()
        .expect("socket has a name")
        .to_string_lossy()
        .into_owned();
    eprintln!(
        "panoptes-desk: cage {size} on {} in {}",
        socket.display(),
        cgroup.path().display()
    );
    let kiosk = Kiosk {
        cgroup: cgroup.clone(),
        socket,
        env: vec![
            ("XDG_RUNTIME_DIR".into(), xdg_s),
            ("HOME".into(), home),
            ("WAYLAND_DISPLAY".into(), display),
        ],
        logs,
        launched: 0,
    };
    Ok((kiosk, Cage { cgroup, child }))
}

/// The `wayland-N` socket in a runtime directory, once one exists.
fn wayland_socket(xdg: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    std::fs::read_dir(xdg).ok()?.flatten().find_map(|e| {
        let name = e.file_name();
        let is_socket = e.file_type().is_ok_and(|t| t.is_socket());
        (is_socket && name.to_string_lossy().starts_with("wayland-")).then(|| e.path())
    })
}

impl Kiosk {
    /// Start `argv` in the kiosk group. It gets the desk's environment, then
    /// the kiosk's runtime directory and displays, then `env`; stdin is
    /// /dev/null, and stdout and stderr go to a fresh log, whose path is
    /// returned.
    pub fn launch(
        &mut self,
        argv: &[String],
        env: &[(String, String)],
        display: Option<&str>,
    ) -> Result<(Child, PathBuf), String> {
        let program = argv.first().ok_or("launch needs a program")?;
        self.launched += 1;
        let n = self.launched;
        let name = Path::new(program)
            .file_name()
            .map_or_else(|| "app".into(), |s| s.to_string_lossy());
        let log_path = self.logs.join(format!("{n}-{name}.log"));
        let log = File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
        let null = File::open("/dev/null").map_err(|e| format!("/dev/null: {e}"))?;

        let mut vars: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        if let Some(d) = display {
            vars.push(("DISPLAY", d));
        }
        for (k, v) in env {
            vars.retain(|(have, _)| *have != k.as_str());
            vars.push((k.as_str(), v.as_str()));
        }
        let child = self
            .cgroup
            .spawn_with_fds(
                argv,
                &vars,
                &[(null.as_fd(), 0), (log.as_fd(), 1), (log.as_fd(), 2)],
            )
            .map_err(|e| format!("launch {program}: {e}"))?;
        Ok((child, log_path))
    }

    /// SIGKILL `pid`, which must be in the kiosk group.
    pub fn kill(&self, pid: u32) -> Result<(), String> {
        let procs = self.cgroup.procs().map_err(|e| e.to_string())?;
        if !procs.contains(&pid) {
            return Err(format!("process {pid} is not in this agent's kiosk"));
        }
        let pid = rustix::process::Pid::from_raw(pid as i32).ok_or("pid 0")?;
        rustix::process::kill_process(pid, rustix::process::Signal::KILL)
            .map_err(|e| format!("kill {pid:?}: {e}"))
    }
}

impl Cage {
    /// Kill everything in the kiosk group, Cage last to be reaped, and
    /// remove the group. A restart must not reuse it: Linux before 6.18.59
    /// kills a clone3(CLONE_INTO_CGROUP) child of a group that has ever had
    /// cgroup.kill.
    pub fn shutdown(self) {
        let _ = self.cgroup.thaw(Duration::from_secs(1));
        let _ = self.cgroup.kill_all();
        let _ = self.cgroup.wait_empty(Duration::from_secs(5));
        let _ = self.child.wait();
        if let Err(e) = self.cgroup.remove() {
            eprintln!("panoptes-desk: {e}");
        }
    }
}
