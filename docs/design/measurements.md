# Measurements: methods

Status: record of runs from 2026-10-26 to 2026-10-28, all on the earlier
desk, which drove Chromium over the Chrome DevTools Protocol. The design
docs, the one-pager and the vision cite these results; this file says how
each number was taken. The first runs predate the name Panoptes, so unit
and path names below keep the old name, glarb.

## Machine

One bare-metal server:

| Part | Value |
|---|---|
| CPU | AMD Ryzen 9 5900XT, 16 cores, 32 threads |
| RAM | 125 GiB, 8 GiB swap partition |
| GPU | NVIDIA RTX 3090 24 GB, driver 580.126.18, 350 W |
| OS | Gentoo, kernel 6.12.58-gentoo-dist, systemd 260.1 |
| Memory settings | zswap on (zbud, lz4, 20% pool), MGLRU on, KSM off, swappiness 60 |
| Kernel config | BPF LSM, PSI, memcg, checkpoint/restore, io_uring; no sched_ext, no BTF |

Software came from the nixpkgs revision that this repository's
`flake.lock` pins (`3ec6363`), except vLLM, which came from PyPI:

| Component | Version |
|---|---|
| Chromium | 156.0.8078.47 |
| Cage, wlroots | 0.3.1, 0.20.2 |
| crun, runc, Podman, Docker | 1.30.1, 1.5.2, 5.8.9, 29.9.1 |
| CRIU | 4.1.1 |
| vLLM, PyTorch | 0.32.0, 2.13.0 with CUDA 13.0 |

## How an agent runs

Each agent is a transient systemd service in `glarb.slice`, started with
`systemd-run -p User=user -p Delegate=yes`, with its own runtime directory
and Chromium profile:

```text
glarb-<id>.service
└─ cage                 WLR_BACKENDS=headless WLR_RENDERER=pixman
   └─ chromium          --ozone-platform=wayland --disable-gpu
                        --user-data-dir=<profile> --remote-debugging-port=N
```

`wlr-randr` sets the headless output's mode before Chromium starts. The
XFCE variant replaces Cage with Xvfb, a private D-Bus session, xfce4-session,
xfwm4, xfce4-panel, xfdesktop, xfsettingsd, and x11vnc. The headless variant
runs Chromium with `--headless=new` and no compositor.

Pages, loaded live over the server's WAN:

| Name | URL |
|---|---|
| blank | `about:blank` |
| Greenhouse | `https://job-boards.greenhouse.io/anthropic` |
| Workday | `https://nvidia.wd5.myworkdayjobs.com/NVIDIAExternalCareerSite` |
| Wikipedia | `https://en.wikipedia.org/wiki/Linux_kernel` |

## Memory has two honest numbers

- `memory.current` is the cgroup's charge: anonymous memory, kernel
  memory, and the file pages this cgroup read first. A page another cgroup
  read first is not charged again. This is the marginal cost of one more
  agent when the browser's files are already cached.
- PSS sums each process's proportional share of every page it maps. With
  one agent running, PSS includes the whole resident browser binary; with N
  agents, each pays about 1/N of it.

Each memory figure was taken 15 to 20 s after the page reported
`document.readyState == "complete"`, with one agent running at a time
unless the experiment says otherwise.

## Experiments

| ID | Question | Method | Reps |
|---|---|---|---|
| E1 | memory per desktop stack | Cage, Cage plus wayvnc, Xvfb plus XFCE, headless; four pages | 3 |
| E1b | memory under lean Chromium flags | cumulative flag sets on Cage; then `Memory.forciblyPurgeJavaScriptMemory` and a critical pressure signal | 2 |
| E1c | memory per agent at density | N = 8, 32, 64 agents at once, pages rotating; the slice's charge, MemAvailable, PSS, and idle CPU; KSM on one 64 run | 1 |
| E1d | one browser holding N agents | N BrowserContexts in one Chromium, against E1c | 1 |
| E2 | what resolution costs | five resolutions; grim and CDP capture time and size; accessibility tree and page text size | 1 |
| E3 | container runtime overhead | 50 instances of one `sleep` per runtime: a sandboxed systemd service, systemd with `RootDirectory`, nspawn, bwrap, crun, runc, Podman, Docker; slice charge and PSS per instance; time for the start command to return | 1 |
| E4 | parking and resume | freeze, reclaim into zswap with writeback on and off, hold 20 s, thaw; time to first CDP call, screenshot, and scroll; zbud and zsmalloc, lz4 and zstd; an oversized reclaim target and one sized to anonymous memory plus shmem; cold relaunch on the same profile; a CRIU dump; the zsmalloc, zstd, writeback-off park again with lean (proc) flags in `--app` mode | 1 to 3 |
| E5 | CPU contention | one acting agent against 64 CPU hogs (stress-ng, one core each) on 32 threads; equal weights, cpu.weight=10000, cpu.idle, cpu.max of half a CPU, siblings frozen | 100 actions |
| E6 | network share of a step | curl's connect, TLS, and first-byte times to three model API hosts; one DevTools HTTP request over loopback; CDP Network events over one load of each page | 10 connections per host |
| E7 | local VLM inference | UI-TARS-1.5-7B on vLLM, `--max-model-len 16384`, `--gpu-memory-utilization 0.92`, up to 8 images per prompt; closed-loop simulated agents at N = 1, 4, 8, 16, six steps each after two warm-up steps, greedy, 64 output tokens; bf16 with and without prefix caching, and FP8 weights; engine-side means from deltas of vLLM's Prometheus histograms, peak KV use sampled each second | 1 |
| E7c | parking the engine | vLLM sleep mode level 1: sleep, GPU memory and MemAvailable while asleep, wake, first request after wake | 1 |
| E8 | refresh rate | 8 agents on an animated page at 60, 30, 10, 2 Hz; slice CPU and screencopy latency | 1 |
| E9 | agent loop memory | Node's floor, Codex 0.163.0, and Claude Code 2.1.308 idle at their first screen, with no credentials | 1 |
| E10 | page work between agent steps | 8 agents on one page, proc flags: a 20 Hz timer page, the E8 spinner, Greenhouse, Workday, CNN. CPU of the agents' units over 30 s: visible at 60, 10, and 2 Hz; hidden by minimizing the window through CDP; hidden by a CDP freeze and resume. Then 60 s of round-robin steps, one per agent about every 4 s (capture, scroll 300 px and wait two frames, 300 ms, capture), with the page visible throughout or shown only for its step. A 10 px capture of an overlay, against visible references, checks that hidden captures are fresh | 1 |
| E10b | idling a page between steps | E10's steps, 60 s per mode: output at 60 Hz throughout, at 2 Hz throughout, at 2 Hz between an agent's steps and 60 Hz during them, and the agent's unit frozen between its steps and thawed for each, in that order, and again in reverse for Workday and Greenhouse. Timings include the connection and page session. A probe of one agent records 20 s each of visible, cycles of 3.5 s frozen and 0.5 s thawed, and frozen, with CPU per thread name | 1 |
| E11 | parking one agent inside a shared browser | one headless Chromium, proc flags and the hang monitor off, holding N = 8 and 32 agents as BrowserContexts, pages rotating. systemd's `DelegateSubgroup` starts the browser in a leaf cgroup, and a root watcher moves each agent's new renderers into a sibling cgroup. Park agent 1: clear its context's HTTP cache, freeze its cgroup, reclaim into zswap (zsmalloc, zstd, writeback off), page out its renderers' remaining private memory with `process_madvise`, hold 20 s, thaw, and time the first CDP call and screenshot; another agent's screenshot is timed during the hold. Then freeze agent 2 alone for 180 s | 1 |
| E14 | native apps alone in Cage | One app (KiCad, Blender, LibreOffice Calc, LTspice under Wine) alone in stock Cage 0.3.1, in a delegated transient unit, driven by grim, wayvnc and AT-SPI scripts rather than panoptes-desk or resd. Memory and idle CPU once settled, capture and virtual input times, the AT-SPI tree, a 20 s freeze, and a park into zswap with writeback off, on the host's zswap settings (zbud, lz4) | 1 |

## Caveats

- One machine, one kernel. Results on newer kernels may differ where this
  doc names a version-specific fix.
- Live pages change and load over a slow link, so page-load times vary.
  Until 18:00 server time, the readiness check could pass on the initial
  `about:blank` before the URL committed. E1b's `ready_ms` column is
  therefore not usable, and one E1b snapshot landed mid-load (noted in
  [desktop.md](desktop.md)). The 64-agent E1c runs were repeated with the
  fixed check; E2, E4, and later runs used it from the start.
- At density, an agent's charge depends on which cgroup first read each
  shared file page, so it varies with what ran before. Two lowend runs
  differed by 39 MiB per agent in charge but by 4 MiB or less in PSS and
  in the drop in MemAvailable. Density comparisons use those two.
- E7 sends 1280x720 images built from page captures, each padded to the
  full output and stamped with an index. No two requests in one server's
  life send the same new image, so prefix-cache hits come only from each
  agent's own history and the shared system prompt.
- MemAvailable moves with anything else on the machine, including the
  model download and vLLM. The slice charge does not.
- E3's first nspawn, bwrap, and Podman runs did not start every instance:
  nspawn instances clashed on a shared machine name, bwrap found the unit
  names still taken, and Podman had no image. Their rows moved to
  `results/e3-buggy.tsv`; the docs use the reruns.
- E6 fetched each API host's root URL without credentials, so first byte
  is a trivial response, not a model call.
- E7 ran with vLLM's FlashInfer sampler off
  (`VLLM_USE_FLASHINFER_SAMPLER=0`). FlashInfer compiles the sampler at
  engine start, and the server's CUDA 12.9 `nvcc` rejects its default
  gcc 15. Requests are greedy, so the sampler does not change outputs.
- E10's freshness check failed on the spinner page because of the check,
  not the capture: the page's stylesheet styles every `div`, so the probe
  overlay became a rotating gray ring and no two captures matched. The
  check passed on the other four pages.
- Eight CNN agents at 60 Hz kept about 28 of the 32 hardware threads busy,
  so CNN's visible rows were measured near saturation.
- E4's first runs kept the host's default, zswap writeback on, and the
  shrinker wrote most of each kiosk to the swap device. Allocator and
  compressor comparisons use only the writeback-off runs.
- `memory.stat`'s `zswap` counts compressed bytes, not the pool's pages.
  The later E4 runs also read the pool's real growth from debugfs
  (`pool_total_size`). zbud's pool grew 112 MiB against a 69 MiB charge
  on Workday, and 109 against 55 on Greenhouse; zsmalloc's grew 2 to 3 MiB
  more than its charge.

## Loop runs, 2026-10-27 and 2026-10-28

panoptes-loop drove one agent per invoice on the earlier desk: Cage and
Chromium at 1280x800, driven over CDP, each desk in a transient systemd
unit under panoptes-resd. The model read each page as Chromium's
accessibility tree, as text, and acted on accessibility nodes. ERPNext and
the supplier portals ran on the same server. The loop verified each
invoice by reading it back through ERPNext's REST API.

| Run | Date | Agents | Model | ERP cap | Verified | Wall |
|---|---|---|---|---|---|---|
| s20 | 2026-10-27 | 20 | scripted | 25 | 20 of 20 | 59 s |
| s300 | 2026-10-28 | 300 | scripted | 25 | 300 of 300 | 502 s |
| s700 | 2026-10-28 | 700 | scripted | 25 | 403 of 700 | 1,354 s |
| s700b | 2026-10-28 | 700 | scripted | 25 | 700 of 700 | 1,156 s |
| s700c | 2026-10-28 | 700 | scripted | 50 | 696 of 700 | 817 s |

- The scripted model, panoptes-oracle, answers from the demo's ground
  truth after 2000 ms, then streams its reply at once.
- The ERP cap is the loop's limit on agents in the ERPNext part of the task
  at once. An agent holds its slot until it leaves that part, through its
  model calls. At most 40 agents act at once in every run.
- s700 failed on the portal server's open-file limit; s700b is the rerun
  after the fix ([one-pager](../one-pager.md), Status).
- Per-step times come from the loop's trace. `action_ms` is the desk calls
  that act. `settle_ms` is the loop's `wait_settled` call with a 300 ms
  quiet window, after steps that changed the page. `observe_ms` is the
  accessibility-tree read. `harness_ms` is step time minus model time, so
  it also counts the ERP and acting waits; in s20 both were zero.

Real models ran on Baseten's OpenAI-compatible API on 2026-10-28, one
agent per invoice, on invoices inv-0701 to inv-0715, with the loop's
walkthrough prompt for the demo:

| Arm | Model | Verified |
|---|---|---|
| ds | DeepSeek V4 Flash | 6 of 10 |
| ds2 | DeepSeek V4 Flash, rerunning ds's 4 failures and inv-0714 | 5 of 5 |
| ds-lean | DeepSeek V4 Flash, trimmed prompt | 9 of 10 |
| oss-low | gpt-oss-120b, low reasoning effort | 7 of 10 |
| k3, k3b | Kimi K3 | 3 of 3 |

Costs come from token counts at prices set in each run's config, not from
bills. The runs' traces and summaries are on the test server, not in this
repository.

## Rerunning

The scripts are in [`bench/`](../../bench), with raw results in
`bench/results/` for E4, E7, E10, E10b, E11, E12, E13 and E14. Most
scripts still assume the test server's paths.
