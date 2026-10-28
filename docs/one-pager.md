# Panoptes one-pager

As of 2026-10-28. Panoptes runs hundreds of graphical AI agents on one
Linux host.

## What it does

Each agent gets its own desktop: a Cage compositor running Chromium. An
agent signs in to web portals the way a person would, by reading the
screen and clicking and typing.

Most of an agent's time goes to waiting for the model. While it waits,
Panoptes freezes the agent's desktop with cgroup v2, so the page uses no
CPU. After it has waited a while, Panoptes compresses the desktop's memory
into zswap. A running desktop holds about 180 MiB; a parked one about 40
MiB, and it takes about 100 ms to wake and take a screenshot.

Agents are driven through `panoptes-desk`, which serves screenshot, click,
type, key, scroll, navigate, and accessibility-tree tools over MCP. Our own
agent loop, `panoptes-loop`, drives many agents from one process with any
OpenAI-compatible model API.

## What we measured

All on one test server: 32 threads, 125 GiB RAM, one RTX 3090. Methods are
in [design/measurements.md](design/measurements.md).

| Measurement | Result |
|---|---|
| Agents per host, running, before parking | about 700 (about 175 MiB each at 64 agents) |
| Parked desktop | 171 to 51.6 MiB; first screenshot 60 ms after wake |
| Freezing between steps | a busy page (CNN) went from 1.8 cores per agent to 0.2 |
| Native apps parked | KiCad 1387 to 333 MiB, Blender 871 to 164, LTspice under Wine 215 to 93 |
| 300 agents, scripted model | 300 of 300 invoices correct |
| 700 agents, scripted model | 700 of 700 in 19 min; 670 parked at peak, 40.6 MiB each |
| Real models on Baseten, 10 invoices | DeepSeek V4 Flash 10 of 10 at about $0.075 each; gpt-oss-120b 7 of 10; Kimi K3 3 of 3 at $0.71 to $1.02 |
| DeepSeek V4 Flash, trimmed prompt | 9 of 10 at about $0.015 each |
| Container overhead | 0.4 to 2.9 MiB per instance; Docker 5.9 |

The scripted model answers from the demo's ground truth after a 2 s delay.
It tests Panoptes at scale; the Baseten rows test real models.

## Architecture

```text
panoptes-resd      one per host; freezes waiting desktops, parks idle
                   ones in zswap, sets CPU weights, evicts when memory
                   or swap runs low
agent, one per     its own cgroup tree
├─ desk/           panoptes-desk: the tools above, over MCP or a socket
└─ kiosk/          Cage and Chromium
panoptes-loop      one process driving every agent's model calls
panoptes top       terminal view of every agent, read from resd
```

One step:

```text
panoptes-loop (or any MCP client): click(...)
└─ panoptes-desk
   ├─ ask resd to thaw or unpark the desktop and raise its CPU weight
   ├─ click over the Chrome DevTools Protocol, then screenshot
   └─ tell resd it is done; resd freezes it, and parks it if it stays idle
```

The Docker image builds with Nix and runs desks without systemd inside.
20 agents ran in it under Podman, 20 of 20 correct.

## The demo

In the demo, 700 agents do invoice entry. Each signs in to a mock supplier
portal with no API, downloads an invoice PDF, and enters it in ERPNext
against a purchase order. We check every result against ERPNext's records.

- The fixture has 20 suppliers, 40 items, 1,000 invoices in 20 layouts,
  and 1,000 purchase orders.
- `panoptes top` shows one box per agent.

## Status and where you can help

The 700-agent run passes, 700 of 700. The first attempt scored 403 of 700;
two host limits caused it, and both are fixed:

| Problem | Cause | Fix |
|---|---|---|
| Portal requests took 51 s; 297 invoices failed | The portal server hit its limit of 1,024 open files, because frozen browsers keep idle connections open | The portal server raises its own limit at start |
| Parked desktops stayed at 92.9 MiB instead of 52.7 | zswap's pool was capped at 20% of RAM, and its zbud allocator stores at most 2 pages in 1 | Pool cap 50%, zsmalloc, zstd; resd evicts the longest-parked desktops when memory or swap runs low |

The code is one Rust workspace in this repository, with the demo fixture
and the benchmark scripts.

Where to start:

- [ ] Read [design/decisions.md](design/decisions.md) and
      [design/README.md](design/README.md).
- [ ] Pick a part to work on: desk tools, loop prompts, resd policy, or
      the `top` dashboard.
