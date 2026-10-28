# Panoptes

Run hundreds of graphical AI agents on one Linux box.

Each agent gets a real desktop: a browser, or a native or Wine app in its
own Cage kiosk. Panoptes freezes an agent's desktop while it waits for its
model and parks it in compressed memory when it goes idle, so a parked
agent costs about 40 MiB and is back on screen in about 100 ms. Everything
is stock Linux driven from userspace: cgroup v2 freeze, `memory.reclaim`,
zswap with writeback off, and `cpu.weight`. No kernel patches.

Any harness can drive an agent over MCP; Panoptes also ships its own Rust
agent loop for fleets. MCP is the wire, Panoptes is the body behind it.

Status: prototype. On one 32-thread, 125 GiB host, 700 agents entered 700
of 700 invoices correctly, with 670 parked at the peak. See
[docs/one-pager.md](docs/one-pager.md) for the numbers and
[docs/design/](docs/design/README.md) for how each was measured.

## Layout

```text
crates/
├─ panoptes-cgroup   cgroup v2: freeze, reclaim, weights, PSI, clone3 spawn
├─ panoptes-cdp      thin Chrome DevTools Protocol client over a pipe
├─ panoptes-desk     per-agent control server: screenshot, input, tree; MCP
├─ panoptes-resd     residency manager: freezes, parks, and evicts kiosks
├─ panoptes-loop     agent loop with an OpenAI-compatible model client
├─ panoptes-oracle   scripted fake model for harness tests
├─ panoptes-top      terminal dashboard of every agent
└─ panoptesctl       command line
demo/                invoice-entry demo: generator, supplier portals, ERPNext
bench/               the E1 to E14 benchmark scripts and raw results
docs/                design record and decisions
```

## Build

Linux only, cgroup v2, kernel 6.8 or later.

```sh
nix develop
cargo build --release --workspace
cargo test --workspace
```

Parking needs zswap (zsmalloc, zstd) and swap sized for the parked agents;
[docs/design/decisions.md](docs/design/decisions.md) lists the host
requirements.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
