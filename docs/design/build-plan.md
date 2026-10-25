# Build plan: unit runtime, residency manager, panoptes-desk

Status: partly superseded, 2026-10-27, by [decisions.md](decisions.md).
Units run inside one Docker container, so panoptes-resd creates each
agent's cgroups itself with `clone3(CLONE_INTO_CGROUP)` instead of asking
systemd, and per-agent users replace `DynamicUser=`. The milestones and
their targets still hold. Proposed 2026-10-26. Covers the first three
parts of Panoptes: the unit runtime, the residency manager, and
panoptes-desk. Audit, inference routing, and the image get only what the
pilot needs. Code is
Rust. Every acceptance target below is a number already measured on the
test server ([measurements.md](measurements.md)), so each milestone passes
or fails against the bench, not against taste.

## The pilot this plan builds

One host runs N agents. Each agent drives its own kiosk through
panoptes-desk. A kiosk is frozen while its agent waits for the model and
parked when the agent goes idle. One command starts, lists, parks, and
stops agents.

Out of scope until a customer asks: more than one host, a web console,
native (non-browser) apps, and microVM isolation.

## Why Rust fits, and where it does not matter

- The daemons talk to systemd over D-Bus, to cgroup v2 through files, to
  Wayland, and to Chromium over a WebSocket. Rust has maintained crates
  for each: `zbus`, `rustix`, `wayland-client`, and `tokio` with a
  WebSocket client. MCP has an official Rust SDK, `rmcp`.
- One static binary per daemon fits a Nix-built appliance.
- Rust does not make an agent cheaper. The measured costs are Chromium,
  the kernel's reclaim, and the GPU; Panoptes' own processes must only stay
  out of the way (a few MiB each, no CPU while agents idle).

## Layout

```text
panoptes/
├─ crates/
│  ├─ panoptes-cgroup  cgroup v2 files: freeze, memory.reclaim, weights, PSI
│  ├─ panoptes-unit    unit spec; transient units through systemd's D-Bus API
│  ├─ panoptes-cdp     thin CDP client, generated from the pinned Chromium's PDL
│  ├─ panoptes-resd    residency manager daemon
│  ├─ panoptes-desk    per-agent control server: capture, input, tree; MCP
│  └─ panoptesctl      command line
├─ nix/                packages, the NixOS module, VM tests
└─ bench/              the bench scripts, moved in from the server
```

`panoptes-cgroup` and `panoptes-unit` are libraries with no daemon state, so
the bench scripts and the daemons share one implementation of each
kernel interface.

## One agent's processes

The kiosk and the control server need different lifecycles: the kiosk
freezes between steps, and panoptes-desk must keep answering so the agent's
next call can thaw it. Each unit therefore splits into subgroups, using
systemd's `DelegateSubgroup=` as E11 did:

```text
panoptes.slice
└─ panoptes-<id>.service  DynamicUser, StateDirectory holds the profile
   ├─ kiosk/              frozen, reclaimed, and weighted by panoptes-resd
   │  └─ cage
   │     └─ chromium      lean flags, --remote-debugging-pipe
   ├─ desk/               never frozen
   │  └─ panoptes-desk
   └─ loop/               the agent loop, when it runs on this host
panoptes-resd.service     root; owns every unit's kiosk/ files
```

## An agent's step

```text
agent: screenshot()
└─ panoptes-desk (desk/)
   ├─ resd.acquire(id)          thaw if frozen; resume if parked;
   │                            cpu.weight=10000 on kiosk/
   ├─ CDP Page.captureScreenshot
   ├─ return image
   └─ no call for GRACE ms
      └─ resd.release(id)       cpu.weight back; freeze kiosk/
```

A CDP capture forces a frame instead of waiting for the output's clock,
so the kiosk runs at 60 Hz while thawed and its rate costs nothing while
frozen ([desktop.md](desktop.md#between-steps-freeze-the-kiosk)). Input
goes through CDP's `Input` domain. Wayland virtual input waits for native
apps.

## Residency states

```text
starting ─> acting ─> waiting ─> parked ─> evicted
acquire: waiting ─> acting (thaw)
acquire: parked ─> acting (thaw from zswap)
acquire: evicted ─> starting (relaunch on the same profile)
```

| State | Kiosk | Cost per agent (measured) | Back to acting |
|---|---|---|---|
| acting | thawed, weight 10000 | about 180 MiB lean | |
| waiting | frozen | same memory, near-zero CPU | 9 to 16 ms more on the first capture |
| parked | frozen, reclaimed into zswap | 33 to 46 MiB lean | 40 to 75 ms to the first screenshot |
| evicted | stopped; profile on disk | disk only | a cold launch on the same profile |

The policy is a pure function from (state, events, clock, host memory) to
actions, with no I/O. Tests drive it with a fake clock and fake memory
pressure; the daemon only applies its actions. After a crash, panoptes-resd
rebuilds its state from `cgroup.freeze`, `memory.stat`, and a small state
file, so restarting it never thaws or kills an agent.

## Milestones

Each milestone ends with a bench rerun on the test server. Targets come
from the numbers named in brackets.

| # | Delivers | Passes when |
|---|---|---|
| B0 | Cargo workspace; Nix packages; `panoptes-cgroup` with NixOS VM tests for freeze, reclaim, and weights; bench scripts in `bench/` | VM tests pass in `nix flake check`; the E4 park script runs on `panoptes-cgroup` |
| B1 | `panoptes-unit` and `panoptesctl run/ls/stop`: a kiosk unit with lean flags | 64 units on one URL charge within 10 percent of E1c; the unit is active within 20 ms of the call, before Chromium starts (E3: about 10 ms) |
| B2 | `panoptes-desk` over MCP: screenshot, click, type, key, scroll, navigate, accessibility tree | An agent fills a Greenhouse application form end to end; click plus screenshot matches E5's idle-host latency |
| B3 | `panoptes-resd` acquire and release with freezing between steps | 8 CNN agents use 25 percent of a core each or less (E10b: 22); first capture adds 16 ms or less |
| B4 | Parking, resume, eviction; crash recovery | A lean kiosk parks to 50 MiB or less and resumes to a screenshot in 75 ms or less (E4); killing panoptes-resd during 64 parked agents loses none |
| B5 | Pilot hardening: an append-only log of every desk call with a capture hash; `panoptesctl status` with per-agent memory, CPU, and state | A 24 h soak of 64 agents with random step timing: no leaked units, no stuck freezes, memory flat |

B2 can run in parallel with B1 once `panoptes-unit` starts a kiosk.

## Tests

- Unit tests for pure logic: the residency policy, unit spec rendering,
  CDP message framing.
- NixOS VM tests for kernel behavior: freeze and thaw, `memory.reclaim`
  into zswap, delegation, and crash recovery. They run in
  `nix flake check`, with no Chromium where a `sleep` unit proves the
  point.
- Bench reruns on the test server as the performance gate. A milestone
  that regresses a measured number by more than 10 percent does not pass.

## Decisions

- CDP client. `chromiumoxide` exists but tracks Chromium's protocol
  loosely, and Panoptes needs about 30 commands. Recommended: generate a thin
  client from `browser_protocol.pdl` at the pinned Chromium (156), and
  regenerate on each bump.
- Pipe, not port. `--remote-debugging-pipe` keeps CDP off the network
  namespace; any process that can reach a debugging port owns the
  browser.
- One Linux user per unit through `DynamicUser=`, with the profile in
  `StateDirectory=`. Kiosks then cannot read each other's profiles.
- SPONGE: the grace period before release. E10b froze right after each
  step. An agent that sends several actions per model call needs a short
  grace; 100 ms is a guess to measure in B3.
- SPONGE: where the agent loop runs. On the host (the `loop/` subgroup),
  it parks with its kiosk but costs about 85 MiB. Off the host, Panoptes is
  only the desktop. Parking the loop is proposed, not measured.
- SPONGE: the MCP tool shape. Anthropic's and OpenAI's computer-use tools
  and UI-TARS each expect their own action schema. Recommended: one
  internal action set, with a thin adapter per provider in `panoptes-desk`.

## Before B4: tests that can change this plan

- Logins and websockets through freezes of 10 min, 1 h, and 8 h. If
  sessions die, B4 needs reconnect logic or a shorter park limit. It
  needs a throwaway account from the user.
- Heavy pages (Sheets, Figma): if renderers park much worse than 33 to
  46 MiB, the density targets in B1 and B4 change.
