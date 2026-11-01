# Panoptes vision

Status: accepted, 2026-10-31.

Panoptes is a high-performance harness and orchestrator for agents that
use graphical programs. The harness, `panoptes-desk`, gives each agent a
Linux desktop that runs any GUI program, as decided in
[design/decisions.md](design/decisions.md). The orchestrator decides which
agents hold a host's memory and CPU (`panoptes-resd`) and drives their
steps (`panoptes-loop`). We judge both by what a step costs the host
outside the model.

## The bet: models get fast, so the harness becomes the cost

We design as if a model call were free and instant. Everything else is the
cost of a step: running the action, waiting for the screen to settle,
capturing it, starting programs, and waiting on the programs' own
backends.

This is a bet, not a measurement. Today a model call usually takes longer
than the harness work around it. Model steps keep getting faster and
cheaper, though, and once the model is fast, the harness work is what is
left.

The earlier desk already shows the shape. We ran 20 agents on a server
with 16 cores and 32 threads, at 1280x800, against a scripted model that
waited 2 s per step. The loop read each page as Chromium's accessibility
tree, as text; no model saw pixels. All 20 invoices verified.

| Part of a step | Median | 90th percentile |
|---|---|---|
| Model call (the scripted 2 s wait) | 2,002 ms | 2,005 ms |
| Harness: everything else | 660 ms | 1,316 ms |
| Settle: the loop's 300 ms quiet wait, over CDP | 324 ms | 1,056 ms |
| Read the page's accessibility tree | 117 ms | 157 ms |
| Action | 61 ms | 437 ms |

The last three rows are medians of each part, so they do not add up to the
harness row. With a free model, the 660 ms would be the whole step, and
settling would be about half of it.

Settling is long because of a default, not because of the program. The
loop asked the earlier desk to wait until the page had been quiet for
300 ms. The Wayland desk keeps 300 ms as its default quiet window
(`QUIET_MS` in [desk.rs](../crates/panoptes-desk/src/desk.rs)): after each
action it waits until no frame has changed the screen for that long. An
action that uses the default costs at least 300 ms, however fast the
program redraws. The Wayland desk's step costs are not measured yet.

## Throughput is the measure; residency follows from it

By Little's law, the number of agents a host must hold equals throughput
times the time each task takes. Agents beyond that number wait in a queue,
and holding them adds no work.

The 700-agent demo shows this, and it shows a slow model shaping a result.
300 agents and 700 agents entered invoices at the same rate, about 0.6 per
second, and every invoice verified. The loop let at most 25 agents into
the ERPNext part of the task at once, and an agent kept its slot through
its model calls, which the scripted model stretched to 2 s each. With that
cap at 50, the same 700 agents entered 0.86 per second, and 696 of 700
invoices verified. In that run agents spent 82 percent of their time
waiting for a slot: 73 percent for an ERPNext slot and 9 percent for a turn
to act. Without those waits the median invoice took 76 s, so about 65
agents in flight would have matched its rate.

So the limit in these runs was the loop's own cap, held across 2 s model
calls. ERPNext's own limit was not measured.

Every benchmark reports:

- steps per second per host, and tasks per hour, at a stated success rate
- harness wall time and CPU time per step
- the scripted model's delay; benchmarks of the harness use none
- how many agents were resident, next to what bounded the run

## Idle time inside a step costs memory

Because residency is throughput times task time, idle time inside a step
costs memory as well as latency: the host holds more agents for the same
throughput. The 300 ms default quiet window is idle time on every action
that uses it.

A step should end when there is evidence the program is done, not when a
timer runs out. The desk has evidence a timer lacks. Each agent's programs
run in their own cgroup, whose CPU use the desk can read, and the
compositor sees every frame a program draws.

## Parking is for waits that are not the model

Freezing and compressing a desktop still pays, for a different reason.
With a fast model, little time is left between an agent's steps, so
freezing between steps saves little CPU. It also has a cost: measured with
Chromium, a frozen kiosk's next capture took 9 to 16 ms longer, and a
parked one took 40 to 75 ms to its first screenshot
([design/README.md](design/README.md), findings 4 and 12).

Parking pays when an agent waits on something slower than itself:

- a person, for a sign-in, a CAPTCHA, or an approval
- a slow backend, or a queue in front of one, as with ERPNext above
- a rate limit or a quota
- its next task, with its programs already started and signed in

700 parked desktops on one host is a capacity number. It matters for these
waits. It says nothing about throughput.

## What would change this

- If model steps for useful tasks stay seconds long, the model call again
  dominates a step, and residency becomes the main lever again.
- Hosted model APIs keep a floor per step: the network round trip, and the
  time the model takes to read a screenshot. A new TLS connection to a
  model API took 22 to 51 ms ([design/README.md](design/README.md),
  finding 9); reading the screenshot is not measured.
- Harness wall and CPU time per step on the Wayland desk, with a scripted
  model and no delay, are not measured yet.
