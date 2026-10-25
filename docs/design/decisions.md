# Decisions, 2026-10-27

Status: accepted by the user, 2026-10-27. These supersede the parts of
[base-os.md](base-os.md) and [build-plan.md](build-plan.md) that assume a
NixOS appliance or systemd inside each unit. Measurements are unchanged.

| Decision | Choice | Why |
|---|---|---|
| Name | Panoptes | Argus Panoptes' hundred eyes slept in turns, as parked agents do |
| Market | Individuals first, then teams, then enterprise | 30 lean kiosks fit in about 5 GB running or 1.2 GB parked, so a laptop runs a useful fleet |
| License | AGPL-3.0 engine, closed hosted control plane, CLA from the first outside contribution | The closest peers protect their engine (Browserless SSPL, Lightpanda AGPL); Tailscale's open agent and paid control plane fit a bottoms-up product |
| Distribution | A Docker image built with Nix (`dockerTools`) | Docker runs everywhere; Nix keeps builds reproducible |
| Base OS | None; the host is the user's | Every measured win is a stock kernel feature driven from userspace |
| Kernel work | No patches. cgroup v2 freeze, `memory.reclaim`, `memory.zswap.writeback`, weights | See the table below |
| Fleet harness | Panoptes' own agent loop, one Rust process, OpenAI-compatible client | 700 Claude Code loops would need about 58 GiB (E9) |
| Other harnesses | panoptes-desk serves MCP | Claude Code, Codex, and browser-use drive Panoptes agents without competing with them |
| Observation | Fixed interval for the MVP; event-driven (damage, settle, diffs) later | Event-driven needs compositor damage and CDP events, too much for the pitch week |
| CAPTCHAs | Detect, park, hand off to a person, resume | Parking makes a waiting agent cost about 40 MiB |
| Networking | VPN routing on day 1: one shared client per network, per-agent netns and routes | E12: 0.7 MB of kernel memory per agent, 504 Mbps through one OpenVPN client |
| Demo | Mock supplier portals with no API, invoices keyed into ERPNext, 700 agents, `panoptes top` in Alacritty | The source portals are where APIs run out; ERPNext gives ground truth |
| Demo fixture | ERPNext on Podman | It works; moving it to Docker would add Docker's firewall rules to the host |

## What the kernel features need from the host

The image checks these at start and refuses to park without them:

| Need | Why |
|---|---|
| Linux 6.8 or later, cgroup v2 | `memory.reclaim` (5.19) and `memory.zswap.writeback` (6.8) |
| zswap on, zstd, zsmalloc | Parking: about 270 MiB to 54 to 64 per kiosk (E4) |
| Swap sized for parked agents | Each parked kiosk holds 150 to 180 MiB of swap slots |
| `net.ipv4.neigh.default.gc_thresh1/2/3` raised, for example 2048/8192/16384 | The defaults cap a host at about 500 reachable agent namespaces (E12) |
| `--privileged` or a writable cgroupfs with `CAP_SYS_ADMIN` and `CAP_NET_ADMIN` | panoptes-resd owns the container's cgroup subtree and the agents' network namespaces |

Without zswap and swap, parking falls back to freezing, which saves CPU but
not memory.

## VPN routing requirements from E12

1. A kill switch: an `unreachable` default route in each VPN table. Without
   it, a dropped tunnel leaked 16.5 MB out the direct path.
2. Bridge port isolation, or agents reach each other below nftables.
3. Route changes only while the agent is frozen; changes under live
   transfers stalled 17 of about 5,700 connections.
4. Netlink from panoptes-resd, not one `ip` exec per agent (18 s for 700).
5. At most 1,024 ports per bridge; more agents need more bridges.
