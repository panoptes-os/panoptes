# Panoptes architecture

Status: superseded, 2026-10-27, by
[design/decisions.md](design/decisions.md) and
[design/build-plan.md](design/build-plan.md). Kept as the first draft,
written before the project was named Panoptes.

Status then: draft, 2026-10-24. Only the dev shell in `flake.nix` existed. Work
proceeds in six milestones, M0 to M5, listed under
[Milestones](#milestones). M0 runs directly on a machine's own OS, units
become containers at M3, local engines arrive with density testing at M4,
and the host layer arrives at M5. Each claim below links its source, or says
that it is an estimate, a user report, or not yet run.

Panoptes runs CLI agents such as Codex and Claude Code against real GUI apps on
headless Linux. Each app runs alone in a Cage kiosk, a Wayland compositor
that shows one maximized window. Agents drive kiosks through MCP servers.
They use CDP for Chrome and the accessibility tree for other apps, and they
fall back to screenshots and synthetic input when neither works. Each agent's
model is a cloud API or a local engine, a model server that the host shares
among units. A unit holds the kiosks and agents of one workload, and it is
the trust boundary. A Nix flake declares each workload. The first workload,
jobpilot, applies to jobs through Chrome and the Simplify extension. The
second, mail, drives Thunderbird. Above the units, a host layer turns a
machine into an appliance that runs units the way Proxmox runs VMs.

```text
host          NixOS appliance from M5
├─ engine     a local model server shared by units, from M4; optional
└─ unit       one workload and one credential domain; a container from M3
   ├─ kiosk   Cage, one app, a private D-Bus session
   └─ agent   codex exec or Claude Code, with its MCP servers and its model
```

## Terms

| Term | Meaning | Example |
|---|---|---|
| kiosk | One `cage` process, the one app it runs, and that app's private D-Bus session bus | `jobpilot/chrome` runs Google Chrome at 1280x720 on the `chrome-profile` volume, with CDP on `127.0.0.1:9222` |
| agent | One CLI process that drives kiosks through MCP servers and sends model requests to a cloud API or an engine | `codex exec --profile pilot`, working through `run.md` |
| unit | The kiosks, agents, volumes, and Icarus group of one workload | `jobpilot` |
| host | The OS layer that runs units and engines, planned as a NixOS appliance | the rig running the Panoptes NixOS module |
| engine | A local inference server and the model it serves, shared by the units that use it | vLLM serving gpt-oss-20b on a GPU |
| engine proxy | The host process between units and an engine, with one socket per unit | `/run/panoptes/engine/jobpilot.sock`, which salts jobpilot's cache and forwards only inference routes |
| workload | A declaration in the flake that the flake builds into a unit | `jobpilot`, `mail`, `stress` |
| run | One scheduled invocation of a unit's agents, with a time limit | the daily `jobpilot` run |
| dot | OpenAI's always-on ChatGPT agent, connected to melchior, the ThinkPad that runs M0, through the ChatGPT app | reads `ledger/` and reports to the user |

## A unit is a set of kiosks driven by agents

```text
unit jobpilot                       one credential domain, one Icarus group
├─ kiosk chrome
│  ├─ dbus-daemon --session         private bus; activates at-spi-bus-launcher
│  ├─ cage -d                       WLR_BACKENDS=headless WLR_RENDERER=pixman
│  │  └─ google-chrome              --ozone-platform=wayland
│  │                                --user-data-dir=<chrome-profile volume>
│  │                                --remote-debugging-port=9222
│  └─ wayvnc 127.0.0.1 5901         human view and takeover
├─ agent codex                      codex exec --profile pilot
│  ├─ model                         a cloud API, or an engine from M4
│  ├─ mcp chrome-devtools           → CDP on 127.0.0.1:9222
│  ├─ mcp fill                      → 1Password service account, then CDP
│  ├─ mcp panoptes-desk, from M2    → a kiosk's Wayland socket and AT-SPI bus
│  └─ mcp icarus, from M3           → icarus-sync and the group database
├─ volumes                          chrome-profile/ icarus/ ledger/ runs/
├─ engine socket, from M4           → the host's engine proxy
└─ timer                            starts a run; RuntimeMaxSec ends it

dot, through the ChatGPT app on melchior → reads ledger/ and reports
```

At M0 the unit is a set of systemd user services on melchior. From M3 the
flake builds the same unit as one OCI image, and rootless podman runs it as
one container.

## Each run appends to the ledger until its time limit

```text
the timer fires at the scheduled time
└─ panoptes-run jobpilot
   ├─ start kiosk chrome if it is down
   ├─ codex exec --profile pilot --json "$(cat run.md)"
   │  ├─ take the next posting from ledger/queue.jsonl
   │  ├─ chrome-devtools: open it, read the accessibility snapshot,
   │  │  fill fields from answers.yaml, upload the resume
   │  ├─ a question missing from answers.yaml → needs_human row, next posting
   │  ├─ append a submitting row, then submit or queue per the submit policy
   │  └─ append the final row with evidence
   ├─ JSONL events, stamped on arrival → runs/<run>.jsonl and journald
   └─ RuntimeMaxSec expires → systemd stops the run; written rows stay
```

A stop between the submit click and the final row would leave an application
with no record. To prevent that, the agent appends a `submitting` row before
it clicks submit. A later run treats a posting with a `submitting` row and no
final row as `needs_human`, never as new, so a restart does not apply twice.

`codex exec --json` events carry no timestamps, per
[`exec_events.rs`][codex-events], so panoptes-run stamps each line when it
arrives. The stamps split each turn into model time and tool time, which
shows whether a faster model would shorten runs.

## Kiosks place windows predictably

Cage maximizes the primary window and centers every other window at
((W - w) / 2, (H - h) / 2), where W×H is the output size and w×h is the
window size. The main window therefore starts at the output origin, and a
dialog's origin follows from its size. The rule is in
[cage `view.c` L79-104][cage-view].

Other kiosk facts:

- Cage exits when its app exits, per [`cage.c` L97-114][cage-exit]. The
  kiosk's service restarts it, and the agent re-reads the app's state.
- Cage 0.3.1 implements wlr-screencopy, virtual keyboard, virtual pointer,
  wlr-output-management, xdg-output, and foreign-toplevel, per
  [`cage.c` L480-556][cage-protocols]. It lacks wlr-data-control, the
  protocol that clipboard tools use in the background, so agents type text
  instead of pasting it.
- The wlroots 0.20.2 headless backend creates one 1280x720 output, per
  [`backend.c` L227-229][wlr-default]. It sends frame events from a timer,
  per [`headless/output.c` L113-151][wlr-frames], at 60 Hz by default, per
  [`headless.h` L7][wlr-refresh]. Apps keep redrawing without a screencast
  stream, so a kiosk needs no PipeWire.
- With `WLR_RENDERER=pixman`, Cage renders on the CPU and needs no GPU or
  OpenGL driver. That should also avoid the GL driver mismatch that Nix-built
  graphics programs hit on Fedora. Not yet run.
- Each kiosk has its own D-Bus session bus. AT-SPI apps find the
  accessibility bus through the session bus, so an agent sees only its own
  kiosk's tree. waydriver isolates its sessions the same way. Not yet run
  here.
- Chrome 136 and later ignore `--remote-debugging-port` on Chrome's default
  profile directory, per [Chrome's announcement][chrome-136]. The
  `chrome-profile` volume is a separate directory, so CDP works.

Resolution is a per-kiosk parameter. The default is 1280x720 because the
backend creates that output with no setup. Cage implements output
management, so `wlr-randr --output HEADLESS-1 --custom-mode 1024x768` should
change it. Not yet run. The case for 1024x768 rests on OpenAI's older
description of how it splits images into tiles. No one has measured whether
the model behind Codex counts image tokens that way. Measure input tokens per
screenshot, for each model, from the usage events in `codex exec --json`
before changing the default.

wayvnc 0.10.2 refuses to start without screencopy, virtual pointer, and
virtual keyboard, per [`main.c` L418-431][wayvnc-req]. Cage has all three,
so wayvnc should attach to a kiosk. Not yet run. It listens on 127.0.0.1, and
a person connects through SSH to melchior.

```sh
ssh -L 5901:127.0.0.1:5901 rhegde@10.40.251.150
```

A person on VNC and the agent share the kiosk's single seat, so keystrokes
interleave if both type at once. Stop the run before taking over. A better
takeover protocol is an open decision.

## Agents read structure first and pixels last

| App | Primary control | Fallback |
|---|---|---|
| Chrome | chrome-devtools-mcp over CDP, which gives the accessibility snapshot with element handles, navigation, and file upload | a panoptes-desk screenshot and an absolute click |
| Thunderbird and other non-browser apps | panoptes-desk, which finds nodes by AT-SPI XPath and then calls AT-SPI actions and text edits that need no coordinates | AT-SPI window-relative extents, mapped to output pixels by Cage's placement rule, then an absolute click |

AT-SPI screen coordinates are unreliable on Wayland. Clients don't know where
their windows are on screen, and GTK 4 stopped reporting screen coordinates,
per [at-spi2-core #72][atspi-72] and [GTK #6343][gtk-6343]. Cage's placement
rule makes the window-to-output mapping computable instead. A calibration
click at M2 checks it.

panoptes-desk is a planned Rust MCP server with these tools:

```text
locate(xpath)           → nodes with id, role, name, states, window extents
act(id, action, text)   → AT-SPI Action or EditableText, no coordinates
screenshot()            → PNG of HEADLESS-1 through wlr-screencopy
click(x, y, button)     → virtual pointer, absolute motion in output pixels
type(text), key(chord)  → virtual keyboard with an uploaded keymap
```

The plan reuses waydriver's core, which provides an AT-SPI XPath locator and
an MCP server, and adds a Cage backend for capture, input, and output size.
[waydriver 0.3.10][waydriver] ships only Mutter backends and documents GTK
apps as its target. Whether its locator handles Thunderbird's Gecko tree and
Chrome's tree is the M1 question.

Structure first also suits local models. gpt-oss, the default model of
Codex's `--oss` mode per [`ollama/src/lib.rs` L16][codex-oss], has no vision
encoder in its [config][gpt-oss-20b-config], so it reads snapshots but not
screenshots. An agent whose model takes no images gets no screenshot tools,
and a step that needs pixels ends that posting with a `needs_human` row.

fill is a small MCP server. It reads one field from the unit's 1Password
vault, types it into the focused element over CDP, and returns `ok` without
the value. That keeps secrets out of transcripts by default. It cannot hide
them from the agent, because any client with CDP can read an input's value
with a script. The vault's scope is the control that matters.

## Agents run without approval prompts

Codex runs as `codex exec` with the `pilot` profile. In Codex 0.163.0, a
profile is a separate file, `$CODEX_HOME/pilot.config.toml`, layered over the
base config. The profile sets `approval_policy = "never"` and
`sandbox_mode = "danger-full-access"`, so a run never waits for a person.
`codex exec` has no `-a` flag; the profile or
`--dangerously-bypass-approvals-and-sandbox` sets this posture. All of this
comes from `codex exec --help` in the pinned dev shell.

The profile also selects the model: a cloud model, or a local provider whose
`base_url` reaches the unit's engine socket, as described under
[Local engines](#local-engines-replace-quota-limits-with-hardware-limits).

The agent's authority depends on the milestone:

- At M0 it runs directly on melchior as a dedicated user, with full access to
  that user's files.
- From M3 it runs as root inside a rootless podman container. Root there maps
  to an unprivileged user on the machine.

Chrome runs as a separate non-root user with its sandbox on, because Chrome
refuses to run as root without `--no-sandbox`. Inside a container, Chrome's
sandbox needs user namespaces, which the container's seccomp profile may
block. Check this at M3.

A systemd timer starts runs, and the dot never does. The user reports that
tasks the dot started stopped for approval about every 30 minutes.

## Icarus coordinates agents inside one unit on one machine

Panoptes adds Icarus at M3, when a unit first has two agents. These constraints
come from [Icarus at commit 9724641][icarus]:

- A unit has one Icarus group, and its database sits on a local volume.
  Icarus keeps one SQLite database per group in WAL mode, and network
  filesystems are outside its contract, per
  [`docs/protocol.md` L105-111][icarus-db].
- A group cannot span machines. Wakeups travel as Unix datagrams and there is no
  central daemon, per [`README.md` L158-169][icarus-shape]. On the rig, each
  unit has its own group, and nothing coordinates across units.
- Member and launch IDs prevent routing mistakes. They are not security
  tokens, per [`docs/protocol.md` L41][icarus-ids], so agents in one unit
  must trust each other.
- Linux is experimental for Icarus, per [`README.md` L12][icarus-linux]. Run
  `make test check` and `make verify-sync` on melchior before a unit depends
  on it.

The launcher appends native arguments after its own and does not reject
`exec`, per [`internal/launcher/launch.go` L77-145][icarus-launch]. This
launch should work. Not yet run.

```sh
icarus open codex jobpilot --group jobpilot --profile pilot -- \
  exec --json "$(cat run.md)"
```

Three gaps keep that command from producing a working unit:

- Templates drop MCP servers. The importer keeps only allow-listed settings.
  `approval_policy` and `sandbox_mode` are on the list, and `mcp_servers` is
  not, per [`internal/launcher/templates.go` L200-229][icarus-templates].
  Either Icarus gains an explicit MCP selection, like its `--instruction` and
  `--skill` options, or Panoptes writes its servers into the instance home,
  which Icarus does not support.
- Templates and the launcher also drop model endpoints. For Codex, the
  importer keeps `model` but not `model_provider` or `model_providers`, per
  the same lines, so a local provider must come through native `-c`
  arguments. For Claude Code, the launcher passes only an allowlist of
  environment variables that leaves out `ANTHROPIC_BASE_URL` and
  `CLAUDE_CODE_ATTRIBUTION_HEADER`, per [`launch.go` L147-158][icarus-env].
  An endpoint option in Icarus, like the MCP selection above, would fix
  both.
- Hooks need trust. Icarus installs hooks in each instance home. A run either
  relies on hook trust saved by an earlier interactive session in that home,
  or it passes `--dangerously-bypass-hook-trust`.

## A Nix flake declares each workload

The flake builds each workload declaration into a unit. At M0 the build is a
runner plus systemd user units for the machine. At M3 it is an OCI image from
`dockerTools.streamLayeredImage`. The attribute names below are a sketch, not
a settled interface.

```nix
lib.workloads.jobpilot = {
  kiosks.chrome = {
    app = "google-chrome";
    resolution = "1280x720";
    cdp = 9222;
    state = [ "chrome-profile" ];
  };
  agents.codex = {
    provider = "codex";
    model = "cloud"; # or an engine from lib.engines, from M4
    profile = ./workloads/jobpilot/pilot.config.toml;
    prompt = ./workloads/jobpilot/run.md;
    mcp = [ "chrome-devtools" "fill" ];
  };
  secrets.vault = "panoptes-jobpilot";
  schedule = {
    onCalendar = "<daily start time>";
    maxRuntime = "<run time limit>";
  };
};
```

- Declarations live under `lib`. `nix flake check` accepts `lib` but warns
  about an unknown top-level output such as `workloads`. Ran on 2026-10-24.
- A new workload gets its own unit. If it uses different accounts, it also
  gets its own vault.
- Engines are declared next to workloads, under `lib.engines`, with their
  server, model, and weights hash. An agent names its engine in `model`.
- At M0, jobpilot uses Fedora's `/usr/bin/google-chrome`. Google Chrome is
  unfree in nixpkgs, so the M3 image needs `allowUnfree` or Chromium.
  Installing Simplify from the Web Store into Chromium is untested.
- `flake.lock` pins every Nix tool. chrome-devtools-mcp comes from npm, so
  pin an exact version at M0 and package it with Nix at M3.

The planned layout:

```text
panoptes/
├─ flake.nix               dev shell today; workloads, runner, images later
├─ docs/architecture.md    this file
├─ runner/                 panoptes-run and systemd unit templates, M0
├─ desk/                   panoptes-desk, Rust, M2
├─ engine/                 engine proxy, M4
└─ workloads/
   ├─ jobpilot/            run.md, AGENTS.md, pilot.config.toml,
   │                       answers.example.yaml
   └─ mail/                M1
```

`answers.yaml` holds personal data and stays out of the repository. Only
`answers.example.yaml` lives here.

## State lives in four volumes

| Volume | Written by | Read by | Contents | Lifetime |
|---|---|---|---|---|
| `chrome-profile/` | Chrome | Chrome | sign-ins, cookies, Simplify | persistent, with a read-only btrfs snapshot after setup |
| `icarus/` | icarus-sync and native CLIs | Icarus | group database, instance homes with logins and hook trust | persistent |
| `ledger/` | agents | the dot and people | `queue.jsonl`, `applications.jsonl`, `needs-human.jsonl`, `shots/` | persistent and append-only |
| `runs/` | panoptes-run | people | one `codex exec --json` event stream per run | rotated |

The user that runs the ChatGPT app must be able to read `ledger/`.

Each `applications.jsonl` line is one JSON object with these fields:

```text
ts         RFC 3339 time the row was written
run        run ID
agent      agent label
url        posting URL
ats        greenhouse, lever, or ashby
company    company name
role       job title
status     submitting, submitted, queued, skipped, blocked, or needs_human
reason     why, for every status except submitted
answers    keys from answers.yaml that the agent used
evidence   screenshot path and confirmation text, required for submitted
```

Two rules make the ledger usable as evidence:

1. A `submitted` row carries the confirmation page text or the confirmation
   email. The agent's own summary never counts as evidence.
2. Rows are append-only. A correction is a new row.

Each agent's stdout and stderr go to journald with
`SyslogIdentifier=panoptes-<unit>-<agent>`.

A 1Password service account gives each unit read access to one vault, such as
`panoptes-jobpilot`. 1Password does not let service accounts reach the Personal,
Private, Employee, or default Shared vault, per
[Nautobot's summary of the rule][op-vaults]. The token reaches fill through a
systemd credential at M0 and a podman secret at M3. No other process reads
it.

## The unit is the trust boundary

| Boundary | Isolates | Does not isolate |
|---|---|---|
| unit, as a container from M3 | units from each other and from the machine | agents inside the unit, which share the network namespace, CDP ports, Wayland sockets, and files |
| kiosk | display, seat, clipboard, and AT-SPI tree | files, processes, and CDP |
| Icarus identity | message routing between agents | anything else, because it is not a security token |
| 1Password vault | which credentials exist in a unit | a credential from the agent that uses it |
| Chrome sandbox | a compromised page renderer from the rest of the unit | anything, if the container blocks user namespaces |
| engine and engine proxy | units' cached prompts from each other, when the engine salts its cache per credential domain | prompts from the engine itself, which reads every unit's requests |

Rules that follow:

1. One unit holds one credential domain. Workloads with different accounts
   get different units.
2. A unit holds nothing worth more than its own accounts. Web pages are
   attacker-controlled input, and the agent has full access inside the unit.
3. At M0 there is no container, so the machine is the boundary. The unit's user
   has no SSH keys to other machines and no personal password manager.
4. CDP and VNC listen on 127.0.0.1 only. Any process that reaches the CDP
   port controls the signed-in browser.
5. Engines run no tools. A tool run by a shared engine would act outside
   every unit's boundary, so every tool runs inside the agent's unit.
6. Units of different credential domains share an engine only if the engine
   salts its prefix cache per domain. Otherwise each domain gets its own
   engine.

## The host runs units the way Proxmox runs VMs

Status: proposed, 2026-10-24. The host is the layer above units. It turns a
bare-metal machine into an appliance whose basic object is a unit, the way a
VM is the basic object in Proxmox. Panoptes builds the host as a NixOS module in
this flake, so the appliance image, upgrades, and rollbacks come from NixOS
instead of a new distribution.

```text
host, NixOS with services.panoptes
├─ panoptes.slice
│  ├─ engine.slice                  AllowedCPUs=, MemoryMin=; from M4
│  │  ├─ engine-<model>.service     the only process with the GPU device
│  │  └─ engine-proxy.service       one socket per unit
│  ├─ unit-jobpilot.slice           CPUWeight=, MemoryMax=, IPAddressDeny=
│  │  ├─ kiosk-chrome.service       MemoryKSM= and freezing, if M4 says so
│  │  └─ agent-codex.service
│  └─ unit-mail.slice
├─ /nix/store                       read-only and shared by every unit
├─ /var/lib/panoptes/models         engine weights, pinned by hash
├─ btrfs subvolume per unit         state volumes and their snapshots
└─ console                          a noVNC tile per kiosk, the ledger and
                                    MCP trace per unit, a takeover button
```

The proposal behind this layer assigned a kernel mechanism to each concern.
Panoptes keeps each concern but uses the plainest mechanism that already ships,
and adds it only after an M4 measurement shows that the concern limits the
system.

| Concern | Panoptes mechanism | Added when |
|---|---|---|
| A unit is a first-class object | one systemd slice per unit, with one service per kiosk and per agent | M0 as services, M3 as containers |
| Idle kiosks use CPU while the model thinks | an MCP proxy in front of the kiosk thaws it before each tool call and freezes it after the reply settles, through the [cgroup v2 freezer][cgroup-v2] that `systemctl freeze` and `thaw` drive | M4 shows idle kiosk CPU limits density |
| Interactive actions wait behind batch work | `CPUWeight=` per unit, then [scx_layered][scx], an existing sched_ext scheduler with per-cgroup policy | M4 shows scheduling delay in turn time |
| Every unit's model requests share one accelerator | one engine per accelerator batches them, and the engine proxy caps each unit's concurrent requests, because [no cgroup v2 controller][cgroup-v2] schedules GPU compute | M4 |
| Engine threads wait behind kiosk work | `AllowedCPUs=` gives the engine slice its own cores and units the rest. ggml's CPU backend waits at a barrier after every graph node, per [`ggml-cpu.c` L3156-3158][ggml-barrier], so one preempted thread stalls the whole step | the first engine that computes on the CPU; for a GPU engine, when M4 shows decode stalls under kiosk load |
| Kiosk memory pressure evicts engine weights | [`MemoryMin=`][systemd-rc] on the engine service, or weights loaded without mmap | the first engine that keeps weights in host RAM |
| Kiosks hold duplicate memory | [`MemoryKSM=yes`][systemd-exec] on kiosk services | M4 shows the savings beat ksmd's CPU cost, and only among units of one credential domain |
| Units share most of their root filesystem | the shared read-only `/nix/store` | now, because Nix provides it |
| Agents can reach any address | systemd [`IPAddressAllow=` and `IPAddressDeny=`][systemd-rc], enforced with cgroup BPF | M3, for agent services; Chrome needs the open web |
| A unit's state needs rollback | btrfs snapshots of its volumes, then an app restart | M3 |
| People watch and take over kiosks | a web console that connects noVNC to [wayvnc's WebSocket mode][wayvnc-ws] | M5 |
| A running GUI should return to an earlier turn | a CRIU spike, not a feature | after M5 |

Freezing has costs. A frozen Chrome stops loading pages and running timers,
remote servers may drop its idle connections, and CDP clients wait until the
thaw. The MCP proxy therefore thaws the kiosk before it forwards each tool
call. A fast local engine also shortens the model waits in which freezing
saves CPU, so M4 measures the saving with cloud and local models separately.

The proposal also included mechanisms that Panoptes does not adopt:

- A thaw triggered by an eBPF socket hook. Cloud model traffic is TLS, so a
  kernel hook sees ciphertext and cannot tell a tool call from a token. A
  local engine's traffic is plaintext, but MCP runs over stdio pipes, which
  socket hooks do not see, and the MCP proxy already sees every tool call.
  What a freeze and thaw cost against model time is unmeasured until M4.
- sockmap and io_uring on the data path. A socket hop costs microseconds.
  A turn's model time and its local work, such as page loads, snapshots, and
  screenshots, each take milliseconds or more, even with a fast local
  engine. This is an estimate until M4 splits turn times. An io_uring SQPOLL
  ring also keeps a kernel thread polling, which spends the CPU that freezing
  saves. In 2023, 60% of the exploits submitted to
  [Google's kernel bug bounty][kernelctf] used io_uring, and Google disabled
  it on ChromeOS and its production servers. A machine that renders
  untrusted web pages should not add that attack surface.
- A kernel-level TLS multiplexer for model traffic. A connection pool would
  be a userspace proxy, and Codex and Claude Code already report token usage
  in their own output. A local engine has no TLS to multiplex.
- KSM through an `LD_PRELOAD` shim. `MemoryKSM=` turns KSM on for a service
  through prctl. KSM merges pages across every process that opts in. Merged
  pages leak information between processes, because a write to a merged
  page takes measurably longer, and [Flip Feng Shui][flip-feng-shui] combined
  KSM merging with Rowhammer to corrupt another VM's memory. KSM therefore
  stays within one credential domain.
- CRIU time travel as a safety net. A restore rolls back local memory, not
  the outside world, so a submitted application stays submitted. Restoring
  an old checkpoint also breaks live TCP and TLS connections, because the
  remote ends have moved on. The spike checkpoints a Cage kiosk running
  Chrome and stops at the first restore that does not give a usable app.

## Local engines replace quota limits with hardware limits

Status: proposed, 2026-10-24. An agent's model is a cloud API or a local
engine on the host. With cloud models, the subscription's rate limits will
likely cap the number of agents before the rig's CPU or RAM does. With a
local engine, the accelerator and RAM set that cap, and the host's resource
controls start to matter. A local engine also keeps prompts on the machine.
Prompts carry `answers.yaml` values, page text, and email.

How fast an engine runs depends on the rig's accelerator, which is not
recorded yet, per [open decision 9](#open-decisions). Without a GPU or a
unified-memory accelerator, an engine is slow. For gpt-oss-20b on the CPU,
prefill costs at least 4 to 7 s per 1,000 new tokens, and decode costs about
40 to 50 ms per token. A turn that adds a few thousand tokens and writes a
few hundred therefore takes tens of seconds. This is an estimate: 7.2 GFLOP
per prefilled token at 1 to 2 TFLOPS sustained, and 3 to 4 GB read per
decoded token at about 80 GB/s, which assumes dual-channel DDR5. A discrete
GPU has one to two orders of magnitude more compute and memory bandwidth,
and cuts both costs by about as much. This is also an estimate.

A turn never takes less than its tool time, the page load, snapshot, or
screenshot that the model waits for. Once the model takes less time than its
tools, a faster engine mostly adds agents per accelerator instead of
shortening runs. The run log's stamps show where that point is.

One engine per accelerator serves every unit, unless the cache rules below
require one per credential domain. In each decode step, an engine reads each
weight that its batch needs once, however many sequences use it. One engine
that batches N agents therefore shares those reads among them, where N
engines would each make their own, and it holds one copy of the weights
instead of N. Each agent then costs its KV cache: about 36 KiB per token for
gpt-oss-120b, which fits one 80 GB GPU, and 24 KiB per token for
gpt-oss-20b, which runs in 16 GB, per the [model card][gpt-oss]. A
64k-token transcript holds 2.25 GiB or 1.5 GiB. These figures come from the
[120b][gpt-oss-120b-config] and [20b][gpt-oss-20b-config] configs, for a
16-bit cache: 18 or 12 full-attention layers, times 8 KV heads of 64
dimensions, times 2 bytes, for each of K and V. The other layers attend over
a 128-token window and add a few megabytes.

The agent CLIs set which API an engine must serve. Codex 0.163.0 speaks only
the Responses API, per [`lib.rs` L107-111][codex-wire]. Claude Code speaks
the Anthropic Messages API to `ANTHROPIC_BASE_URL`, per its
[gateway guide][cc-gateway]. The pinned nixpkgs has three engines that serve
both. They differ in whether units can share a prefix cache safely:

| Engine | Version | `/v1/responses` | `/v1/messages` | Cache salt per domain |
|---|---|---|---|---|
| vLLM | 0.29.0 | [yes][vllm-responses] | [yes][vllm-messages] | yes, [`cache_salt`][vllm-salt] |
| llama.cpp `llama-server` | v0.6.0 | [yes][llama-routes] | [yes][llama-routes] | no |
| Ollama | 0.40.6 | [yes][ollama-routes] | [yes][ollama-routes] | no `cache_salt` in its source |

llama-server shares one KV buffer across its slots, keeps an 8 GiB prompt
cache in host RAM, and sends each request to the slot whose cached prompt it
most resembles, all by default, per its README ([cache][llama-cache],
[slots][llama-sps]). Units of two credential domains therefore need two
llama-server processes, and on a GPU each holds its own copy of the weights.
vLLM is the default engine where it supports the accelerator, because its
salt lets one batch mix credential domains.

Units reach an engine only through the engine proxy:

- Each unit gets its own Unix socket, and the socket a unit can open is its
  identity. In a container, a unit mounts only its own socket. A forwarder
  inside the unit serves it on 127.0.0.1, because both CLIs take an HTTP
  base URL.
- The proxy forwards only inference routes. Ollama serves model management,
  such as `/api/pull` and `/api/delete`, on the same port as inference, per
  [`routes.go` L2368-2414][ollama-routes].
- The proxy fixes each unit's model name and cache salt. Ollama forwards a
  request that names one of its cloud models to ollama.com, per
  [`cloud_proxy.go` L74-137][ollama-cloud], so a unit that chose its own
  model name could send its prompts off the machine.
- The proxy caps each unit's concurrent requests, because an agent has full
  access inside its unit and can call the socket directly.

vLLM can also run built-in and MCP tools inside the engine. Panoptes leaves
that off, per [rule 5](#the-unit-is-the-trust-boundary). Salting has failed
on that path before: vLLM before 0.30.0 dropped `cache_salt` on server-side
tool continuations, per [GHSA-935w-9g4m-p28p][vllm-ghsa]. The pinned nixpkgs
ships 0.29.0, so M4 pins 0.30.0 or later.

Claude Code needs two more settings against a local engine:

- `CLAUDE_CODE_ATTRIBUTION_HEADER=0`. Without it, Claude Code puts a
  conversation fingerprint at the start of the system prompt, and a
  non-Anthropic engine receives it as part of the prompt and its cache key,
  per the [gateway guide][cc-gateway].
- WebFetch off. Its domain safety check calls api.anthropic.com directly even
  when `ANTHROPIC_BASE_URL` is set, per the same guide. Agents browse through
  Chrome instead.

A local model may make more mistakes than a cloud model, and a submitted
application stays submitted. The submit policy therefore counts errors for
each pair of ATS and model, so a new local model starts by queueing, per
[open decision 1](#open-decisions). A unit can also pair a fast local agent
that fills forms with a checker on a stronger model. The checker compares
the filled fields with `answers.yaml` before the `submitting` row, so the
stronger model spends tokens only on the step that cannot be undone. From
M3, the filler hands each posting to the checker through Icarus.

Engine weights are pinned by hash in the flake and fetched into
`/var/lib/panoptes/models` instead of the Nix store, so the appliance image
stays small.

## Workloads

| Workload | Kiosk app | Primary control | Milestone |
|---|---|---|---|
| jobpilot | Google Chrome with Simplify | CDP | M0 |
| mail | Thunderbird | AT-SPI through panoptes-desk | M1 and M2 |
| stress | synthetic apps | recorded MCP calls replayed without a model | M4 |

jobpilot follows these rules:

- It applies through Greenhouse, Lever, and Ashby posting URLs. Workday is
  out, per [Decisions](#decisions), and LinkedIn is out unless the user
  decides otherwise, per [Open decisions](#open-decisions).
- Answers come only from `answers.yaml`. A question that is not there stops
  that posting and adds a `needs_human` row.
- Verification emails go through himalaya over IMAP, not through the mail
  kiosk, so jobpilot does not depend on the AT-SPI path.

mail tests the AT-SPI path on an app that is not GTK 4. Thunderbird uses
Gecko.

stress separates two questions on the rig. A replay harness sends recorded
MCP calls with no model, which measures how many units fit. A few real agents
measure throughput. Many real agents on one subscription would mostly
measure its rate limits. Against a local engine, they measure the rig.

## Machines

| Machine | Facts | Role |
|---|---|---|
| melchior, `rhegde@10.40.251.150` | ThinkPad T420 with an i7-2620M, 4 threads, and 7.6 GiB RAM, about 2.3 GiB free when checked. Fedora 43 with kernel 6.17.12 and systemd 258.3. The kernel has sched_ext, BPF LSM, KSM, checkpoint/restore, and io_uring built in; BPF LSM is active, KSM is off, and sched_ext is idle. btrfs `/home`. Nix, Google Chrome, and the ChatGPT app installed. Podman, CRIU, and scx not installed. User linger off. Checked over SSH on 2026-10-24 | M0 to M3, and the dot's connected computer |
| the rig | 32 threads and 128 GB RAM, as reported by the user. Its accelerator is not recorded, and its OS is not chosen | M4 and M5, and local engines |

- melchior's RAM fits one or two Chrome kiosks next to the desktop session
  and the ChatGPT app. This is an estimate.
- melchior cannot hold an engine, because gpt-oss-20b alone runs in 16 GB.
  Its units can reach an engine on the rig through an SSH forward of their
  engine socket, the way people reach VNC.
- Linger is off, so user services stop when the user logs out. M0 needs
  `loginctl enable-linger` for the user that runs the unit.
- The dot connects to one computer at a time, and the ChatGPT app must be
  open there. Reports from rig units need another path to the dot.

## Milestones

| | Scope | Done when |
|---|---|---|
| M0 | jobpilot directly on melchior, with a Cage kiosk running Chrome, chrome-devtools-mcp, `codex exec` on a timer, the ledger, and dot reports | runs start on schedule with zero approval prompts, every `submitted` row has evidence, and run logs split each turn into model time and tool time |
| M1 | an AT-SPI probe that runs waydriver's locator against Thunderbird and Chrome in a Cage kiosk with a private bus | XPath finds Thunderbird's Get Messages button and an AT-SPI action triggers it. If not, Panoptes writes its own locator |
| M2 | panoptes-desk with the Cage backend | M1's check passes through MCP, and a calibration click lands where AT-SPI places the element |
| M3 | OCI units built by the flake, rootless podman on melchior, and Icarus inside units | M0's loop runs unchanged in a container, Icarus's Linux checks pass on melchior, and the template gaps have answers |
| M4 | units at density on the rig, against cloud models and a local engine behind its proxy | measurements exist for units per machine, the limit that binds first, idle kiosk CPU during model waits, KSM savings, how turn time splits between model wait and local work for each model, and agents per accelerator at the target model time per turn |
| M5 | the host: a NixOS module, an appliance image, the web console, the engine and its proxy, and the M4-gated mechanisms that passed | a fresh machine boots the image and runs the jobpilot and mail units from the flake, and the console shows a kiosk and takes it over |

## Decisions

Accepted on 2026-10-24 unless marked proposed. Supersede an entry with a new
one instead of editing it.

| Decision | Why | Reopen when |
|---|---|---|
| One Cage kiosk per app instead of one Sway with an output and a seat per agent | Each app owns seat0, focus and clipboard never collide, and placement is predictable. Multi-seat Sway lost because toolkit support for secondary seats is uncertain and no workload moves windows between agents | an agent needs two apps side by side, or windows must move between agents |
| CDP for Chrome, AT-SPI for other apps, pixels last | Structure gives element handles. Pixels need coordinate guesses and cost image tokens | a needed app exposes neither |
| `codex exec` on a timer instead of OpenClaw | OpenClaw runs the same Codex app-server with approvals off by default, per [its docs][openclaw], adds a second always-on orchestrator, and has had critical CVEs such as CVE-2026-25253 | Codex drops unattended exec |
| The dot reports and never starts runs | Tasks the dot started stopped for approval about every 30 minutes | the dot can start tasks without prompts |
| jobpilot does not use the dot's cloud browser | Its Chromium disables Add to Chrome for Simplify, as the user found on 2026-10-24 | it allows Web Store installs |
| jobpilot v1 skips Workday | Each employer needs its own account and email verification, and the user judges most Workday postings not worth it | the user wants Workday postings |
| No tailscale. VNC listens on loopback, and people reach it through SSH | SSH to melchior already works | VNC is needed from outside the LAN |
| Proposed: Nix builds OCI images, and rootless podman runs them on melchior | Podman is rootless and logs to journald, and the same image runs under Docker on the rig | podman cannot run a unit |
| Proposed: the agent gets root only inside containers | Root directly on melchior would expose the whole machine to web page content | M3 slips and melchior itself needs root |
| Proposed: the host is a NixOS module in this flake, not a new distribution | NixOS already provides an immutable store, atomic upgrades, rollback generations, and image builds, and workloads are already flake declarations | the rig cannot run NixOS |
| Proposed: each kernel mechanism waits for an M4 measurement | The proposal's figures, such as agents idle 85% of the time and 85% of memory identical across kiosks, are unmeasured | a mechanism is needed before M4 runs |
| Proposed: no sockmap, io_uring, eBPF socket triggers, or kernel TLS proxy | The reasons are in the host section | M4 shows the local data path in turn time |
| Proposed: local engines are host services that units share through per-unit proxy sockets | An engine reads each weight once per decode step for its whole batch and holds one copy of the weights. Only the engine needs the GPU device, which keeps the GPU driver out of units | a unit needs its own model |
| Proposed: engines run no tools, and share a prefix cache only among units of one credential domain | A shared engine is outside every unit's boundary. A shared cache answers cached prompts faster, which can tell one user what another sent, per [Gu et al.][prompt-cache-audit] | an engine isolates tools and caches per tenant by itself |

## Open decisions

1. jobpilot's submit policy: queue every application for a person, or submit
   automatically. The proposed default queues the first 50, then submits
   automatically for each pair of ATS and model once that pair has zero
   errors.
2. LinkedIn stays out unless the user says otherwise. Its user agreement bans
   bots, and LinkedIn restricts accounts that use them, per
   [LinkedIn's help page][linkedin].
3. The M0 user: a dedicated `panoptes` user, `rhegde`, or root. The proposal is
   a dedicated user with linger on.
4. The Icarus template gaps: add MCP and endpoint selection to Icarus, or let
   Panoptes write instance homes.
5. Takeover: how an agent yields the seat when a person connects over VNC.
   Until this is settled, stop the run first.
6. The path from rig units to the dot.
7. The rig's OS. The host layer assumes NixOS there.
8. Where the web console listens. Without tailscale, the choices are the LAN
   or loopback behind SSH.
9. The rig's accelerator: which GPU or unified-memory part it has, and how
   much memory that part has. It decides whether a local engine is fast,
   which engine runs, and which model fits.
10. The target model time per turn. The proposed default is the tool time
    per turn that M0 measures, because past that point a faster engine
    mostly adds agents instead of shortening runs.

## Sources

The claims above cite these versions, checked on 2026-10-24:

- [cage v0.3.1][cage], from the nixpkgs that `flake.lock` pins
- [wlroots 0.20.2][wlroots], which cage 0.3.1 builds against
- [wayvnc v0.10.2][wayvnc]
- [Icarus at 9724641][icarus]
- [Codex CLI 0.163.0][codex], from the same nixpkgs
- [waydriver 0.3.10][waydriver]
- [scx 1.1.4][scx] and CRIU 4.1.1, from the same nixpkgs
- kernel 6.17.12 and systemd 258.3, on melchior
- vLLM 0.29.0, llama.cpp v0.6.0, and Ollama 0.40.6, from the same nixpkgs
- the [gpt-oss model card][gpt-oss] and configs, and Claude Code's
  [gateway guide][cc-gateway]

To read a pinned source locally, run
`nix build --inputs-from . nixpkgs#cage.src --print-out-paths` and open the
printed path.

[cage]: https://github.com/cage-kiosk/cage/tree/v0.3.1
[cage-view]: https://github.com/cage-kiosk/cage/blob/v0.3.1/view.c#L79-L104
[cage-exit]: https://github.com/cage-kiosk/cage/blob/v0.3.1/cage.c#L97-L114
[cage-protocols]: https://github.com/cage-kiosk/cage/blob/v0.3.1/cage.c#L480-L556
[wlroots]: https://gitlab.freedesktop.org/wlroots/wlroots/-/tree/0.20.2
[wlr-default]: https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/0.20.2/backend/backend.c#L227-229
[wlr-frames]: https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/0.20.2/backend/headless/output.c#L113-151
[wlr-refresh]: https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/0.20.2/include/backend/headless.h#L7
[wayvnc]: https://github.com/any1/wayvnc/tree/v0.10.2
[wayvnc-req]: https://github.com/any1/wayvnc/blob/v0.10.2/src/main.c#L418-L431
[chrome-136]: https://developer.chrome.com/blog/remote-debugging-port
[atspi-72]: https://gitlab.gnome.org/GNOME/at-spi2-core/-/issues/72
[gtk-6343]: https://gitlab.gnome.org/GNOME/gtk/-/issues/6343
[waydriver]: https://docs.rs/crate/waydriver/latest
[icarus]: https://github.com/Roshyoshi/icarus/tree/9724641e381b3465b58b9ea4115315d5e4c33497
[icarus-db]: https://github.com/Roshyoshi/icarus/blob/9724641e381b3465b58b9ea4115315d5e4c33497/docs/protocol.md?plain=1#L105-L111
[icarus-shape]: https://github.com/Roshyoshi/icarus/blob/9724641e381b3465b58b9ea4115315d5e4c33497/README.md?plain=1#L158-L169
[icarus-ids]: https://github.com/Roshyoshi/icarus/blob/9724641e381b3465b58b9ea4115315d5e4c33497/docs/protocol.md?plain=1#L41
[icarus-linux]: https://github.com/Roshyoshi/icarus/blob/9724641e381b3465b58b9ea4115315d5e4c33497/README.md?plain=1#L12
[icarus-launch]: https://github.com/Roshyoshi/icarus/blob/9724641e381b3465b58b9ea4115315d5e4c33497/internal/launcher/launch.go#L77-L145
[icarus-templates]: https://github.com/Roshyoshi/icarus/blob/9724641e381b3465b58b9ea4115315d5e4c33497/internal/launcher/templates.go#L200-L229
[op-vaults]: https://docs.nautobot.com/projects/secrets-providers/en/latest/admin/providers/onepassword_setup/
[openclaw]: https://docs.openclaw.ai/plugins/codex-harness-reference/approval-and-sandbox.md
[linkedin]: https://www.linkedin.com/help/linkedin/answer/a1341387
[cgroup-v2]: https://docs.kernel.org/admin-guide/cgroup-v2.html
[scx]: https://github.com/sched-ext/scx/tree/main/scheds/rust/scx_layered
[systemd-exec]: https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html
[systemd-rc]: https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html
[wayvnc-ws]: https://github.com/any1/wayvnc/blob/v0.10.2/src/main.c#L978-L990
[kernelctf]: https://security.googleblog.com/2023/06/learnings-from-kctf-vrps-42-linux.html
[flip-feng-shui]: https://www.usenix.org/conference/usenixsecurity16/technical-sessions/presentation/razavi
[codex]: https://github.com/openai/codex/tree/83086188ae0614bd882818ff1aa23f3194962012
[codex-wire]: https://github.com/openai/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/model-provider-info/src/lib.rs#L107-L111
[codex-oss]: https://github.com/openai/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/ollama/src/lib.rs#L16
[codex-events]: https://github.com/openai/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/exec/src/exec_events.rs
[icarus-env]: https://github.com/Roshyoshi/icarus/blob/9724641e381b3465b58b9ea4115315d5e4c33497/internal/launcher/launch.go#L147-L158
[gpt-oss]: https://huggingface.co/openai/gpt-oss-120b
[gpt-oss-120b-config]: https://huggingface.co/openai/gpt-oss-120b/blob/main/config.json
[gpt-oss-20b-config]: https://huggingface.co/openai/gpt-oss-20b/blob/main/config.json
[cc-gateway]: https://code.claude.com/docs/en/llm-gateway-protocol
[vllm-responses]: https://github.com/vllm-project/vllm/blob/v0.29.0/vllm/entrypoints/openai/responses/api_router.py
[vllm-messages]: https://github.com/vllm-project/vllm/blob/v0.29.0/vllm/entrypoints/anthropic/api_router.py
[vllm-salt]: https://github.com/vllm-project/vllm/blob/v0.29.0/vllm/entrypoints/openai/responses/protocol.py#L253
[vllm-ghsa]: https://github.com/vllm-project/vllm/security/advisories/GHSA-935w-9g4m-p28p
[llama-routes]: https://github.com/ggml-org/llama.cpp/blob/v0.6.0/tools/server/server.cpp#L276-L280
[llama-cache]: https://github.com/ggml-org/llama.cpp/blob/v0.6.0/tools/server/README.md?plain=1#L167-L168
[llama-sps]: https://github.com/ggml-org/llama.cpp/blob/v0.6.0/tools/server/README.md?plain=1#L242
[ggml-barrier]: https://github.com/ggml-org/llama.cpp/blob/v0.6.0/ggml/src/ggml-cpu/ggml-cpu.c#L3156-L3158
[ollama-routes]: https://github.com/ollama/ollama/blob/v0.40.6/server/routes.go#L2368-L2414
[ollama-cloud]: https://github.com/ollama/ollama/blob/v0.40.6/server/cloud_proxy.go#L74-L137
[prompt-cache-audit]: https://arxiv.org/abs/2502.07776
