# Panoptes design notes

These notes come from one research and test run on 2026-10-26, on a
32-thread, 125 GiB server with an RTX 3090. They cover each agent's
desktop, what an agent container is and what schedules it, and where
kernel and inference work pays off. They describe the earlier desk, which
drove Chromium over CDP; since 2026-10-30 the desk drives any program over
Wayland ([decisions.md](decisions.md)).

| Doc | Question | Answer |
|---|---|---|
| [desktop.md](desktop.md) | Which compositor, browser, and screen size, and how agents drive them | One Cage per agent, a trimmed Chromium frozen between steps, 1280x720 at scale 1, and panoptes-desk serving its tools over MCP. Superseded in part on 2026-10-30: any program, 1024x768, driven over Wayland |
| [scheduler.md](scheduler.md) | What an agent container is, and what schedules it | A cgroup tree. A residency manager on each host parks idle kiosks; Kubernetes cannot. Historical: the unit layout is not what Panoptes builds |
| [kernel-and-inference.md](kernel-and-inference.md) | Where kernel and inference work pays | Memory parking pays, network work does not, and the GPU sets local density. Historical: written while a model step took seconds; see the section below |
| [decisions.md](decisions.md) | What was decided on 2026-10-27, 2026-10-30 and 2026-10-31 | Panoptes: an AGPL engine in a Docker image built with Nix, its own agent loop, and MCP for other harnesses. Since 2026-10-30, a harness for any Linux GUI program on a Cage fork at 1024x768. Since 2026-10-31, std threads, with tokio only for network libraries |
| [verification.md](verification.md) | How to write a subsystem so that its checks can prove it | Split it into a pure policy, threads, and I/O. Check the policy exhaustively, the threads with loom, and the I/O in NixOS VMs |
| [verification-rationale.md](verification-rationale.md) | Why each verification rule exists | The fact each rule rests on, with its date; the tools left out; what would change the rules |
| [measurements.md](measurements.md) | How each number was taken | Machine, versions, methods, caveats |
| [vpn.md](vpn.md) | How agents' traffic goes through VPN tunnels | Proposed: one network namespace per WireGuard tunnel, agents spread across tunnels by default, with a limit per tunnel and a pin per task |

## Since 2026-10-31, the harness is the cost

These notes were measured with Chromium, while a model step took seconds,
and several findings below lean on that. Since 2026-10-31,
[vision.md](../vision.md) sets the rule: treat a model call as free and
instant, and count harness time per step as the cost. The harness's median
step on the earlier desk was 660 ms, with a text model reading Chromium's
accessibility tree.

| Finding | Under that rule |
|---|---|
| 1. The browser is what an agent costs | True of Chromium. Panoptes now runs any program; other programs were measured only in stock Cage (E14), not on the Wayland desk |
| 2. Chromium flags | Applies only when the agent's program is Chromium |
| 3. About 700 running agents | Arithmetic from 64 agents (E1c), and a residency number. Throughput is the measure; see vision.md |
| 4. Parking | Still pays, for waits that are not the model: a person, a slow backend, a quota. Waking costs 40 to 75 ms, 6 to 11 percent of a 660 ms step |
| 9. The network | It compared against a model step of seconds. A new 22 to 51 ms TLS connection is 3 to 8 percent of a 660 ms step, so reusing connections matters more |
| 10. The GPU | It describes one local model on one GPU: how fast that model runs, not what limits Panoptes |
| 12. Freezing between steps | It assumed seconds between steps. With a fast model little gap is left to save CPU in, and a frozen kiosk's next capture took 9 to 16 ms longer |
| 13. A shared browser | Applies only to agents that all use one browser |

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
   CPU.
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
    Codex each hold about 85 MiB of their own, half a lean kiosk.
12. Page JavaScript ignores the refresh rate, so freeze each kiosk between
    its agent's steps. At 2 Hz, CNN still used 1.8 cores per agent between
    steps; frozen, it used a fifth of one, and the next capture took 9 to
    16 ms longer. Hiding the page helps busy pages as much, but costs
    settled ones a re-raster.
13. A shared browser saves memory only while agents run: about 105 MiB per
    agent against a lean kiosk's 177. Parking one agent inside it freed only
    33 to 70 MiB, because only memory charged to the agent's own cgroup can
    be reclaimed. A parked agent kept 40 to 75 MiB, against a parked lean
    kiosk's 33 to 46.

## Not measured

- Agent task success at 720p against 1080p is unmeasured; every resolution
  claim here is about cost.
- Freezes between steps lasted about 4 s and parks 20 s; one 180 s freeze
  inside a shared browser resumed cleanly. Whether a page's live
  connections and login sessions survive minutes or hours is untested.
- The bench scripts are in `bench/` and their raw results in
  `bench/results/`; most still assume the test server's paths.
