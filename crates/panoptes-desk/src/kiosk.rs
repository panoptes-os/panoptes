//! Start one agent's kiosk: Cage running Chromium, charged to the agent's
//! `kiosk/` cgroup from its first page.
//!
//! ```text
//! <agent cgroup>
//! ├─ desk/    panoptes-desk; never frozen, so it can thaw the kiosk
//! └─ kiosk/   cage -> sh (sets the output mode) -> chromium
//! ```

use panoptes_cdp::Cdp;
use panoptes_cgroup::{Cgroup, Child};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How the desk reaches Chromium's DevTools.
#[derive(Debug, Clone, Copy)]
pub enum CdpMode {
    /// `--remote-debugging-pipe` on fds 3 and 4; nothing listens anywhere.
    Pipe,
    /// `--remote-debugging-port` on loopback, for when a pipe cannot reach
    /// the browser.
    Port(u16),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub run_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub url: String,
    pub cdp: CdpMode,
    pub extra_flags: Vec<String>,
}

#[derive(Debug)]
pub struct Kiosk {
    pub cdp: Cdp,
    pub cgroup: Cgroup,
    pub downloads: PathBuf,
    child: Option<Child>,
}

/// Chromium flags of the bench's "proc" set (E1b, E10): background services
/// off, GPU and network service in the browser process, no spare renderer.
const LEAN: &[&str] = &[
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-sync",
    "--disable-extensions",
    "--disable-default-apps",
    "--disable-breakpad",
    "--disable-domain-reliability",
    "--disable-client-side-phishing-detection",
    "--disable-component-extensions-with-background-pages",
    "--metrics-recording-only",
    "--no-pings",
    "--mute-audio",
    "--in-process-gpu",
    "--disable-features=Translate,OptimizationHints,MediaRouter,DialMediaRouteProvider,\
CertificateTransparencyComponentUpdater,AutofillServerCommunication,LensOverlay,\
SpareRendererForSitePerProcess",
    "--enable-features=NetworkServiceInProcess2,NetworkServiceInProcess",
];

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

fn write_prefs(profile: &Path, downloads: &Path) -> std::io::Result<()> {
    let default = profile.join("Default");
    std::fs::create_dir_all(&default)?;
    let prefs = serde_json::json!({
        "plugins": { "always_open_pdf_externally": true },
        "download": { "prompt_for_download": false, "default_directory": downloads },
        "profile": { "exit_type": "Normal" },
    });
    std::fs::write(default.join("Preferences"), prefs.to_string())
}

pub async fn launch(kiosk_cg: Cgroup, cfg: &Config) -> Result<Kiosk, String> {
    let xdg = cfg.run_dir.join("xdg");
    let profile = cfg.run_dir.join("profile");
    let downloads = cfg.run_dir.join("downloads");
    for d in [&xdg, &profile, &downloads] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&xdg, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", xdg.display()))?;
    }
    write_prefs(&profile, &downloads).map_err(|e| format!("preferences: {e}"))?;

    let mut argv: Vec<String> = vec![
        "cage".into(),
        "--".into(),
        "sh".into(),
        "-c".into(),
        // Cage sets WAYLAND_DISPLAY for its client; set the headless
        // output's mode before the browser maps its window.
        r#"wlr-randr --output HEADLESS-1 --custom-mode "$PANOPTES_MODE" >/dev/null 2>&1; exec "$@""#
            .into(),
        "panoptes-kiosk".into(),
        "chromium".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--password-store=basic".into(),
        format!("--user-data-dir={}", profile.display()),
        "--disable-gpu".into(),
        "--ozone-platform=wayland".into(),
    ];
    argv.extend(LEAN.iter().map(|s| s.to_string()));
    argv.extend(cfg.extra_flags.iter().cloned());
    match cfg.cdp {
        CdpMode::Pipe => argv.push("--remote-debugging-pipe".into()),
        CdpMode::Port(p) => {
            argv.push(format!("--remote-debugging-port={p}"));
            argv.push("--remote-debugging-address=127.0.0.1".into());
        }
    }
    // --kiosk drops the client-side title bar --app alone draws on Wayland.
    argv.push("--kiosk".into());
    argv.push(format!("--app={}", cfg.url));

    let mode = format!("{}x{}@60Hz", cfg.width, cfg.height);
    let xdg_s = xdg.display().to_string();
    let home = cfg.run_dir.display().to_string();
    let env = [
        ("XDG_RUNTIME_DIR", xdg_s.as_str()),
        ("HOME", home.as_str()),
        ("WLR_BACKENDS", "headless"),
        ("WLR_RENDERER", "pixman"),
        ("WLR_LIBINPUT_NO_DEVICES", "1"),
        ("PANOPTES_MODE", mode.as_str()),
    ];

    let (cdp, child) = match cfg.cdp {
        CdpMode::Pipe => {
            let (cdp, ends) = Cdp::pipe().map_err(|e| e.to_string())?;
            let child = kiosk_cg
                .spawn_with_fds(&argv, &env, &[(ends.read.as_fd(), 3), (ends.write.as_fd(), 4)])
                .map_err(|e| format!("spawn cage: {e}"))?;
            drop(ends);
            (cdp, child)
        }
        CdpMode::Port(p) => {
            let child = kiosk_cg
                .spawn(&argv, &env)
                .map_err(|e| format!("spawn cage: {e}"))?;
            let cdp = Cdp::port(p, Duration::from_secs(20))
                .await
                .map_err(|e| e.to_string())?;
            (cdp, child)
        }
    };
    let product = cdp.version().await.map_err(|e| format!("browser never answered: {e}"))?;
    eprintln!("panoptes-desk: {product} up in {}", kiosk_cg.path().display());
    Ok(Kiosk {
        cdp,
        cgroup: kiosk_cg,
        downloads,
        child: Some(child),
    })
}

impl Kiosk {
    /// Close the browser, then kill whatever is left in the kiosk group.
    pub async fn shutdown(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(3), self.cdp.close_browser()).await;
        let _ = self.cgroup.thaw(Duration::from_secs(1));
        let _ = self.cgroup.kill_all();
        let _ = self.cgroup.wait_empty(Duration::from_secs(5));
        if let Some(child) = self.child.take() {
            let _ = tokio::task::spawn_blocking(move || child.wait()).await;
        }
    }
}
