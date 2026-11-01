# Verification

Status: accepted, 2026-10-31; revised 2026-11-01. These rules say how to
write a subsystem so that its checks can prove it.

This is the copy for agents. People read
[docs/design/verification.md](../design/verification.md), with the reasons
in [verification-rationale.md](../design/verification-rationale.md). Change
a rule in both copies.

Most bugs come where threads meet, and where code drives the kernel or
another process. There is little arithmetic. So the rules split each
subsystem by what can prove each part, not by feature.

## Each subsystem has three layers, each with its own check

```text
subsystem
├─ policy    state and decisions                       example tests
│            no I/O, clock, threads, locks or async    exhaustive tests
├─ threads   owner threads, channels, the few locks    loom
│            no decisions
└─ I/O       sockets, files, the kernel, processes     NixOS VM checks
             no decisions                              C under ASan and UBSan
```

Each layer is its own module; a small subsystem can keep all three in one
file. The threads layer calls the policy and the I/O layer. It gets the I/O
layer as a parameter, so a loom model can pass a fake.

## Example: a race that only a schedule checker finds

A server holds a resource while requests run, and releases it when no
request has run for a grace period. A first version counts the requests
that started, and a timer releases the resource when that count stays the
same for the grace period. Two requests overlap:

```text
request A  begins   count 0→1; acquire the resource
request B  begins   count 1→2; already held
request A  ends     the timer reads count 2, and sleeps for the grace period
request B  runs
timer      wakes    count still 2, so it releases the resource
                    the resource is gone while request B runs
```

The count is of requests that started, not of requests that still run.
ASan and TSan report nothing: there is no memory error and no data race,
only a wrong order of events. A test fails only if the timer wakes in that
window.

The three layers give each part of the protocol its own check:

| Layer | Code | Check |
|---|---|---|
| policy | A `Lease` with `step(input, now)` for `Begin`, `End` and `Tick`. It counts requests in flight, and returns `Release` only when none runs and the grace period has passed | Example tests; every sequence of up to 7 inputs, with `check` after each step |
| threads | One owner thread owns the lease and the resource, and runs one request at a time | A loom model with two requests |
| I/O | The connection to the resource | Tests against a fake; a VM check against the real one |

The owner thread's loop:

```rust
loop {
    let request = match lease.deadline() {
        Some(at) => sync::recv_timer(&requests, at),
        None => sync::recv(&requests),
    };
    match request {
        Ok(r) => {
            step(Input::Begin); // Acquire, unless held
            let out = run(&r);
            step(Input::End); // idle from now
            (r.reply)(out);
        }
        Err(Wait::Timer) => step(Input::Tick), // Release, once idle for grace
        Err(Wait::Closed) => return,
    }
}
```

`Begin`, the request and `End` run on one thread, so a release can come
only between requests. A sender that stops waiting for its reply cannot
leave a request counted. The loom model sends two requests with the grace
period at zero and the clock stopped, so loom tries the timer before the
second request and after it. A planted bug that releases on the timer
without telling the lease makes the model fail.

## Policy: a pure function, checked exhaustively

1. Write the policy as a state type with a `step` method. `step` takes one
   input and the time, and returns the effects.
2. Do not do I/O in the policy. Do not read a clock, start a thread, take
   a lock, use an atomic, or write `async`. The caller passes `now`.
3. Do not use `unsafe`, `Cell`, `RefCell` or `Rc` in the policy. Do not
   take a closure or a trait object: the policy calls only code it owns.
4. If the policy has timers, give it a `deadline` method that says when
   the next `Tick` is due.
5. Name things with integer ids. Keep names, paths and file descriptors in
   the I/O layer. A small integer state is fast to enumerate.
6. Write a `check` method that asserts every invariant. Call it at the end
   of `step` in debug builds.
7. Write one example test for each behavior, named for the behavior.
8. Write an exhaustive test. It runs every sequence of inputs up to a fixed
   length in a small world, and calls `check` after each step. Skip inputs
   that the threads layer never sends in that state, and say why. Write
   the world and the length in the test. Keep the test under 10 s in a
   debug build.

### Aeneas: rarely

Aeneas translates a Rust policy to Lean, where an invariant can be proved
for every input sequence, not only up to the exhaustive test's length. It
costs a toolchain, proofs that change with the code, and models of the
standard library that you write and trust. Use it only for an invariant
over unbounded state, in a policy that has stopped changing.

On 2026-11-01, with Aeneas ab34bd28 and Charon 0.1.304:

- A policy with only enums, structs, `Option` and integer arithmetic
  translated with no changes, and the Lean type-checked.
- A closure passed to `filter`, `map` or `any` stopped the translation with
  an internal error, and `slice::Iter::find` crashed it. The same loops
  written with an index translated.
- The Lean library models `Vec`, slices, arrays, `Option`, `Result`, ranges
  and a few iterator adapters. It does not model `BTreeMap`, `HashMap` or
  `VecDeque`. Each item it does not model becomes an axiom, and you write
  its model by hand.
- The Aeneas README adds: no `return` inside a nested loop, no `break` or
  `continue` to an outer loop, and no generic type instantiated with a
  `&mut`.

## Threads: one owner per state, checked with loom

1. Give each piece of mutable state one owner thread. Other threads send
   the owner a message. If they need an answer, they wait for its reply.
2. Name every thread: start it with `sync::spawn(name, f)`.
3. Put threads, channels, locks and atomics only in the threads layer.
   Import them from `panoptes_sync`, not from `std`. In a normal build
   `panoptes_sync` re-exports `std`. With `--cfg panoptes_loom` it
   re-exports loom, except for channels: loom 0.7's channel never
   disconnects, so an owner that ends when its senders go would hang a
   model. The facade's own loom channel keeps std's rules.
4. Do not decide anything from an atomic. Send the owner a message. Use
   atomics only for counters that nothing decides from, such as statistics.
5. Hold a lock only to copy or update its data. Do not hold it across I/O,
   a wait or another lock. A lock suits data that one thread writes and
   others copy.
6. Give every wait for a reply a limit. `sync::recv_reply(&rx, limit, what)`
   returns an error that names `what` when the limit passes. Under loom it
   returns that error at once if no reply is queued, so loom tries the
   timeout at every point. Handle the error: a late reply can still come
   after it.
7. An owner waits for its next message without a limit, unless its policy
   has a deadline. Then it calls `sync::recv_timer(&rx, deadline)`. Under
   loom, `recv_timer` returns at once if no message is queued, so the timer
   can fire at any point. loom does not implement `recv_timeout`; the
   facade uses `try_recv`.
8. Read time only with `sync::now()`. Under loom the clock stands still.
9. Correctness must not depend on how long anything takes. In a loom model,
   set every duration to zero.
10. Use `sync::scope` when the threads end before the function returns,
    such as one operation run on several resources at once. loom has no
    `scope`, so keep a scoped fan-out out of loom models, and let its
    threads share nothing they write.
11. Keep the receiver and every pending reply sender in the owner, so they
    drop when it exits. Its callers then get an error, not a hang.
12. Write a loom model for each claim the layer makes, with two or three
    threads and a fake I/O layer. If a model needs a preemption bound to
    finish, set it in the model and say why beside it. If it is still too
    slow, split the protocol.
13. A check that runs no model passes too. Before trusting a model, plant
    a bug that breaks its claim, and see the model fail for that reason.

## I/O: no decisions, checked in a VM

1. Do I/O and make no decisions. Turn what comes in into policy inputs,
   and carry out the effects the policy returns.
2. Check each I/O layer in a NixOS VM check against the real thing: the
   kernel, the compositor, or the process at the other end of the socket.
3. When a subsystem claims to recover from a crash, its VM check kills it
   during an operation and checks the recovery.
4. Keep `unsafe` Rust in modules named for what they wrap, such as a
   system call. Put a `// SAFETY:` comment on every block.
5. Build C and C++ code that Panoptes ships with
   `-Db_sanitize=address,undefined` in a separate check, and run its
   end-to-end scenarios under it. The check fails on any sanitizer report,
   and on an exit code other than 0, which under ASan includes a leak.

## tokio stays at the network edges

A thread's stack shows in gdb and in a panic message; a task's does not.
loom checks code written against `std`, and cannot see inside tokio. So
tokio runs only where a network library needs it.

1. Use tokio only for a network library that needs it, such as an HTTP or
   MCP server or client.
2. Run the edge on one named thread with a current-thread runtime. Add
   worker threads only when a measurement shows the need.
3. An edge task turns a request into the subsystem's types, sends it to
   the owner, and awaits the reply. It keeps no state and makes no
   decisions.
4. Send to the owner on a `sync::mpsc` channel, whose `send` does not
   block. Reply on a `tokio::sync::oneshot`, whose `send` does not block
   either and whose receiver the edge task can await.

## Commands

| Check | Command |
|---|---|
| Policy tests | `cargo test -p CRATE`; every crate: `nix build -L .#checks.x86_64-linux.unit` |
| loom models | `RUSTFLAGS="--cfg panoptes_loom" cargo test --release -p CRATE -- loom_models`; every crate: `nix build -L .#checks.x86_64-linux.loom` |
| VM checks | `nix build -L .#checks.x86_64-linux.NAME`; see the [README](../../README.md#build-and-check) |
| TLA+ for the facade | `bash crates/panoptes-sync/tla/check.sh`; every spec: `nix build -L .#checks.x86_64-linux.tla` |
| Aeneas | `charon cargo --preset=aeneas`, then `aeneas -backend lean -split-files FILE.llbc` |

Put a crate's loom models in a module named `loom_models`, gated with
`#[cfg(all(test, panoptes_loom))]`, and call `panoptes_sync::model` so
that loom stays one crate's dependency. Gate every other test that uses
`panoptes_sync` with `#[cfg(not(panoptes_loom))]`.

TLA+ specs in `crates/panoptes-sync/tla` model the facade's code between
the `tla` markers in its `src/lib.rs`, and the check compares a hash of
that text. Change the specs with the code, run the check, and put the new
hash in `tla/source.sha256`.

loom's own docs use `--cfg loom`. Panoptes cannot: tokio drops its `net`
module under that cfg (tokio 1.54.1, `src/net/mod.rs:1`). On 2026-10-31,
with `--cfg loom`, hyper-util and tokio-tungstenite failed to compile.

## Tools left out

| Tool | Why not |
|---|---|
| Kani | Not in nixpkgs at the flake's pin. Exhaustive tests cover the small worlds a policy needs without another toolchain |
| ThreadSanitizer | Safe Rust cannot data-race, and loom checks atomics more thoroughly |
| ASan and Miri on Rust | Both need a nightly toolchain; the flake pins a stable one. Miri cannot run raw system calls such as clone3 either |
| Veil; TLA+ beyond the facade | They check a model written by hand in a second language, which can drift from the code. The facade is the exception; see verification-rationale.md |
| shuttle | Random schedules for protocols too large for loom. Split the protocol first |
