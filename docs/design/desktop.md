# Desktop: one Cage per agent, a trimmed Chromium, 1280x720

Status: proposed, 2026-10-26. This doc covers the userspace each agent
sees: the compositor, the browser, the screen size, and how agents drive
them. Numbers come from the test server and its methods in
[measurements.md](measurements.md); provider limits come from the docs
linked under [Sources](#sources).

## Recommendation

- One Cage kiosk per agent, on wlroots' headless backend with the pixman
  renderer: one app, 1280x720, output scale 1. It costs about 10 MiB more
  than headless Chromium and gives every agent a real compositor.
- Chromium in `--app` mode, with background services off, the GPU and
  network services in the browser process, and a memory-pressure signal
  once the page settles. On the three test pages that cuts one agent's
  charge by 32 to 43 percent, to 143 to 189 MiB.
- panoptes-desk, one long-lived Wayland and CDP client per kiosk, serves the
  providers' own computer and browser toolsets and MCP from one coordinate
  space.
- Each kiosk frozen between its agent's steps. Page JavaScript ignores the
  refresh rate, and freezing stops all of it: CNN fell from 3 cores per
  agent to a fifth of one, and the next capture took 9 to 16 ms longer.
  Outputs run at 60 Hz while thawed.

## A compositor per agent costs 10 MiB; the browser costs the rest

One agent at a time, three runs each, 1280x720. Median memory.current /
PSS in MiB; [measurements.md](measurements.md) explains both metrics.

| Stack | about:blank | Greenhouse | Workday | Wikipedia | Processes |
|---|---|---|---|---|---|
| Cage + Chromium | 184 / 431 | 254 / 510 | 280 / 534 | 276 / 526 | 14 |
| Cage + Chromium + wayvnc | 194 / 451 | 265 / 533 | 295 / 559 | 286 / 546 | 16 |
| Xvfb + XFCE + Chromium + x11vnc | 280 / 550 | 347 / 627 | 362 / 638 | 374 / 646 | 32 |
| headless Chromium | 180 / 420 | 250 / 498 | 280 / 529 | 271 / 514 | 12 |

Cage itself holds 9.5 MiB PSS. Against headless Chromium, the kiosk costs
0 to 10 MiB of charge, so "every agent gets a window manager" is nearly
free. A full desktop is not: XFCE adds about 95 MiB and 18 processes per
agent (xfdesktop 40, Xvfb 38, the session 14, xfwm4 13, the panel 9 MiB
PSS). wayvnc adds about 11 MiB, so start it only while a person watches.

Where Chromium's memory goes, PSS on the Greenhouse page:

```text
chromium processes, default flags              PSS MiB
renderers (page, tab-strip WebUI, spare)       193
browser                                        172
network service                                43
zygotes (3)                                    35
GPU process (software compositing)             26
storage service                                9
sandbox broker                                 6
```

## Flags and one signal cut Chromium by 32 to 43 percent

Each flag set adds to the previous one. Two runs each; median
memory.current in MiB, settled / purged. Settled is 15 to 20 s after the
load. Purged is the same agent 10 s after CDP's
`Memory.forciblyPurgeJavaScriptMemory` and a simulated critical
memory-pressure signal.

| Set | Greenhouse | Workday | Wikipedia | Processes |
|---|---|---|---|---|
| base | 251 / 214 | 278 / 233 | 277 / 261 | 14 |
| app | 234 / 177 | 244 / 198 | 239 / 222 | 13 |
| quiet | 213 / 172 | 219 / 189 | 233 / 218 | 13 |
| proc | 196 / 143 | 213 / 165 | 205 / 189 | 9 |
| lowend | 192 / 142 | 204 / 162 | 203 / 189 | 9 |
| lite | 171 / 140 | 186 / 158 | 204 / 187 | 9 |
| noiso, first run only | 182 / 142 | 202 / 161 | 202 / 188 | 9 |

app, quiet, and proc do the work, together with the purge. After the
purge, lowend, lite, and noiso each move the charge by 4 MiB or less. noiso
saves nothing because a one-tab agent has no second site to fold into its
renderer.

The purged column repeats within 6 MiB between runs. The settled column
does not, because a snapshot can land mid-load or before a garbage
collection; noiso's second run took its first snapshot mid-load, so the
table shows its first run. How fast memory grows back after a purge is
unmeasured, so panoptes-desk sends the purge after every page settles and
before it parks a kiosk.

What each set trades:

| Set | Flags | Trade |
|---|---|---|
| app | `--app=URL` | no tab strip or omnibox, which an agent does not use; removes the tab-strip WebUI renderer |
| quiet | `--disable-background-networking`, `--disable-component-update`, `--disable-sync`, `--disable-extensions`, `--disable-default-apps`, `--disable-breakpad`, `--disable-domain-reliability`, `--disable-client-side-phishing-detection`, `--metrics-recording-only`, `--no-pings`, `--mute-audio`, and Translate, OptimizationHints, MediaRouter, and LensOverlay off | no extensions and no Safe Browsing model; workloads that need an extension, such as Simplify for jobpilot, keep `--disable-extensions` off |
| proc | `--in-process-gpu`, `NetworkServiceInProcess`, `SpareRendererForSitePerProcess` off | a GPU or network-stack bug now runs in the browser process; the agent's unit sandbox stays the boundary |
| lowend | `--enable-low-end-device-mode` | smaller caches; several of its features apply only on Android. Skip: 3 MiB or less after the purge |
| lite | `--js-flags=--lite-mode` | no optimizing JIT; [V8 reports][v8-lite] a 22% smaller heap and slower JavaScript. Skip: 4 MiB or less after the purge |
| noiso | `--disable-site-isolation-trials`, `--renderer-process-limit=1` | cross-site pages share a renderer, so one malicious page can read another site's data in a logged-in agent; [field cost of isolation][site-isolation]: 9 to 13% memory. Skip: it saved nothing here, and it is never safe for agents with credentials |

## At density, a lean agent costs about 175 MiB

64 agents at once, pages rotating among the three sites, sampled 90 s
after every page loaded. Per agent:

| Flags | PSS MiB | MemAvailable drop MiB | Charge MiB | Threads | Idle CPU, % of a core |
|---|---|---|---|---|---|
| default, 3 runs | 233 to 240 | 244 to 253 | 252 to 270 | 167 to 170 | 0.25 to 0.62 |
| lowend, 2 runs | 171 to 173 | 176 to 180 | 120 to 159 | 102 | 0.04 to 0.05 |
| lite, 2 runs | 171 | 176 to 179 | 176 | 102 | 0.04 to 0.06 |

- Cost per agent stays flat with count. At default flags the charge was
  266 MiB per agent at 8 agents, 256 at 32, and 252 to 270 at 64. PSS per
  agent fell from 510 for one agent to 261, 233, and about 237, as more
  agents split the browser's file pages.
- The lean sets take about 70 MiB per agent off the host and cut idle CPU
  tenfold. These runs did not purge, so the purge above saves more.
- At 177 MiB per agent, 125 GiB holds about 700 running agents before any
  parking (arithmetic), and before counting each agent's loop
  ([scheduler.md](scheduler.md#the-agent-loop-costs-half-a-lean-kiosk)).
- The charge varies between runs with page-cache history, as
  [measurements.md](measurements.md) explains; PSS and the MemAvailable
  drop repeat.

## One shared browser saves memory only while agents run

One Chromium can hold many agents as BrowserContexts, each with its own
cookies and storage. Per agent:

| Model | Flags | Agents | Charge MiB | MemAvailable drop MiB |
|---|---|---|---|---|
| a browser per agent, each in its own Cage | default | 64 | 252 to 270 | 244 to 253 |
| a browser per agent, each in its own Cage | lowend | 64 | 120 to 159 | 176 to 180 |
| one headless browser | default | 8 | 120 | 159 |
| one headless browser | default | 32 | 110 | 113 |
| one browser in one Cage | default | 32 | 92 | 102 |
| one headless browser, a cgroup per agent | proc | 8 | 109 | 112 |
| one headless browser, a cgroup per agent | proc | 32 | 105 | 106 |

Each context's renderers are their own processes, so the residency manager
can park one agent inside a running shared browser. In E11, systemd's
[`DelegateSubgroup=`][systemd-rc] started the browser in a leaf cgroup, and
a watcher moved each agent's renderers into a sibling cgroup as they
appeared. Parking a Greenhouse agent:

| Agents | Freed, MiB of MemAvailable | Reclaim and page-out ms | Neighbor's screenshot during the park | After thaw: first CDP / screenshot |
|---|---|---|---|---|
| 8 | 70 of its 112 | 230 + 45 | 21 ms | 24 / 51 ms |
| 32 | 33 of its 106 | 192 + 48 | 20 ms | 37 / 83 ms |

- Parking frees only what the agent's own cgroup was charged. A charge
  stays with the cgroup that first touched a page
  ([cgroup v2][cgroup-v2]), so pages a renderer touched before the watcher
  moved it, and the zygote's copy-on-write pages, stay with the browser.
  41 to 66 percent of each agent's renderer anonymous memory was charged to
  its own cgroup.
- Paging out the renderers' private memory with
  [`process_madvise`][process-madvise] reaches pages charged elsewhere,
  but freed only 4 to 7 MiB more. The kernel skips a page another process
  also maps ([madvise.c][madvise-shared]).
- About 32 MiB per agent stays in the browser process, out of any
  per-agent mechanism's reach.
- With the hang monitor off (`--disable-hang-monitor`), the browser kept an
  agent frozen alone for 180 s; it answered CDP 9 ms after the thaw.
- The watcher found renderers by creating agents one at a time. In
  production, panoptes-desk would map renderers to agents from CDP trace
  events, which carry each frame's process ID.

Sharing also gives up what the rest of Panoptes relies on:

- Isolation. One browser process holds every agent's cookies and tokens;
  BrowserContexts separate storage, not memory. Only agents of one
  credential domain can share.
- Fate. One crash or hang stops every agent.
- Display. The agents share one output, so compositor capture and native
  popups work only for the window on top.

A browser per agent stays the default. A parked agent in a shared browser
keeps 40 to 75 MiB, its share less what parking freed. A parked kiosk with
the same lean flags keeps 33 to 46 MiB ([measured][parking]), so once
agents park, sharing saves nothing. It saves about 70 MiB per running
agent, so it pays only for many short-lived agents that run at once inside
one credential domain.

## 1280x720 at scale 1 is the default screen

Visual tokens per screenshot, computed from each provider's formula:

| Model family, formula | 1024x768 | 1280x720 | 1280x800 | 1440x900 | 1920x1080 |
|---|---|---|---|---|---|
| Claude 4.7 and later: ceil(w/28) x ceil(h/28), at most 4,784 tokens and 2,576 px; oversized tool results are rejected, per the [vision docs][claude-vision] | 1036 | 1196 | 1334 | 1716 | 2691 |
| Qwen2.5-VL and UI-TARS-1.5: 28-px tokens after a 2x2 merge, per [UI-TARS's coordinate notes][ui-tars-coords] | 999 | 1196 | 1334 | 1632 | 2691 |
| Qwen3-VL: 32-px tokens | 768 | 880 | 1000 | 1260 | 2040 |
| OpenAI patch models: ceil(1.2 x ceil(w/32) x ceil(h/32)), per the [images guide][openai-vision] | 922 | 1104 | 1200 | 1566 | 2448 |

At 720p every family lands between 880 and 1,200 tokens, and no family
resamples the image. Anthropic's [computer-use guide][claude-cu] recommends
1024x768 or 1280x720 for desktops, 1280x800 or 1366x768 for web apps, and
nothing above 1920x1080. At scale 1, screenshot pixels, Wayland logical
pixels, and CSS pixels coincide, so CDP and virtual-pointer coordinates need
no conversion.

Resolution is a second-order cost. The first-order ones are how many
screenshots stay in history and the toolset's fixed overhead: about 4,500
input tokens for `computer_toolset_20260801`, per the same guide. With a local
model, each 720p screenshot holds 65 MiB of KV cache on UI-TARS-1.5-7B
(56 KiB per token), against 147 MiB at 1080p.

On the test server, resolution moves memory and capture time far less than
it moves tokens. Means over the three pages; capture times are medians of
10:

| Output | Charge MiB | grim JPEG | grim PNG | CDP JPEG | CDP WebP |
|---|---|---|---|---|---|
| 1024x768 | 267 | 16 ms, 107 KB | 27 ms, 149 KB | 32 ms, 76 KB | 50 ms, 47 KB |
| 1280x720 | 261 | 16 ms, 112 KB | 32 ms, 172 KB | 32 ms, 77 KB | 54 ms, 47 KB |
| 1280x800 | 271 | 16 ms, 126 KB | 35 ms, 187 KB | 32 ms, 90 KB | 58 ms, 55 KB |
| 1440x900 | 285 | 16 ms, 151 KB | 41 ms, 204 KB | 30 ms, 108 KB | 68 ms, 65 KB |
| 1920x1080 | 292 | 16 ms, 200 KB | 59 ms, 241 KB | 48 ms, 140 KB | 103 ms, 82 KB |

- 1080p costs 31 MiB more per agent than 720p, and 2.25 times the tokens.
- grim's JPEG time is one 60 Hz frame at every size, because screencopy
  waits for the next frame. grim captures the whole output, popups
  included; CDP captures only the page viewport.
- A no-op CDP call through the same client took 2 ms, so the CDP times are
  mostly Chromium's.

## A full accessibility tree can cost more than a screenshot

Text observations of the same pages at 1280x720:

| Page | Accessibility nodes | Tree as role and name lines | innerText |
|---|---|---|---|
| Greenhouse | 565 | 22.1 KB, 4,969 tokens | 4.5 KB, 1,010 tokens |
| Workday | 628 | 13.2 KB, 3,090 tokens | 2.3 KB, 696 tokens |
| Wikipedia, Linux kernel | 27,212 | 612 KB, 159,631 tokens | 127 KB, 35,858 tokens |

Tokens are UI-TARS-1.5-7B's, counted by vLLM's tokenizer endpoint; other
providers' tokenizers were not measured.

A screenshot costs the same 1,196 tokens on every page. The full tree grows
with the document: 2.6 to 4.2 screenshots' worth on the job boards, and 133
on a long article, more than a 128K context holds. "Structure first"
therefore needs a bound: panoptes-desk serves the tree cut to the viewport or
to interactive elements, and the full tree only on request.

## The refresh rate paces rendering, not JavaScript

A page keeps working while its agent waits for the model. Eight agents on
one page, lean flags (proc); CPU of the agents' units over 30 s, in percent
of one core per agent:

| Page | 60 Hz | 10 Hz | 2 Hz | Hidden |
|---|---|---|---|---|
| CNN home page | 345 | 157 | 138 | 0.21 to 0.33 |
| a timer page: 200 cells rewritten 20 times a second | 11.3 | 6.8 | 1.8 | 0.05 to 0.07 |
| a CSS spinner, like a loading screen | 3.4 | 0.76 | 0.24 | 0.01 to 0.03 |
| Workday | 0.19 | 0.04 | 0.03 | 0.03 to 0.04 |
| Greenhouse | 0.07 | 0.04 | 0.03 | 0.02 to 0.03 |

- The output rate paces only work that waits for a frame:
  `requestAnimationFrame`, style, layout, paint, and CSS animation. The
  spinner is all animation, and 2 Hz cut it by 93 percent.
- Timers, network callbacks, and React do not wait for frames. React's
  scheduler runs its work as `MessageChannel` tasks in 5 ms slices, and
  its source says it "does not attempt to align with frame boundaries"
  ([Scheduler.js][react-scheduler]). At 2 Hz, the timer page still used
  1.8 percent of a core, and CNN 1.4 cores per agent.
- A hidden page falls under Chromium's background policy: timers wake at
  most once a second ([Chrome's timer notes][chrome-timers]), a page hidden
  for 10 s gets 1 percent of a core for them
  ([page scheduler][chromium-budget]), and nothing renders. Only timer
  queues are throttled; posted messages and network tasks are not
  ([frame scheduler][chromium-queues]). CNN still fell to a thousandth of
  its visible cost.
- Settled pages cost almost nothing at any rate. The rate and visibility
  matter for pages that animate or poll: loading screens, dashboards, and
  news pages full of ads.
- With default flags, the spinner cost 1.2 percent at 10 Hz and 0.6 at
  2 Hz (E8), against 0.76 and 0.24 here. The gap matches the default
  flags' idle cost at density, 0.25 to 0.62 percent (inference).
- A capture through the compositor waits for the next frame: grim took
  16 ms at 60 Hz, 100 ms at 10 Hz, and 500 ms at 2 Hz (E8).

## Between steps, freeze the kiosk

The residency manager freezes a kiosk while its agent waits for the model
([scheduler.md][residency]). E10 and E10b measured that against the
alternatives. Each of 8 agents takes one step every 4 s: capture, scroll
300 px and wait two frames, wait 300 ms, capture. CPU of the agents' units
over 60 s, in percent of one core per agent:

| Page | Visible, 60 Hz | 2 Hz | 2 Hz between steps, 60 during | Hidden between steps | Frozen between steps |
|---|---|---|---|---|---|
| CNN home page | 300 to 307 | 180 | 167 | 22 to 26 | 22 |
| timer page | 11.8 to 11.9 | 3.6 | 3.7 | 2.0 to 2.1 | 2.0 |
| spinner | 3.9 | 1.1 | 1.4 | 1.2 to 1.3 | 1.2 |
| Workday | 0.78 to 1.03 | 0.81 to 0.86 | 0.84 to 0.88 | 1.0 to 1.3 | 0.79 to 1.17 |
| Greenhouse | 0.84 to 1.73 | 0.86 to 0.91 | 0.89 to 0.93 | 1.1 to 2.0 | 0.84 to 1.85 |

- Freezing cost the least on CNN and the timer page, tied with hiding.
  On the spinner, 2 Hz cost 0.08 point of a core less but ran 112 steps
  to freezing's 120; per step they tie. Freezing stops timers, React,
  network callbacks, and rendering until the next step, so a page's
  JavaScript runs only while its agent acts. CNN fell from 3 cores per
  agent to a fifth of one.
- On Workday and Greenhouse every mode cost about 1 percent of a core.
  Run in the same position, freezing matched the visible page or cost up
  to 0.14 point more.
- Freezing adds 9 to 16 ms to a step's first capture, counted from the
  start of the step with the connection: 49 to 60 ms on the timer page, 59
  to 72 on the spinner, and 23 to 39 on Workday and Greenhouse.
- Hiding matches freezing on busy pages and costs more time on settled
  ones. A page shown again re-rasters: on Workday and Greenhouse its first
  capture took 23 to 30 ms longer than the visible page's, against 9 to
  16 for a frozen one. Hidden, Greenhouse used 1.1 to 2.0 percent of a
  core against 0.84 visible, but E10 ran its hidden step modes before the
  visible one, so part of that gap is the order effect below.
- 2 Hz cuts animation, not JavaScript: CNN kept 1.8 cores per agent. A CDP
  capture does not wait for the output's next frame, so first captures
  stayed within 10 ms of 60 Hz. A step that waits on frames slowed: the
  spinner's p95 step took 994 ms against 446. Raising the rate to 60 Hz for
  each step restored the step time.
- Eight visible CNN agents saturated the host and slowed every step: p95
  1.3 to 1.5 s, against 0.48 s frozen.
- A live page keeps working for a minute or more after it loads, which
  inflates whichever mode runs first. Workday and Greenhouse ran in both
  orders, and their ranges span the two positions.

Rules:

1. Freeze each kiosk from the start of its agent's model call until its
   next action.
2. Run a thawed kiosk's output at 60 Hz. A frozen kiosk draws nothing, so
   its rate costs nothing.
3. Keep a kiosk thawed while its agent waits only when its page must keep
   working, such as a download or a feed the agent watches. Run it at 2 Hz,
   or hidden if nothing captures it.
4. Do not pace pages from inside Chromium. Paused virtual time stops the
   page's clock: "The virtual time base may not advance"
   ([Emulation.pdl][emulation-pdl]). CPU throttling emulation busy-waits in
   a signal handler ([thread_cpu_throttler.cc][cpu-throttler]), and
   `HeadlessExperimental.beginFrame` works only in headless mode
   ([HeadlessExperimental.pdl][headless-pdl]). These come from the
   protocol and source, not from measurements.

## panoptes-desk drives each kiosk through Wayland and CDP

```text
agent step
├─ act       virtual pointer motion_absolute(x, y, 1280, 720), one pointer
│            and one keyboard held for the kiosk's life
├─ settle    screencopy copy_with_damage until no damage for a short window
├─ observe   the provider's choice: screenshot, zoom crop, or the CDP
│            accessibility tree with element refs
└─ history   keep the last few screenshots, drop old ones in batches so
             the provider's prompt cache keeps hitting
```

panoptes-desk is one process per kiosk. It is a Wayland client of that
kiosk's Cage and a CDP client of its Chromium, and it serves:

| Interface | Executes |
|---|---|
| [`computer_toolset_20260801`][claude-cu] | screenshot, zoom, clicks, drags, type, key, scroll, through screencopy and virtual input |
| [`browser_toolset_20260801`][claude-bu] | navigate, `read_page` and `find` with element refs mapped to CDP accessibility nodes, page text, form input, tabs |
| [OpenAI `computer`][openai-cu] | the same actions in OpenAI's schema |
| MCP | the same tools for any other client |

Rules:

1. Hold one virtual pointer and one virtual keyboard per kiosk. A device
   created per action loses clicks, as in [wlrctl's bug][cage-305].
2. Take the accessibility tree from CDP, not AT-SPI: AT-SPI on Wayland has
   no global coordinates ([at-spi2-core #158][atspi-158]).
3. Capture through the compositor, not only CDP. Native `<select>` popups
   and context menus are separate surfaces that a page screenshot misses
   (inference, not yet run).

## Cage needs four patches

Cage 0.3.1 binds every protocol for every client on its socket; it has no
global filter. Panoptes' kiosk fork adds:

1. A client filter, or security-context-v1, so only panoptes-desk can bind
   capture, virtual input, and output management.
2. ext-image-copy-capture with an output source.
   [wlroots 0.20][wlroots-020] deprecated wlr-screencopy in its favor.
3. ext-data-control, for clipboard access without focus.
4. Render on demand, for kiosks kept thawed at a low rate: render a frame
   as soon as a capture arrives. A compositor capture otherwise waits a
   frame period, 500 ms at 2 Hz; CDP captures already force a frame.

## Sources

Checked on 2026-10-26.

[claude-vision]: https://platform.claude.com/docs/en/build-with-claude/vision
[claude-cu]: https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool
[claude-bu]: https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool
[openai-vision]: https://developers.openai.com/api/docs/guides/images-vision
[openai-cu]: https://developers.openai.com/api/docs/guides/tools-computer-use
[ui-tars-coords]: https://github.com/bytedance/UI-TARS/blob/main/README_coordinates.md
[v8-lite]: https://v8.dev/blog/v8-lite
[site-isolation]: https://www.usenix.org/system/files/sec19-reis.pdf
[cage-305]: https://github.com/cage-kiosk/cage/issues/305
[atspi-158]: https://gitlab.gnome.org/GNOME/at-spi2-core/-/issues/158
[wlroots-020]: https://gitlab.freedesktop.org/wlroots/wlroots/-/releases/0.20.0
[parking]: kernel-and-inference.md#parking-is-the-kernel-lever-that-pays
[systemd-rc]: https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html
[cgroup-v2]: https://docs.kernel.org/admin-guide/cgroup-v2.html
[process-madvise]: https://man7.org/linux/man-pages/man2/process_madvise.2.html
[madvise-shared]: https://github.com/torvalds/linux/blob/v6.12/mm/madvise.c#L505-L513
[residency]: scheduler.md#the-residency-manager-moves-kiosks-between-four-states
[emulation-pdl]: https://chromium.googlesource.com/chromium/src/+/refs/branch-heads/8078/third_party/blink/public/devtools_protocol/domains/Emulation.pdl#68
[cpu-throttler]: https://chromium.googlesource.com/chromium/src/+/refs/branch-heads/8078/third_party/blink/renderer/platform/scheduler/common/thread_cpu_throttler.cc#129
[headless-pdl]: https://chromium.googlesource.com/chromium/src/+/refs/branch-heads/8078/third_party/blink/public/devtools_protocol/domains/HeadlessExperimental.pdl#7
[react-scheduler]: https://github.com/facebook/react/blob/v19.3.1/packages/scheduler/src/forks/Scheduler.js#L452-L455
[chrome-timers]: https://developer.chrome.com/blog/timer-throttling-in-chrome-88
[chromium-budget]: https://chromium.googlesource.com/chromium/src/+/refs/branch-heads/8078/third_party/blink/renderer/platform/scheduler/main_thread/page_scheduler_impl.cc#43
[chromium-queues]: https://chromium.googlesource.com/chromium/src/+/refs/branch-heads/8078/third_party/blink/renderer/platform/scheduler/main_thread/frame_scheduler_impl.cc#494
