# Panoptes design: the agent-native distro

Status: proposed, 2026-10-26. These docs come from one research and test
run on a 32-thread, 125 GiB server with an RTX 3090. They answer four
questions: what base OS to ship, what each agent's desktop should be, what
an agent container is and what schedules it, and where kernel and
inference work pays off.

| Doc | Question | Answer |
|---|---|---|
| [base-os.md](base-os.md) | Will enterprises adopt NixOS? Should containers use a lighter base? | Yes, as an appliance whose lifecycle Panoptes owns. Containers get light by sharing pages, not from a smaller distro |
| [desktop.md](desktop.md) | Which compositor, browser, and screen size, and how agents drive them | One Cage per agent, a trimmed Chromium frozen between steps, 1280x720 at scale 1, and panoptes-desk serving the providers' toolsets |
| [scheduler.md](scheduler.md) | What an agent container is, and what schedules it | A systemd cgroup tree. A residency manager on each host parks idle kiosks; Kubernetes cannot |
| [kernel-and-inference.md](kernel-and-inference.md) | Where kernel and inference work pays | Memory parking pays, network work does not, 6.12 is the kernel, and the GPU sets local density |
| [decisions.md](decisions.md) | What was decided on 2026-10-27, and what supersedes what | Panoptes: an AGPL engine in a Docker image, a closed control plane, individuals first, VPN routing on day 1 |
| [build-plan.md](build-plan.md) | What to build first, in what order, and how each step passes | A Rust workspace; six milestones from cgroup library to a 24 h pilot soak, each gated by a bench number |
| [nt-on-linux.md](nt-on-linux.md) | Can real Windows DLLs run on Linux without a VM? | Maybe; a one to two week probe after the pitch decides |
| [measurements.md](measurements.md) | How each number was taken | Machine, versions, methods, caveats |

## Findings

1. The browser is what an agent costs. Cage holds 10 MiB per agent;
   Chromium holds about 250. A full XFCE desktop would add 95 MiB per
   agent.
2. Flags and one memory-pressure signal cut Chromium's charge by 32 to 43
   percent. Low-end mode, lite mode, and disabling site isolation add 4 MiB
   or less on top.
3. Agents scale linearly. With lean flags, 64 agents cost about 175 MiB
   each, and an idle one uses 0.05 percent of a core. 125 GiB holds about
   700 running agents before any parking.
4. Parking is the kernel lever that pays. Frozen and compressed into zswap
   with zstd, an idle kiosk drops from about 270 MiB to 54 to 64 MiB, or
   from about 200 to 33 to 46 with lean flags. That is ten times what KSM
   saves, and the kiosk takes its first screenshot 40 to 75 ms after thaw.
   zswap needs writeback off and a swap slot for every parked page.
5. Kubernetes cannot pause, compress, or restore a pod, so it cannot park
   an agent. The lifecycle belongs in a per-host residency manager, as at
   AWS Lambda, E2B, and Unikraft.
6. The container mechanism costs nothing next to the browser: 0.4 to 2.9
   MiB per instance with systemd, crun, runc, bwrap, nspawn, or Podman.
   Docker costs 5.9 MiB, for its per-container shim. Docker and Podman
   take 100 to 150 ms to start a container, crun and systemd about 10.
7. Weights protect an acting agent; caps do not. With 64 busy siblings on
   32 threads, equal weights slowed a click and screenshot by half at p50.
   A weight of 10000 restored the idle host's latency; half-CPU caps did
   not help.
8. KSM saved 8 percent across 64 agents of one build, for 108 s of ksmd
   CPU. It is a second step after parking, inside one credential domain.
9. The network is not where agent time goes. A new TLS connection to a
   model API took 22 to 51 ms, and a page load moved 0.7 to 1.7 MB, against
   a model step of seconds. Kernel fast paths save microseconds;
   connection reuse saves the milliseconds.
10. The GPU, not memory, limits how many agents a host keeps busy. One
    RTX 3090 running UI-TARS-1.5-7B served about 1.7 screenshot steps per
    second: 8 agents at a 4.6 s median step, 16 at 8.9 s. FP8 weights
    tripled the KV cache, which sets the agent count, and prefix caching
    tripled throughput. gpt-oss-20b on accessibility-tree text peaked at
    the same 1.75 steps/s, and halved once its cache overflowed.
11. The agent loop is not free. Idle at their first screen, Claude Code and
    Codex each hold about 85 MiB of their own, half a lean kiosk. Budget,
    park, and share loops like kiosks.
12. kernel.org ends both current LTS kernels in December 2028. Only CIP's
    6.12 meets a five-year support promise.
13. Page JavaScript ignores the refresh rate, so freeze each kiosk between
    its agent's steps. At 2 Hz, CNN still used 1.8 cores per agent between
    steps; frozen, it used a fifth of one, and the next capture took 9 to
    16 ms longer. Hiding the page helps busy pages as much, but costs
    settled ones a re-raster.
14. A shared browser saves memory only while agents run: about 105 MiB per
    agent against a lean kiosk's 177. Parking one agent inside it freed only
    33 to 70 MiB, because only memory charged to the agent's own cgroup can
    be reclaimed. A parked agent kept 40 to 75 MiB, against a parked lean
    kiosk's 33 to 46.

## What is not settled

- The threat model decides the isolation tier: units on the host kernel
  for one credential domain, or one microVM per unit for untrusted work.
- Agent task success at 720p against 1080p is unmeasured; every resolution
  claim here is about cost.
- Freezes between steps lasted about 4 s and parks 20 s; one 180 s freeze
  inside a shared browser resumed cleanly. Whether a page's live
  connections and login sessions survive minutes or hours is untested.
- Parking the agent loop with its kiosk is proposed, not measured.
- The bench scripts are in `bench/` and their raw results in
  `bench/results/`; most still assume the test server's paths.
