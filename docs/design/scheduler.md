# Scheduler: each host decides which agents stay resident

Research notes, 2026-10-26: what an agent container is, and what
schedules it. A kiosk is one Cage and the app it runs, an agent is the
loop that drives kiosks, and a unit is the trust boundary around both.
Numbers come from the test server, described in
[measurements.md](measurements.md).

## Summary

- An agent container is a cgroup tree. Panoptes creates each agent's tree
  itself and starts its processes in it with `clone3(CLONE_INTO_CGROUP)`;
  neither systemd nor dockerd manages agents.
- Density comes from residency, not placement. A residency manager on each
  host moves every kiosk between running, frozen, parked in compressed
  memory, and evicted to disk.
- A cluster scheduler cannot run an agent's lifecycle: Kubernetes can
  neither pause nor compress a pod.

## An agent container is a cgroup tree

```text
panoptes.slice
├─ unit-jobpilot.slice              one credential domain
│  ├─ kiosk-chrome.service          cage and chromium, frozen and parked
│  │                                as one cgroup
│  └─ agent-codex.service           the loop; never frozen while a model
│                                   response is streaming
└─ engine.slice                     vLLM, the only process with the GPU
```

The container mechanism costs almost nothing next to the browser. Fifty
instances per runtime of the same `sleep` from the shared `/nix/store`, all
in one slice; start is the time for the start command to return:

| Runtime | Processes each | Charge KiB each | PSS KiB each | Start p50 ms | Start max ms |
|---|---|---|---|---|---|
| crun | 1 | 428 | 207 | 11 | 36 |
| runc | 1 | 555 | 207 | 18 | 43 |
| systemd service, `RootDirectory` | 1 | 742 | 418 | 9 | 17 |
| systemd service, sandboxing on | 1 | 760 | 407 | 9 | 19 |
| Podman with crun, rootful | 2 | 963 | 505 | 102 | 112 |
| bwrap in a service | 3 | 1,163 | 705 | 9 | 13 |
| systemd-nspawn in a service | 3 | 2,876 | 2,185 | 9 | 17 |
| Docker with containerd and runc | 2 | 5,889 | 4,789 | 152 | 191 |

The sandboxing row sets `DynamicUser`, `PrivateUsers`, `PrivateNetwork`,
`PrivateTmp`, `PrivateIPC`, `ProtectSystem=strict`, and `ProtectHome`.
`systemd-run` returns once the process forks, so its start times do not
include nspawn's setup.

Even Docker's cost is 3 percent of a lean kiosk. Docker images are fine as
the packaging format. dockerd adds a daemon and a Go shim per container,
which is most of its 5.9 MiB, and its
[checkpoint command][docker-checkpoint] is experimental and restores only
into the same container.

## The agent loop costs half a lean kiosk

Each loop idle at its first screen, with no credentials, in its own scope:

| Loop | Charge MiB | PSS MiB, one instance | Threads |
|---|---|---|---|
| Node.js 24, an empty event loop | 11 | 49 | 7 |
| Claude Code 2.1.308, at its welcome screen | 85 | 197 | 19 |
| Codex 0.163.0, stopped at a terminal query the pty never answers | 84 | 174 | 49 |

A one-instance PSS includes the binary's file pages, which every instance
shares, so the charge is the cost of one more agent. With lean flags, a
kiosk at density costs about 172 MiB of PSS, so a loop process per agent
adds half again before it holds any history. A working session also starts
each MCP server as its own process.

1. Budget an agent as kiosk plus loop.
2. Run one MCP server per kiosk, panoptes-desk from
   [desktop.md](desktop.md), instead of one process per tool family.
3. Run many loops in one process where the harness allows it.

## Kubernetes cannot park an agent

| The host needs to | Kubernetes | The host kernel and systemd |
|---|---|---|
| pause an agent | has no pod pause | `cgroup.freeze`, through [`systemctl freeze`][systemctl] |
| compress an idle agent | has no equivalent | `memory.reclaim` into zswap, capped by `memory.zswap.max` ([cgroup v2][cgroup-v2]) |
| checkpoint and restore | the [kubelet checkpoint][kubelet-checkpoint] is forensic, with no restore API; pod checkpoint, [KEP-5823][kep-5823], is alpha and leaves out suspend and resume | evict to disk: the browser profile and the loop's session file |
| make room | [Kueue preemption][kueue] deletes pods | park or evict the least recently active agent |
| fit agents on a node | supports [110 pods per node][k8s-large]; EKS reserves [255 MiB plus 11 MiB per pod][eks-reserve] | no unit limit; 64 running agents cost 258 MiB each with default flags, so 125 GiB holds about 490 running or about 2,000 parked kiosks; with lean flags, about 700 running or 3,000 parked (arithmetic) |

Platforms that run sandboxes at density put the lifecycle in a per-host
daemon:

- AWS Lambda gives each worker a MicroManager that owns slot state
  ([Firecracker, NSDI 2020][firecracker-nsdi]).
- [E2B][e2b-infra] runs its API and orchestrator as Nomad jobs. Its
  sandboxes are Firecracker VMs that the orchestrator manages, not Nomad
  allocations.
- Unikraft wrote its own controller to hold a
  [million scaled-to-zero VMs on one host][unikraft-1m] (vendor claim).

## The residency manager moves kiosks between four states

| State | Kiosk | Memory held | Back to running | Cost |
|---|---|---|---|---|
| running | thawed | 258 MiB at 64 agents with default flags, 175 with lean flags | | |
| frozen | `cgroup.freeze` set | unchanged | thaw; first screenshot 29 ms after the thaw write | 6 ms to freeze |
| parked | frozen, swap-backed memory reclaimed into zswap | 64 MiB with zstd, 86 with lz4, for a 280 MiB Workday kiosk; 46 MiB with zstd for a 200 MiB lean one | thaw and fault pages back; first screenshot after 45 to 70 ms | 0.3 (lz4) to 0.9 s (zstd) of reclaim; about 250 MiB of swap slots |
| evicted | stopped; profile and session file on disk | none | relaunch on the profile; page complete after 1.3 s | page state the profile does not hold is lost |

[kernel-and-inference.md][parking] has the measurements.

[parking]: kernel-and-inference.md#parking-is-the-kernel-lever-that-pays
[between-steps]: desktop.md#between-steps-freeze-the-kiosk

```text
running  model call starts         -> frozen     panoptes-desk freezes the kiosk
frozen   tool call arrives         -> running    panoptes-desk thaws, then acts
frozen   idle past T, or pressure  -> parked     memory.reclaim into zswap
parked   idle past T2, or no room  -> evicted    stop the kiosk service
```

Rules:

1. Freeze the kiosk, never the loop. The loop holds the model's response
   stream, and the kiosk has nothing to do until the reply arrives. A
   frozen kiosk also spends no CPU: between steps, a visible CNN page used
   3 cores per agent and a frozen one a fifth of a core
   ([desktop.md][between-steps]).
2. panoptes-desk owns freeze and thaw, because every tool call passes through
   it. It thaws the kiosk before it forwards an action.
3. Park swap-backed memory, and warm the browser build from a cgroup that
   is never parked. Of a Workday kiosk's 65 MiB file charge, 57 MiB was
   shmem, which parks into zswap like anonymous memory; its own page cache
   was 8 MiB. The build stays charged to the cgroup that first read it
   ([cgroup v2][cgroup-v2]), so parking the kiosk that read it first would
   evict it for every kiosk. On 6.12 with MGLRU, the reclaim write cannot
   spare file pages: `swappiness=200` left none of the 8 MiB, and a target
   sized to anonymous memory plus shmem left 2. [6.18][cgroup-v2-618] adds
   `swappiness=max`, which reclaims anonymous memory only.
4. Set `memory.zswap.writeback=0` on parked kiosks. With writeback on, the
   zswap shrinker wrote 205 to 242 MiB of the kiosk to the swap device in
   11 of 13 reclaim runs, and the first screenshot after thaw took 146 to
   200 ms instead of 45 to 70.
5. Size swap for parking. Each zswap entry holds a swap slot, so parked
   memory cannot exceed the swap size even when nothing reaches the disk. A
   parked kiosk held about 250 MiB of slots, so 1,000 parked kiosks need
   about 245 GiB of swap that is never written.
6. Bound every reclaim write with a timeout. On 6.12, a target above the
   reclaimable memory of a cgroup that holds only kernel memory makes the
   write spin for minutes.
7. Evict by stopping the kiosk, and resume by relaunching it. CRIU cannot
   dump a sandboxed Chromium: Chromium creates
   [nested user, PID, and network namespaces][chromium-ns], and
   [CRIU refuses nested namespaces][criu-ns]. CRIU 4.1.1 on the test server
   stopped with "Can't dump nested pid namespace". With the sandbox off, it
   stopped on wlroots' unlinked 3.6 MB frame buffer, above its 1 MiB limit
   for deleted files, and with that limit raised, on a systemd-logind
   inhibitor descriptor. An agent that browses untrusted pages keeps its
   sandbox, so this path stays closed.
8. Freeze through systemd, and thaw before stopping. On systemd 260,
   `systemctl stop` refuses a unit that `systemctl freeze` froze. A unit
   frozen by a direct `cgroup.freeze` write waited in `stop-sigterm` for
   the full 90 s `TimeoutStopSec`, then got SIGKILL.

## Agents get CPU by weight, never by cap

On the 32-thread test server, one acting kiosk clicks a button, waits two
frames, and takes a JPEG screenshot 100 times, while 64 sibling services in
`panoptes.slice` each burn a core:

| Policy | p50 ms | p95 | p99 | max |
|---|---|---|---|---|
| siblings stopped, idle host | 49.5 | 50.1 | 50.2 | 56.9 |
| equal weights, the default | 73.6 | 90.1 | 97.6 | 169.1 |
| acting kiosk and its client at `cpu.weight=10000` | 43.4 | 56.4 | 58.2 | 71.3 |
| siblings at `cpu.idle=1` | 46.2 | 56.1 | 68.8 | 71.5 |
| siblings capped at half a CPU each with `cpu.max` | 78.8 | 100.1 | 110.9 | 132.9 |
| siblings frozen | 49.5 | 57.6 | 57.8 | 58.0 |

- Equal weights slow the acting kiosk by half at p50 and double its p99.
- A high weight on the acting kiosk, or `cpu.idle` on its siblings,
  restores the idle host's p50. The busy host is even a little faster at
  p50; idle-state exits on the idle host are a likely cause (inference, not
  measured).
- Half-CPU caps protect nothing, because 64 halves still fill 32 threads. A
  cap protects only by leaving CPUs idle, and a weight needs no idle CPUs.

Caps have a second problem. `cpu.max` throttles a group for the rest of
its period once its quota runs out, and Chromium's bursty, multithreaded
work hits that edge. On 6.12 a throttled group also stalls whoever waits on
a lock that one of its tasks holds. [Task-based throttling][task-throttle]
fixes that and was queued for 6.18 (inference from the tip tree's date).

## Alternatives

| Option | Why it loses |
|---|---|
| Docker and Kubernetes | no pause or restore; Kueue deletes pods to make room; an 11 MiB reservation per pod on EKS |
| k3s | the same kubelet semantics; its [agent alone uses 275 MB][k3s-profile] |
| Nomad | places services well, but has only an Enterprise [`alloc pause`][nomad-pause] |
| Incus system containers | pause works, but [CRIU migration][incus-move] works only for "very basic containers" |
| Proxmox, a VM per agent | QEMU costs about 131 MB per VM ([NSDI 2020][firecracker-nsdi]), and Proxmox [turns KSM on across VMs][pve-ksm] by default |

## Sources

Checked on 2026-10-26.

[docker-checkpoint]: https://docs.docker.com/reference/cli/docker/checkpoint/
[systemctl]: https://www.freedesktop.org/software/systemd/man/latest/systemctl.html
[cgroup-v2]: https://docs.kernel.org/admin-guide/cgroup-v2.html
[cgroup-v2-618]: https://www.kernel.org/doc/html/v6.18/admin-guide/cgroup-v2.html
[kubelet-checkpoint]: https://kubernetes.io/docs/reference/node/kubelet-checkpoint-api/
[kep-5823]: https://www.kubernetes.dev/resources/keps/5823/
[kueue]: https://kueue.sigs.k8s.io/docs/overview/
[k8s-large]: https://kubernetes.io/docs/setup/best-practices/cluster-large/
[eks-reserve]: https://docs.aws.amazon.com/batch/latest/userguide/memory-cpu-batch-eks.html
[firecracker-nsdi]: https://www.usenix.org/system/files/nsdi20-paper-agache.pdf
[e2b-infra]: https://github.com/e2b-dev/infra
[unikraft-1m]: https://unikraft.com/blog/1m-vms-single-box
[chromium-ns]: https://chromium.googlesource.com/chromium/src/+/main/sandbox/linux/services/namespace_sandbox.cc
[criu-ns]: https://github.com/checkpoint-restore/criu/blob/criu-dev/criu/namespaces.c
[bb-contexts]: https://docs.browserbase.com/features/contexts
[task-throttle]: https://lkml.iu.edu/hypermail/linux/kernel/2509.0/04201.html
[chv-snapshot]: https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/docs/snapshot_restore.md
[gvisor-cr]: https://gvisor.dev/docs/user_guide/checkpoint_restore/
[runtime-study]: https://www.scitepress.org/Papers/2021/104405/104405.pdf
[mig]: https://docs.nvidia.com/datacenter/tesla/mig-user-guide/supported-gpus.html
[vgpu]: https://docs.nvidia.com/vgpu/gpus-supported-by-vgpu.html
[k3s-profile]: https://docs.k3s.io/reference/resource-profiling
[nomad-pause]: https://developer.hashicorp.com/nomad/commands/alloc/pause
[incus-move]: https://linuxcontainers.org/incus/docs/main/howto/move_instances/
[pve-ksm]: https://pve.proxmox.com/pve-docs/chapter-sysadmin.html
