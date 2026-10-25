# Panoptes one-pager

As of 2026-10-27. Panoptes runs hundreds of graphical AI agents on one
Linux box by parking idle ones in compressed memory. We pitch it to a16z on
Nov 1 and YC on Nov 2.

## What it is

Panoptes gives each AI agent a real desktop (a browser, or a native or Wine
app) and keeps hundreds of them alive on one host. It targets work where
the API runs out: supplier portals, legacy web apps, desktop tools.

- The browser is what an agent costs: about 180 MiB running with lean
  flags.
- An agent spends most of its life waiting for the model. Panoptes freezes
  the desktop between steps and, when idle, compresses it into zswap. A
  parked agent costs about 50 MiB and is back in 60 ms.
- Everything is stock Linux driven from userspace: cgroup v2 freeze,
  `memory.reclaim`, zswap with writeback off, `cpu.weight`. No kernel
  patches.
- It ships as a Docker image built with Nix. Any harness can drive an
  agent over MCP; Panoptes also has its own Rust agent loop for fleets.

Pitch line: MCP is the wire, Panoptes is the body behind it.

## What we measured

One test server: 32 threads, 125 GiB RAM, one RTX 3090. Methods are in
[design/measurements.md](design/measurements.md).

| Claim | Result |
|---|---|
| Agents per host, running, before parking | about 700 (lean kiosk about 175 MiB each at 64 agents) |
| Parked kiosk | 171 to 51.6 MiB; first screenshot 60 ms after wake |
| Freeze between steps | a busy page (CNN) went from 1.8 cores per agent to a fifth of one |
| Native apps parked | KiCad 1387 to 333 MiB, Blender 871 to 164, LTspice under Wine 215 to 93 |
| VPN routing | 0.7 MB kernel memory per agent; 504 Mbps through one shared OpenVPN client |
| Agent loop, scripted model, 300 agents | 300 of 300 invoices correct |
| Agent loop, scripted model, 700 agents | 403 of 700; host limits, fixes known (see Status) |
| Container mechanism | 0.4 to 2.9 MiB per instance; Docker 5.9 |

The scripted model is a fake that answers instantly, so these runs test
Panoptes, not model accuracy. Real-model runs on Baseten are in progress.

## Architecture

```text
Docker image, built with Nix
├─ panoptes-resd    one per host; freezes waiting kiosks, parks idle
│                   ones in zswap, sets cpu.weight
└─ agent, x700      one cgroup tree each
   ├─ loop/         panoptes-loop, Rust agent loop; calls the model API
   ├─ desk/         panoptes-desk: screenshot, click, type, ax tree; MCP
   └─ kiosk/        Cage + Chromium, or a native or Wine app; reaches
                    target apps through per-agent VPN routes
panoptes top        terminal view of every agent, read from resd
```

One step:

```text
loop (or any MCP harness): click(...)
└─ panoptes-desk
   ├─ resd.acquire(id)     thaw or unpark the kiosk, raise its weight
   ├─ CDP over a pipe      act, then screenshot
   └─ resd.release(id)     freeze again; park if it stays idle
```

## Business model

The Tailscale model: an open engine people adopt bottom-up, and a paid
hosted control plane for teams.

| Tier | Gets | Inference |
|---|---|---|
| Individuals | AGPL-3.0 engine; 30 agents fit on a laptop (about 5 GB running, 1.2 GB parked) | Bring your own key, or a local model |
| Teams | Hosted control plane (closed) | Bundled, through our router to Baseten or Cerebras |
| Enterprise | Control plane, support, VPN routing into private networks | Bundled |

Outside contributions need a CLA, so the control plane can stay closed.

## Pitch week

The demo: 700 graphical agents do invoice entry. Each logs into a mock
supplier portal with no API, downloads an invoice PDF, and keys it into
ERPNext against a purchase order. ERPNext's records are ground truth, so
the score is checkable.

- Fixture: 20 suppliers, 40 items, 1,000 invoices in 20 layouts, 1,000
  POs.
- One portal can stage a CAPTCHA. The agent detects it, parks, and waits
  for a person; it never solves CAPTCHAs itself.
- The command center is `panoptes top`, one box per agent, in Alacritty,
  filmed on a phone.
- Kimi K3 is the ceiling model; DeepSeek V4 Flash and gpt-oss-120b set the
  price story.

## Status and where you can help

The prototype works end to end at 300 agents. At 700 one host limit failed
297 invoices, and a second cost memory; both are fixed and the rerun is
pending:

| Problem at 700 | Cause | Fix |
|---|---|---|
| Portal requests took 51 s; 297 invoices failed | Portal server hit 1,024 open files; frozen browsers hold idle keep-alive connections | The portal server raises its own limit at start |
| Parked kiosks stayed at 92.9 MiB, not 52.7 | zswap pool capped at 20% of RAM, with zbud's 2:1 ceiling | Pool cap 50%, zsmalloc, zstd; resd evicts the longest-parked kiosks when memory or swap runs low |

Code today: a Rust workspace (cgroup library, CDP client, desk, loop,
resd, top, CLI), the demo fixture, and bench scripts. It moves into this
repo as one monorepo this week.

Where to start:

- [ ] Read [design/decisions.md](design/decisions.md) and
      [design/README.md](design/README.md).
- [ ] Pick a subsystem once the monorepo lands: desk tools, loop prompts,
      resd policy, or the `top` dashboard.
- [ ] Help script and film the demo before Nov 1.
