//! Host checks before a fleet starts. Each failure carries the commands that
//! fix it, written for Ubuntu where they differ by distribution. The image's
//! entry script (nix/entry.sh) warns about the same settings from inside the
//! container, once it is too late to fix them.

use super::{Kind, Runtime};
use std::path::Path;

/// One host requirement. A failing check always carries its fix.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    /// What the host has, such as `6.12.58-gentoo-dist` or `off`.
    pub found: String,
    /// What needs it, and what happens without it.
    pub need: &'static str,
    /// Shell lines that fix it, `#` lines explaining; `None` when it passes.
    pub fix: Option<String>,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.fix.is_none()
    }
}

/// panoptes-resd evicts parked desktops once free swap falls below this
/// (its `--swap-low-mib` default, crates/panoptes-resd/src/policy.rs:130).
const SWAP_FLOOR_KIB: u64 = 4 << 20;
/// Swap slots a parked desktop holds (docs/design/decisions.md:26).
const PARKED_KIB: u64 = 180 << 10;

/// Check this host for `rt`, or for no runtime when none was found.
pub fn check(rt: Option<&Runtime>) -> Vec<Check> {
    judge(&Host::read(), rt)
}

/// The host's answers, as read; `None` where the file is missing.
#[derive(Debug, Clone)]
struct Host {
    cgroup_v2: bool,
    kernel: String,
    zswap_enabled: Option<String>,
    zswap_zpool: Option<String>,
    zswap_compressor: Option<String>,
    swap_total_kib: u64,
    userns_restricted: Option<String>,
    euid: u32,
}

impl Host {
    fn read() -> Host {
        let read = |p: &str| std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned());
        let z = "/sys/module/zswap/parameters";
        let swap = read("/proc/meminfo").and_then(|m| {
            m.lines()
                .find_map(|l| l.strip_prefix("SwapTotal:"))?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        });
        Host {
            cgroup_v2: Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
            kernel: read("/proc/sys/kernel/osrelease").unwrap_or_default(),
            zswap_enabled: read(&format!("{z}/enabled")),
            zswap_zpool: read(&format!("{z}/zpool")),
            zswap_compressor: read(&format!("{z}/compressor")),
            swap_total_kib: swap.unwrap_or(0),
            userns_restricted: read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns"),
            euid: rustix::process::geteuid().as_raw(),
        }
    }
}

fn judge(h: &Host, rt: Option<&Runtime>) -> Vec<Check> {
    let mut out = Vec::new();

    out.push(Check {
        name: "runtime",
        found: rt.map_or("neither podman nor docker on PATH".into(), |rt| {
            rt.program.display().to_string()
        }),
        need: "Panoptes runs as a container.",
        fix: rt.is_none().then(|| "sudo apt install podman".into()),
    });

    if rt.is_some_and(|rt| rt.kind == Kind::Podman) {
        let root = h.euid == 0;
        out.push(Check {
            name: "root",
            found: if root {
                "root".into()
            } else {
                format!("uid {}", h.euid)
            },
            need: "The fleet's container is rootful: panoptes-resd owns its cgroup \
                   subtree. Rootless Podman is untested.",
            fix: (!root).then(|| "# run panoptes as root, as in:\nsudo panoptes up ...".into()),
        });
    }

    out.push(Check {
        name: "cgroup v2",
        found: if h.cgroup_v2 {
            "mounted"
        } else {
            "not mounted at /sys/fs/cgroup"
        }
        .into(),
        need: "panoptes-resd freezes and parks desktops through cgroup v2; the \
               container will not start without it.",
        fix: (!h.cgroup_v2).then(|| {
            format!(
                "{}\nsudo reboot",
                on_cmdline("systemd.unified_cgroup_hierarchy=1")
            )
        }),
    });

    let kernel_ok = kernel_at_least(&h.kernel, (6, 8));
    out.push(Check {
        name: "kernel",
        found: h.kernel.clone(),
        need: "Parking turns off zswap writeback per cgroup, which came in Linux 6.8; \
               before it, parking fails.",
        fix: (!kernel_ok).then(|| {
            "# install Linux 6.8 or later; on Ubuntu 22.04:\n\
             sudo apt install --install-recommends linux-generic-hwe-22.04\n\
             sudo reboot"
                .into()
        }),
    });

    let Some(enabled) = &h.zswap_enabled else {
        out.push(Check {
            name: "zswap",
            found: "not in this kernel".into(),
            need: "Parking compresses idle desktops into zswap.",
            fix: Some("# use a kernel built with CONFIG_ZSWAP=y, as Ubuntu's is".into()),
        });
        return finish(out, h);
    };
    let on = enabled == "Y";
    out.push(Check {
        name: "zswap",
        found: if on { "on" } else { "off" }.into(),
        need: "Parking compresses idle desktops into zswap. Without it, parked \
               desktops go to the swap device, or keep their memory when there is \
               no swap.",
        fix: (!on).then(|| param("enabled", "Y", "zswap.enabled=1")),
    });

    // Linux 6.18 dropped the other allocators, and with them the choice.
    let zpool = h.zswap_zpool.as_deref().unwrap_or("zsmalloc");
    out.push(Check {
        name: "zswap zpool",
        found: zpool.into(),
        need: "zbud stores at most 2 compressed pages per page: a parked desktop \
               charged 69 MiB grew zbud's pool by 112, zsmalloc's by 2 to 3 more \
               than its charge (docs/design/kernel-and-inference.md).",
        fix: (zpool != "zsmalloc").then(|| param("zpool", "zsmalloc", "zswap.zpool=zsmalloc")),
    });

    let compressor = h.zswap_compressor.as_deref().unwrap_or("none");
    out.push(Check {
        name: "zswap compressor",
        found: compressor.into(),
        need: "zstd parks a desktop in about a quarter of its memory, lz4 in about \
               a third (docs/design/kernel-and-inference.md).",
        fix: (compressor != "zstd").then(|| param("compressor", "zstd", "zswap.compressor=zstd")),
    });

    finish(out, h)
}

/// The checks after zswap's, which hold whether or not the kernel has it.
fn finish(mut out: Vec<Check>, h: &Host) -> Vec<Check> {
    let swap_ok = h.swap_total_kib > SWAP_FLOOR_KIB;
    out.push(Check {
        name: "swap",
        found: match h.swap_total_kib {
            0 => "none".into(),
            kib => format!(
                "{:.1} GiB, room for about {} parked desktops",
                kib as f64 / (1 << 20) as f64,
                kib.saturating_sub(SWAP_FLOOR_KIB) / PARKED_KIB
            ),
        },
        need: "A parked desktop holds about 180 MiB of swap slots, even with \
               writeback off, and panoptes-resd evicts parked desktops once free \
               swap falls under 4 GiB.",
        fix: (!swap_ok).then(|| {
            "# 64 GiB holds about 340 parked desktops; size it for your fleet\n\
             sudo fallocate -l 64G /swapfile.panoptes\n\
             sudo chmod 600 /swapfile.panoptes\n\
             sudo mkswap /swapfile.panoptes\n\
             sudo swapon /swapfile.panoptes\n\
             # keeps it across reboots:\n\
             echo '/swapfile.panoptes none swap sw 0 0' | sudo tee -a /etc/fstab"
                .into()
        }),
    });

    // NOTE: Inferred, not yet seen on a host: Chromium's sandbox makes a
    //       user namespace, and Ubuntu 23.10 and later deny one to
    //       unconfined programs (Chromium's
    //       docs/security/apparmor-userns-restrictions.md). A privileged
    //       container runs unconfined, and the desks run as uid 1000, not
    //       root. The Ubuntu 24.04 run confirms or drops this.
    let restricted = h.userns_restricted.as_deref() == Some("1");
    out.push(Check {
        name: "user namespaces",
        found: if restricted { "restricted by AppArmor" } else { "allowed" }.into(),
        need: "Chromium's sandbox makes a user namespace; without one, desks exit \
               with \"No usable sandbox\".",
        fix: restricted.then(|| {
            "sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0\n\
             # keeps it across reboots:\n\
             echo kernel.apparmor_restrict_unprivileged_userns=0 | sudo tee /etc/sysctl.d/60-panoptes.conf"
                .into()
        }),
    });
    out
}

/// Fix a zswap parameter now, and keep it with a kernel parameter.
fn param(name: &str, value: &str, boot: &str) -> String {
    format!(
        "echo {value} | sudo tee /sys/module/zswap/parameters/{name}\n{}",
        on_cmdline(boot)
    )
}

/// Add `p` to the kernel command line on GRUB hosts such as Ubuntu.
fn on_cmdline(p: &str) -> String {
    format!(
        "# keeps it across reboots, on GRUB hosts such as Ubuntu:\n\
         sudo sed -i 's/^GRUB_CMDLINE_LINUX=\"/&{p} /' /etc/default/grub\n\
         sudo update-grub"
    )
}

/// `release` is `uname -r`: `6.8.0-45-generic`, `6.12.58-gentoo-dist`.
fn kernel_at_least(release: &str, want: (u32, u32)) -> bool {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let mut next = || parts.next().and_then(|p| p.parse::<u32>().ok());
    match (next(), next()) {
        (Some(major), Some(minor)) => (major, minor) >= want,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn podman() -> Runtime {
        Runtime {
            program: "/usr/bin/podman".into(),
            kind: Kind::Podman,
        }
    }

    /// The test host, run as root: Gentoo 6.12, zswap tuned, 264 GiB swap.
    fn tuned() -> Host {
        Host {
            cgroup_v2: true,
            kernel: "6.12.58-gentoo-dist".into(),
            zswap_enabled: Some("Y".into()),
            zswap_zpool: Some("zsmalloc".into()),
            zswap_compressor: Some("zstd".into()),
            swap_total_kib: 264 << 20,
            userns_restricted: None,
            euid: 0,
        }
    }

    fn failing(h: &Host, rt: Option<&Runtime>) -> Vec<&'static str> {
        let checks = judge(h, rt);
        for c in &checks {
            assert_eq!(c.ok(), c.fix.is_none());
        }
        checks.iter().filter(|c| !c.ok()).map(|c| c.name).collect()
    }

    #[test]
    fn a_tuned_host_passes() {
        assert_eq!(failing(&tuned(), Some(&podman())), Vec::<&str>::new());
    }

    /// Ubuntu 24.04 as a cloud image comes up, by its kernel config and
    /// defaults: zswap off with the build's default zpool and compressor,
    /// no swap, AppArmor restricting user namespaces. Not yet read off a VM.
    #[test]
    fn stock_ubuntu_fails_what_parking_needs() {
        let ubuntu = Host {
            kernel: "6.8.0-45-generic".into(),
            zswap_enabled: Some("N".into()),
            zswap_zpool: Some("zbud".into()),
            zswap_compressor: Some("lzo".into()),
            swap_total_kib: 0,
            userns_restricted: Some("1".into()),
            euid: 1000,
            ..tuned()
        };
        assert_eq!(
            failing(&ubuntu, Some(&podman())),
            [
                "root",
                "zswap",
                "zswap zpool",
                "zswap compressor",
                "swap",
                "user namespaces"
            ]
        );
        assert_eq!(failing(&ubuntu, None)[0], "runtime");
    }

    #[test]
    fn kernel_versions() {
        for (release, ok) in [
            ("6.8.0-45-generic", true),
            ("6.12.58-gentoo-dist", true),
            ("7.0.1", true),
            ("6.7.12-arch1-1", false),
            ("5.15.0-122-generic", false),
            ("", false),
        ] {
            assert_eq!(kernel_at_least(release, (6, 8)), ok, "{release}");
        }
    }

    #[test]
    fn swap_must_clear_resds_floor() {
        let with = |kib| Host {
            swap_total_kib: kib,
            ..tuned()
        };
        assert_eq!(failing(&with(4 << 20), None), ["runtime", "swap"]);
        let c = judge(&with(8 << 20), None);
        let swap = c.iter().find(|c| c.name == "swap").unwrap();
        assert!(swap.ok());
        assert_eq!(swap.found, "8.0 GiB, room for about 22 parked desktops");
    }

    #[test]
    fn without_zswap_its_settings_are_not_judged() {
        let h = Host {
            zswap_enabled: None,
            zswap_zpool: None,
            zswap_compressor: None,
            ..tuned()
        };
        assert_eq!(failing(&h, Some(&podman())), ["zswap"]);
    }
}
