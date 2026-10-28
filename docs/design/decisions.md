# Decisions, 2026-10-27

Status: accepted, 2026-10-27.

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

## What the kernel features need from the host

The image checks these at start and refuses to park without them:

| Need | Why |
|---|---|
| Linux 6.8 or later, cgroup v2 | `memory.reclaim` (5.19) and `memory.zswap.writeback` (6.8) |
| zswap on, zstd, zsmalloc | Parking: about 270 MiB to 54 to 64 per kiosk (E4) |
| Swap sized for parked agents | Each parked kiosk holds 150 to 180 MiB of swap slots |
| `--privileged` or a writable cgroupfs with `CAP_SYS_ADMIN` | panoptes-resd owns the container's cgroup subtree |

Without zswap and swap, parking falls back to freezing, which saves CPU but
not memory.
