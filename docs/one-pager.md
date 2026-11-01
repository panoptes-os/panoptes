# Panoptes one-pager

As of 2026-10-31. Panoptes is a high-performance harness and orchestrator
for graphical AI agents on Linux, designed for fast models. It is a
prototype, and heavily a work in progress.

## What it does

Each agent gets its own Linux desktop: our fork of the Cage compositor at
1024x768, running whatever programs the agent launches, Wayland or X11.
`panoptes-desk` serves the agent's tools over MCP, in screen pixels:
screenshot and windows to look; click, move, drag, scroll, key and type to
act; launch, focus, close, kill and wait to manage programs. Each action
waits for the screen to settle, then reports the windows that opened,
closed, or took focus.

We design for models that keep getting faster and cheaper. We treat a
model call as free and instant, and optimize what each step costs the
host: acting, waiting for the screen to settle, and capturing it. On the
earlier desk, with a scripted model, that work took a median of 660 ms per
step, about half of it the loop's 300 ms settle wait.
[vision.md](vision.md) has the numbers and the bet behind them.

When an agent stops acting, `panoptes-resd` freezes its desktop with
cgroup v2, so its programs use no CPU. After 30 s idle, it compresses the
desktop's memory into zswap. That pays while an agent waits on something
slower than itself: a person, a slow backend, or a quota. Measured with
Chromium, parking took an idle desktop from about 270 MiB to 54 to 64, and
its first screenshot came 40 to 75 ms after waking.

`panoptes-loop` is the invoice demo's driver for the earlier desk. One
process drives many agents through any OpenAI-compatible model API, with a
walkthrough prompt written for the demo. It speaks the earlier desk's
browser tools, so it does not run against the Wayland desk yet.

## What we measured

Every result below comes from the earlier desk, which ran Chromium in each
Cage and drove it over the Chrome DevTools Protocol. In the agent runs the
screen was 1280x800, and the model read each page as Chromium's
accessibility tree, as text. No task driven by pixels has been measured
yet, and neither has the Wayland desk's cost per step.

All runs used one test server: 16 cores and 32 threads, 125 GiB RAM, one
RTX 3090. Methods are in [design/measurements.md](design/measurements.md).

| Measurement | Result |
|---|---|
| Parked desktop, Chromium (E4) | about 270 MiB before, 54 to 64 MiB parked; first screenshot 40 to 75 ms after waking |
| Freezing between steps (E10b) | a busy page (CNN) went from 1.8 cores per agent to 0.2 |
| Agents per host, running, before parking | about 700, by arithmetic from 64 agents at about 175 MiB each (E1c) |
| Native apps parked (E14: stock Cage, zbud) | KiCad 1387 to 333 MiB, Blender 871 to 164, LTspice under Wine 215 to 93, as charged to the cgroup; zbud's real pool is larger |
| 300 agents, scripted model | 300 of 300 invoices verified |
| 700 agents, scripted model | 700 of 700 verified in 19 min; 670 parked at peak, 40.6 MiB each |
| 300 against 700 agents, scripted model | The same rate, about 0.6 invoices/s, set by the loop's own cap of 25 agents in ERPNext, each slot held through 2 s model calls |
| Harness time per step, 20 agents | 660 ms median, 1,316 at the 90th percentile; the loop's settle wait 324 ms median |
| DeepSeek V4 Flash on Baseten | 6 of 10 on the first run; the 4 that failed passed when rerun |
| DeepSeek V4 Flash, trimmed prompt | 9 of 10, about $0.018 per invoice |
| gpt-oss-120b on Baseten | 7 of 10, about $0.061 per invoice |
| Kimi K3 on Baseten | 3 of 3, $0.71 to $1.02 per invoice |
| Container overhead (E3) | 0.4 to 2.9 MiB per instance; Docker 5.9 |

The scripted model answers from the demo's ground truth after a 2 s delay.
It tests Panoptes at scale. The Baseten rows test real models with a
prompt written for this demo, so they measure following a walkthrough, not
invoice entry in general. Their costs come from token counts at prices set
in each run's config.

## Architecture

```text
panoptes-resd      one per host; freezes waiting desktops, parks idle
                   ones in zswap, sets CPU weights
agent, one per     its own cgroup tree
├─ desk/           panoptes-desk: the tools above, over MCP or a socket
└─ kiosk/          the Cage fork, Xwayland, and the programs the agent launched
panoptes-loop      the invoice demo's driver, for the earlier desk
panoptes-top       terminal view of every agent, read from resd
```

One step:

```text
any MCP client: click(x, y)
└─ panoptes-desk
   ├─ ask resd to thaw or unpark the desktop and raise its CPU weight
   ├─ move and press a virtual pointer over Wayland
   ├─ wait until the screen stops changing, then screenshot it
   └─ tell resd it is done; resd freezes it, and parks it if it stays idle
```

## The demo

In the demo, 700 agents do invoice entry. Each signs in to a mock supplier
portal with no API, downloads an invoice PDF, and enters it in ERPNext
against a purchase order. We check every result against ERPNext's records.
The demo ran on the earlier desk. panoptes-loop has not moved to the
Wayland desk, so the demo does not run on it yet.

- The fixture has 20 suppliers, 40 items, 1,000 invoices in 20 layouts,
  and 1,000 purchase orders.
- `panoptes-top` shows one box per agent.

## Status and where you can help

The 700-agent run passed on the earlier desk, 700 of 700. The first
attempt scored 403 of 700; two host limits caused it, and both are fixed:

| Problem | Cause | Fix |
|---|---|---|
| Portal requests took 51 s; 297 invoices failed | The portal server hit its limit of 1,024 open files, because frozen browsers keep idle connections open | The portal server raises its own limit at start |
| Parked desktops stayed at 92.9 MiB instead of 52.7 | zswap's pool was capped at 20% of RAM, and its zbud allocator stores at most 2 pages in 1 | Pool cap 50%, zsmalloc, zstd |

The code is one Rust workspace in this repository, with the demo fixture
and the benchmark scripts.

Where to start:

- [ ] Read [vision.md](vision.md), [design/decisions.md](design/decisions.md)
      and [design/README.md](design/README.md).
- [ ] Pick a part to work on: desk tools, resd policy, a general agent loop
      for the Wayland desk, or the `panoptes-top` dashboard.
