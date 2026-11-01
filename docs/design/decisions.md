# Decisions

## 2026-10-30: a harness for any Linux GUI app

Status: accepted, 2026-10-30. Supersedes the 2026-10-27 Observation row,
and the screen size and CDP driving in [desktop.md](desktop.md).

| Decision | Choice | Why |
|---|---|---|
| Scope | Any Linux GUI program, Wayland or X11; not a browser harness | The earlier desk drove one Chromium over CDP, so an agent could use only web pages |
| Compositor | Our fork of Cage: [panoptes-os/cage](https://github.com/panoptes-os/cage), branch `panoptes` | Upstream Cage cannot set its output size or list windows with their pid and box, and it mishandled X11 dialogs: they lost focus to clicks on their main window and could not resize |
| Screen | 1024x768 at scale 1 | Anthropic's computer-use guide recommends it for desktops, and it is the cheapest size in desktop.md's token table for every model family there |
| Driving | panoptes-desk as a Wayland client: screencopy for pixels, a virtual pointer and keyboard for input, the fork's `panoptes_window_v1` for windows | It works the same for every program; CDP reaches only Chromium |
| Programs | `launch` runs any argv in the agent's kiosk cgroup | Tasks need programs Panoptes does not know in advance |
| Observation | After each action, wait until the screen stops changing, then report window changes and a screenshot | screencopy reports damage, which the 2026-10-27 row lacked |

## 2026-10-27

Status: accepted, 2026-10-27. The Observation row is superseded by
2026-10-30.

| Decision | Choice | Why |
|---|---|---|
| Name | Panoptes | Argus Panoptes' hundred eyes slept in turns, as parked agents do |
| License | AGPL-3.0-or-later | The closest peers protect their engine (Browserless SSPL, Lightpanda AGPL) |
| Distribution | A Docker image built with Nix (`dockerTools`) | Docker runs everywhere; Nix keeps builds reproducible |
| Base OS | None; the host is the user's | Every measured win is a stock kernel feature driven from userspace |
| Kernel work | No patches. cgroup v2 freeze, `memory.reclaim`, `memory.zswap.writeback`, weights | See the table below |
| Fleet harness | Panoptes' own agent loop, one Rust process, OpenAI-compatible client | 700 Claude Code loops would need about 58 GiB (E9) |
| Other harnesses | panoptes-desk serves MCP | Claude Code, Codex, and browser-use drive Panoptes agents without competing with them |
| Observation | Fixed interval | Event-driven observation (damage, settle, diffs) needs compositor damage and CDP events |
| Demo | Mock supplier portals with no API, invoices keyed into ERPNext, 700 agents, `panoptes top` in Alacritty | The source portals are where APIs run out; ERPNext gives ground truth |
| Demo fixture | ERPNext on Podman | It works; moving it to Docker would add Docker's firewall rules to the host |

### What the kernel features need from the host

The image checks these at start and warns when one is missing:

| Need | Why |
|---|---|
| Linux 6.8 or later, cgroup v2 | `memory.reclaim` (5.19) and `memory.zswap.writeback` (6.8) |
| zswap on, zstd, zsmalloc | Parking: about 270 MiB to 54 to 64 per kiosk (E4) |
| Swap sized for parked agents | Each parked kiosk holds 150 to 180 MiB of swap slots |
| `--privileged` or a writable cgroupfs with `CAP_SYS_ADMIN` | panoptes-resd owns the container's cgroup subtree |

Without zswap and swap, parking falls back to freezing, which saves CPU but
not memory.
