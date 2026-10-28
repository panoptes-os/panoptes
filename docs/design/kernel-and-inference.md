# Kernel and inference: memory is the lever, the network is not

Research notes, 2026-10-26: where kernel scheduling, network I/O, and
local inference can raise agent density.
Numbers come from the test server, described in
[measurements.md](measurements.md); the residency states it refers to are
in [scheduler.md](scheduler.md).

## Summary

- Memory: parked in zswap with zsmalloc and zstd, an idle kiosk holds
  about a quarter of its running charge. Writeback stays off while it is
  parked, so swap must hold the parked total.
- CPU: Panoptes sets `cpu.weight` on EEVDF. sched_ext was not tried; the
  test server's kernel lacks it.
- KSM saved 8 percent across 64 agents of one build. Panoptes does not
  turn it on.
- Network: the kernel needs no changes. A new TLS connection to a model
  API took 22 to 51 ms against a step of seconds, and the kernel's fast
  paths save microseconds. Reusing connections saves the milliseconds.
- Inference: one RTX 3090 running vLLM with FP8 weights and prefix caching
  kept 8 to 16 screenshot agents stepping, about 1.7 steps per second in
  all, against the 700 kiosks the host's memory holds.

## Parking is the kernel lever that pays

One kiosk per run: frozen, reclaimed into zswap with
`memory.zswap.writeback=0`, held 20 s, and thawed. Times count from the
thaw write. The first CDP call opens a new websocket and evaluates `1+1`,
and the first screenshot is grim's JPEG of the output. Pool is the zswap
pool's real growth, from debugfs, and ratio is the memory the pool holds
over that growth.

| Page | zswap | Charge MiB, running to parked | Pool MiB | Ratio | Reclaim ms | First CDP ms | First screenshot ms |
|---|---|---|---|---|---|---|---|
| Workday | zsmalloc, lz4 | 267 to 86 | 71 | 3.1 | 341 | 33 | 45 |
| Greenhouse | zsmalloc, lz4 | 239 to 71 | 57 | 3.6 | 292 | 34 | 45 |
| Wikipedia | zsmalloc, lz4 | 275 to 89 | 75 | 3.2 | 347 | 36 | 49 |
| Workday | zsmalloc, zstd | 279 to 64 | 48 | 4.6 | 927 | 43 | 57 |
| Greenhouse | zsmalloc, zstd | 253 to 54 | 38 | 5.4 | 727 | 32 | 45 |
| Wikipedia | zsmalloc, zstd | 276 to 63 | 49 | 4.9 | 893 | 55 | 70 |
| Workday | zbud, lz4 | 284 to 88, but 131 in RAM | 112 | 2.0 | 349 | 35 | 47 |
| Workday, lean flags | zsmalloc, zstd | 200 to 46 | 36 | 4.6 | 704 | 40 | 54 |
| Greenhouse, lean flags | zsmalloc, zstd | 181 to 33 | 25 | 5.9 | 507 | 28 | 40 |
| Wikipedia, lean flags | zsmalloc, zstd | 204 to 44 | 35 | 5.2 | 652 | 61 | 75 |

The other states, on Workday:

| State | Charge MiB | First CDP ms | First screenshot ms |
|---|---|---|---|
| frozen, nothing reclaimed | 275 | 20 | 29 |
| parked with writeback on, pages on the swap device | 17 | 79 | 157 |
| evicted, then relaunched on its profile | none while stopped | DevTools up after 160 to 240 | page complete after 1.3 s |

- zstd parks a kiosk in about a quarter of its running charge, lz4 in
  about a third. zstd adds 0.4 to 0.6 s of reclaim and up to 21 ms to the
  first screenshot.
- Lean flags (proc, from [desktop.md](desktop.md)) shrink the parked kiosk
  with the running one: 33 to 46 MiB parked, from 181 to 204 running.
- A parked kiosk's charge is its share of the pool plus about 16 MiB of
  other kernel memory: page tables, slabs, and stacks.
- After a scroll and two screenshots, a resumed kiosk had faulted back 70
  to 115 MiB, and 108 to 137 MiB of its pages were still compressed. A
  short task does not pay for the whole kiosk.

Use zsmalloc with zstd. A kiosk parks while its agent waits, so the longer
reclaim adds no latency, and a third less memory per parked kiosk raises
the count that fits in 125 GiB from about 1,500 to about 2,000 (arithmetic,
kiosks alone). Lean flags raise it to about 3,000, though each parked kiosk
still holds 150 to 180 MiB of swap slots. Parking saves about 215 MiB per
idle kiosk, ten times what KSM saves.

[TMO][tmo] runs the same mechanisms at Meta: proactive `memory.reclaim`
driven by PSI, into a compressed tier with zstd, saving 20 to 32 percent of
memory per server. Panoptes' case is simpler, because an idle kiosk is known
to be idle: the residency manager parks it on a timer, not on a pressure
estimate.

## Use zswap, not zram

zswap charges compressed pages to the cgroup that owns them and honors
`memory.zswap.max` and `memory.zswap.writeback` per cgroup
([cgroup v2][cgroup-v2]). zram is a block device, so its pages belong to no
cgroup, and a parked kiosk's memory would vanish from its unit's accounting
and PSI. Chris Down [relays][down-zswap] a Cloudflare report of 20 to
30 minute brownouts on zram machines, with the OOM killer never triggering.

zbud stores at most two compressed pages per page, so it caps the ratio at
2:1; zsmalloc packs pages densely. zbud was the test server's default, and
the cgroup does not see its waste: `memory.stat` charges compressed bytes,
69 MiB for a parked Workday kiosk, while zbud's pool grew 112 MiB.
zsmalloc's pool grew 2 to 3 MiB more than its charge. 6.18's zswap
[supports only zsmalloc][zswap-618].

## KSM saves 8 percent across agents of one build

64 agents on the same three pages, every kiosk with `MemoryKSM=yes`, and
ksmd scanning 5,000 pages per wake, 50 times the default, for 240 s:

```text
pages_shared       37,510     146 MiB of merged pages
pages_sharing      410,926    1,605 MiB of duplicate pages mapped onto them
general_profit     1,312 MiB  20 MiB per agent, 8% of the charge
ksmd CPU           108 s      for the 240 s scan and the unmerge after it
```

`general_profit` is KSM's own count of memory saved, net of its metadata.
Parking saves about 215 MiB per idle kiosk against KSM's 20 and costs no
scanning, so KSM is a second step for a host that is still short of memory
with its idle kiosks parked.

KSM stays inside one credential domain whatever it saves: a write to a
merged page takes measurably longer, which leaks memory contents between
processes, and [Flip Feng Shui][flip-feng-shui] combined merging with
Rowhammer to corrupt another VM's memory.

## CPU: weights on EEVDF

[scheduler.md](scheduler.md#agents-get-cpu-by-weight-never-by-cap) has the
contention measurement. The kernel facts behind it:

- EEVDF has been the fair scheduler since 6.6; [6.12 completed it][eevdf]
  with lag handling and per-task slices.
- sched_ext policies are BPF programs. Meta's
  [scx_layered case study][scx-layered] reports about 3.5 percent more
  throughput across about 1,000 hosts. A gain of that size needs a
  CPU-saturated host. In E1c an idle, unfrozen agent used 0.25 to 0.83
  percent of a core with default flags, and 0.05 percent with lean flags.
  Idle agents would therefore not saturate the test server's 32 threads
  until about 4,000 of them ran, or about 60,000 lean ones.
- The test server's kernel is built without `CONFIG_SCHED_CLASS_EXT`, so
  sched_ext could not be tried here.

## The network is not on the critical path

From the test server, medians of 10 new connections:

| Model API host | TCP connected | TLS done | First byte |
|---|---|---|---|
| api.anthropic.com | 11.7 ms | 21.6 ms | 31.9 ms |
| api.openai.com | 13.2 ms | 24.1 ms | 35.0 ms |
| generativelanguage.googleapis.com | 22.0 ms | 50.7 ms | 78.3 ms |

- An HTTP request to Chromium's DevTools over loopback took 118 us. A CDP
  call over an open websocket took 2 ms ([desktop.md](desktop.md)), nearly
  all of it inside Chromium.
- One page load moved 0.67 to 1.73 MB in 20 to 40 requests.

A model step takes seconds, so a new TLS connection costs a few percent of
it and a reused one costs nothing. The work that pays is in user space:
reusing connections to model APIs.

Kernel network features, against that budget:

| Feature | Best published gain | Verdict |
|---|---|---|
| sockmap | cross-netns TCP round trip from 5.75 to 4.69 us ([Cloudflare][sockmap]) | 1 us against seconds |
| busy polling | memcached p99 from 416 to 141 us at 200K QPS, at 100% CPU ([v9 series][busypoll]) | spends the CPU that parking saves |
| kTLS | zero-copy `sendfile` and NIC offload ([tls.rst][ktls]) | agents send small requests and stream replies |
| io_uring | no published gain for this traffic; 60% of 2023 kernelCTF exploits used it ([Google][kernelctf]) | attack surface on a host that renders untrusted pages |

## The GPU sets local density

UI-TARS-1.5-7B on vLLM 0.32.0 and the RTX 3090, closed-loop simulated
agents: each step sends the history plus one new 1280x720 screenshot and
decodes 64 tokens. A history starts with a 1,000-token system prompt and
grows by one screenshot per step, to six; prompts averaged 5,570 tokens.

| Weights, prefix cache | KV cache, tokens | Agents | Steps/s, all agents | Step p50 / p95, s | Prefix hit rate | Peak KV use |
|---|---|---|---|---|---|---|
| bf16, on | 58,144 | 1 | 0.58 | 1.7 / 1.8 | 0.78 | 15% |
| | | 4 | 1.30 | 3.0 / 3.2 | 0.78 | 55% |
| | | 8 | 1.49 | 4.8 / 6.9 | 0.76 | 95% |
| | | 16 | 0.69 | 22.1 / 41.7 | 0.31 | 99%, 1 preemption |
| bf16, off | 58,144 | 1 | 0.37 | 2.5 / 3.5 | | 15% |
| | | 4 | 0.51 | 6.6 / 13.0 | | 61% |
| | | 8 | 0.51 | 15.8 / 23.5 | | 91% |
| | | 16 | 0.50 | 28.5 / 46.5 | | 100%, 1 preemption |
| FP8, on | 176,368 | 1 | 0.82 | 1.2 / 1.2 | 0.78 | 5% |
| | | 4 | 1.49 | 2.6 / 2.8 | 0.78 | 18% |
| | | 8 | 1.70 | 4.6 / 5.0 | 0.78 | 35% |
| | | 16 | 1.80 | 8.9 / 10.4 | 0.78 | 70% |

- The KV cache sets how many agents fit, and past it throughput falls. The
  bf16 weights took 15.7 GiB and left room for about ten histories. At 16
  agents the cache overflowed: the hit rate fell from 0.78 to 0.31, and
  throughput halved.
- FP8 weights took 9.0 GiB, which tripled the cache, and decoded faster:
  one agent's step fell from 1.7 to 1.2 s. At 16 agents the cache was 70
  percent full and throughput had flattened at 1.8 steps/s, so more agents
  add latency, not steps.
- Prefix caching tripled throughput at 8 agents. Without it, prefill
  capped the engine at 0.5 steps/s.
- One 3090 therefore keeps 8 to 16 agents stepping, at a median step of
  4.6 to 8.9 s. The host's memory holds about 700 running kiosks, so the
  GPU, not memory, limits how many agents a host keeps busy; the rest are
  parked or call an API.
- Histories here stopped at six screenshots. A longer history holds more
  cache, so fewer agents fit.

A text model on the same card, gpt-oss-20b with its MXFP4 weights (13.8
GiB loaded), gave the same ceiling. Each step sends a 12 KB slice of a
page's accessibility tree and decodes 64 tokens. A slice is about 2,600
tokens, derived from the mean prompt, so prompts averaged 10,550 tokens
over six steps, nearly twice UI-TARS's:

| Agents | Steps/s, all agents | Step p50 / p95, s | Prefix hit rate | Peak KV use |
|---|---|---|---|---|
| 1 | 1.09 | 0.85 / 1.05 | 0.72 | 8% |
| 4 | 1.57 | 2.4 / 3.1 | 0.74 | 28% |
| 8 | 1.75 | 4.5 / 5.7 | 0.74 | 54% |
| 16 | 1.45 | 9.2 / 19.0 | 0.65 | 94% |
| 32 | 0.70 | 27.9 / 86.6 | 0.19 | 99% |

- Text observations were not cheaper. A 12 KB tree slice costs about two
  screenshots' tokens, and throughput peaked at 1.75 steps/s, as with
  UI-TARS in FP8.
- The cache again set the agent count. Its 225,423 tokens hold about 21
  histories of 10,550 tokens. At 32 agents the hit rate fell from 0.74 to
  0.19 and throughput halved, the same failure as bf16 UI-TARS at 16.

vLLM's sleep mode, level 1, parks the engine itself. Sleep took 6.8 s and
cut GPU memory from 20,868 to 998 MiB. Wake took 1.8 s, and the first step
after it took 1.9 s against 1.7 s warm, because level 1 drops the KV cache.
While the engine slept, MemAvailable was 30 GiB lower, about twice the
15.7 GiB of weights that level 1 moves to host memory. Sleep therefore
swaps models on one GPU in about 2 s, such as a grounding model and a
planner; it saves no memory.

Rules:

1. Keep histories append-only. A harness that rewrites or truncates its
   prompt loses the prefix cache; LMCache measured hit ratio falling from 85
   to 45 percent when context was truncated ([LMCache][lmcache]).
2. Salt the prefix cache per credential domain. A cache hit is faster, so
   shared caches leak whether another tenant sent the same prefix
   ([CVE-2025-46570][vllm-salt]); vLLM's `cache_salt` partitions it.
3. Drop old screenshots in batches, not one per step. A drop changes the
   prompt from the dropped image onward, so everything after it is
   prefilled again; batching pays that once per batch. Anthropic's
   [computer-use demo][cu-demo] removes images in chunks for this reason,
   and keeps every image when the provider's prompt cache is on, because
   cached reads cost a tenth of the input price. A local engine is bound
   by KV memory instead, so it drops in batches.
4. Quantize the weights before adding agents. On a 24 GB card the weights
   and the KV cache share memory, and the cache sets the agent count; FP8
   tripled it here.
5. Admit agents by KV budget: the cache's tokens over one history's
   tokens. Past that budget the engine evicts prefixes and preempts
   requests, so it serves fewer steps, not the same steps more slowly.

## 6.12's gaps and Panoptes' workarounds

| Gap on 6.12 | Fixed in | Panoptes' workaround |
|---|---|---|
| `memory.reclaim` rejects `swappiness=max`, the anonymous-only mode | 6.18 ([docs][cgroup-v2-618]) | warm the browser build from a cgroup that is never parked, and accept losing a kiosk's own page cache, 8 MiB on Workday |
| a reclaim target above what a kernel-memory-only cgroup can reclaim spins for minutes | unverified | a timeout on every reclaim write |
| `cpu.max` throttling stalls lock waiters | [task-based throttling][task-throttle], queued for 6.18 | no `cpu.max` on agents |
| zswap's default allocator is a build-time choice, and zbud was the default here | zsmalloc only by 6.18 | `zswap.zpool=zsmalloc` on the kernel command line |

## Sources

Checked on 2026-10-26.

[cip-slts]: https://lwn.net/Articles/1049604/
[tmo]: https://www.pdl.cmu.edu/ftp/NVM/tmo_asplos22.pdf
[cgroup-v2]: https://docs.kernel.org/admin-guide/cgroup-v2.html
[cgroup-v2-618]: https://www.kernel.org/doc/html/v6.18/admin-guide/cgroup-v2.html
[down-zswap]: https://chrisdown.name/2026/03/24/zswap-vs-zram-when-to-use-what.html
[zswap-618]: https://www.kernel.org/doc/html/v6.18/admin-guide/mm/zswap.html
[flip-feng-shui]: https://www.usenix.org/conference/usenixsecurity16/technical-sessions/presentation/razavi
[eevdf]: https://lwn.net/Articles/969062/
[scx-layered]: https://github.com/sched-ext/scx/blob/case-studies/case-studies/scx_layered.md
[sockmap]: https://www.usenix.org/system/files/srecon23emea-slides_sitnicki.pdf
[busypoll]: https://lkml.iu.edu/hypermail/linux/kernel/2411.1/01569.html
[ktls]: https://docs.kernel.org/networking/tls.html
[kernelctf]: https://security.googleblog.com/2023/06/learnings-from-kctf-vrps-42-linux.html
[lmcache]: https://arxiv.org/html/2510.09665v2
[vllm-salt]: https://github.com/vllm-project/vllm/security/advisories/GHSA-4qjh-9fv9-r85r
[cu-demo]: https://github.com/anthropics/claude-quickstarts/blob/main/computer-use-demo/computer_use_demo/loop.py
[task-throttle]: https://lkml.iu.edu/hypermail/linux/kernel/2509.0/04201.html
[kernel-releases]: https://www.kernel.org/category/releases.html
