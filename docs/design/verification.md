# Verification

Status: accepted, 2026-10-31; revised 2026-11-01.

Every Panoptes subsystem is split by what can prove each part, not by
feature. It has three layers, and each layer has its own check. The code in
each layer follows the rules below, so that its check can prove it.

The reasons for each rule are in
[verification-rationale.md](verification-rationale.md). Agents use
[docs/agents/verification.md](../agents/verification.md), which has the
same rules with the same numbers.

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

Each layer is its own module. A small subsystem can keep all three in one
file. The threads layer calls the policy and the I/O layer. It gets the I/O
layer as a parameter, so a loom model can pass a fake.

## Example: a lease on a shared resource

A server holds a resource while requests run. It releases the resource
when no request has run for a grace period.

| Layer | Code | Check |
|---|---|---|
| policy | `Lease::step(input, now)` takes `Begin`, `End` or `Tick`. It counts the requests in flight, and returns `Release` only when none runs and the grace period has passed | Example tests; every sequence of up to 7 inputs, with `check` after each step |
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

`Begin`, the request and `End` run on one thread, so a release comes only
between two requests. The loom model sets the grace period to zero, so
loom runs the timer both before and after the second request.

## Policy: a pure function, checked exhaustively

1. Write the policy as a state type with a `step` method. `step` takes one
   input and the time, and returns the effects.
2. Do not do I/O in the policy. Do not read a clock, start a thread, take
   a lock, use an atomic, or write `async`. The caller passes `now`.
3. Do not use `unsafe`, `Cell`, `RefCell` or `Rc` in the policy. Do not
   take a closure or a trait object. The policy calls only code that it
   owns.
4. If the policy has timers, give it a `deadline` method. It returns the
   time of the next `Tick`.
5. Name things with integer ids. Keep names, paths and file descriptors in
   the I/O layer.
6. Write a `check` method that asserts every invariant. Call it at the end
   of `step` in debug builds.
7. Write one example test for each behavior. Name the test for the
   behavior.
8. Write an exhaustive test. It runs every sequence of inputs up to a fixed
   length in a small world, and calls `check` after each step. It can skip
   an input that the threads layer never sends in that state; a comment
   says why. The test states the world and the length, and runs in less
   than 10 s in a debug build.

### Aeneas is optional

Use Aeneas only to prove an invariant over unbounded state, in a policy
that no longer changes often. Aeneas translates the policy to Lean, where
the proof covers every input sequence. A policy for Aeneas:

- uses loops with an index, not closures in `filter`, `map`, `any` or
  `find`;
- uses only integers, `Vec`, slices, arrays, `Option` and `Result` from
  std, or comes with a Lean model of each other std item that it uses;
- has no `return` inside a nested loop, no `break` or `continue` to an
  outer loop, and no generic type instantiated with a `&mut`.

## The panoptes_sync facade swaps std for loom

The threads layer imports its threads, channels, locks, atomics and time
from `panoptes_sync`. One cfg, `--cfg panoptes_loom`, swaps them for
loom's.

| Item | Normal build | With `--cfg panoptes_loom` |
|---|---|---|
| `Arc`, `Mutex`, `Condvar`, `atomic`, `thread` | std | loom |
| `mpsc` | std | The facade's channel on loom's `Mutex` and `Condvar`. Like std's: `recv` fails once every sender is gone, and `send` fails once the receiver is gone |
| `spawn(name, f)` | A named std thread | A loom thread |
| `scope` | `std::thread::scope` | `std::thread::scope`, which loom does not model |
| `now()` | Milliseconds since the first call | Always 0 |
| `recv(&rx)` | Waits for a message; `Wait::Closed` when every sender is gone | The same |
| `recv_timer(&rx, deadline)` | Waits for a message until `deadline`; then `Wait::Timer` | `Wait::Timer` at once if no message is queued |
| `recv_reply(&rx, limit, what)` | Waits for a reply up to `limit`; the error names `what` | The timeout error at once if no reply is queued |

## Threads: one owner per state, checked with loom

1. Give each piece of mutable state one owner thread. Other threads send
   the owner a message. If they need an answer, they wait for its reply.
2. Start every thread with `sync::spawn(name, f)`.
3. Put threads, channels, locks and atomics only in the threads layer.
   Import them from `panoptes_sync`, not from `std`.
4. Do not decide anything from an atomic. Send the owner a message. Use
   atomics only for counters that nothing decides from, such as statistics.
5. Hold a lock only to copy or update its data. Do not hold it across I/O,
   a wait or another lock. Use a lock for data that one thread writes and
   other threads copy.
6. Wait for a reply with `sync::recv_reply(&rx, limit, what)`, and handle
   its error. A late reply can still come after it.
7. An owner waits for its next message with `sync::recv(&rx)`. If its
   policy has a deadline, it waits with `sync::recv_timer(&rx, deadline)`.
8. Read the time only with `sync::now()`.
9. Correctness must not depend on how long anything takes. In a loom model,
   set every duration to zero.
10. Use `sync::scope` when the threads end before the function returns,
    such as one operation run on several resources at once. Keep a scoped
    fan-out out of loom models. Its threads share nothing that they write.
11. Keep the receiver and every pending reply sender in the owner. When the
    owner exits, they drop, and its callers get an error.
12. Write a loom model for each claim that the layer makes, with two or
    three threads and a fake I/O layer. If a model needs a preemption bound
    to finish, set it in the model and write the reason beside it. If the
    model is still too slow, split the protocol.
13. Before you trust a model, plant a bug that breaks its claim. The model
    must fail, for that reason.

## I/O: no decisions, checked in a VM

1. Do I/O and make no decisions. Turn what comes in into policy inputs,
   and do the effects that the policy returns.
2. Check each I/O layer in a NixOS VM check against the real thing: the
   kernel, the compositor, or the process at the other end of the socket.
3. If a subsystem claims to recover from a crash, its VM check kills it
   during an operation and checks the recovery.
4. Keep `unsafe` Rust in modules named for what they wrap, such as a
   system call. Put a `// SAFETY:` comment on every block.
5. Build the C and C++ code that Panoptes ships with
   `-Db_sanitize=address,undefined` in a separate check, and run its
   end-to-end scenarios under it. The check fails on any sanitizer report
   and on an exit code other than 0. Under ASan, a leak sets the exit code.

## tokio stays at the network edges

1. Use tokio only for a network library that needs it, such as an HTTP or
   MCP server or client.
2. Run the edge on one named thread with a current-thread runtime. Add
   worker threads only when a measurement shows the need.
3. An edge task turns a request into the subsystem's types, sends it to
   the owner, and awaits the reply. It keeps no state and makes no
   decisions.
4. Send to the owner on a `sync::mpsc` channel. Reply on a
   `tokio::sync::oneshot`, which the edge task awaits. Neither `send`
   blocks.

## CI checks the rules that a tool can check

| Rules | What fails the check | Check |
|---|---|---|
| Policy 2, 3 | A policy file that names I/O, the clock, a thread, a lock, an atomic, shared or interior-mutable state, `unsafe`, a closure or trait object argument, or `async`, or that imports more than std's collections and plain-data modules | `rules` nix check: `nix/rules/policy.sh` |
| Policy 7, 8; threads 12, 13 | A mutant that cargo-mutants plants on a policy or threads line that the pull request changes, and that no test (policy) or loom model (threads) catches | CI `check-mutants`: `nix/rules/mutants.sh` |
| Threads 2, 3, 8; tokio 2 | std's threads, locks, channels, atomics or clock named outside panoptes-sync, or a multi-threaded tokio runtime | CI `check-rust`: clippy under `--cfg panoptes_loom`, with `nix/rules/clippy.toml` |
| Threads 13 | A crate that uses panoptes-sync and runs no loom model | `loom` nix check: `nix/rules/loom-count.sh` |
| I/O 4 | `unsafe` in a module that does not allow `unsafe_code`, or an unsafe block with no `// SAFETY:` comment | The workspace lints `unsafe_code` and `undocumented_unsafe_blocks` |
| I/O 5 | A sanitizer report, or an exit code other than 0, in one of the Cage fork's scenarios | The `cage-*-asan` nix checks |
| Every rule | The two copies of the rules, here and in `docs/agents/`, numbered differently | `rules` nix check: `nix/rules/rule-copies.sh` |

Review checks the other rules.

## Commands and layout

| Check | Command |
|---|---|
| Policy tests | `cargo test -p CRATE`; every crate: `nix build -L .#checks.x86_64-linux.unit` |
| loom models | `RUSTFLAGS="--cfg panoptes_loom" cargo test --release -p CRATE -- loom_models`; every crate: `nix build -L .#checks.x86_64-linux.loom` |
| VM checks | `nix build -L .#checks.x86_64-linux.NAME`; see the [README](../../README.md#build-and-check) |
| TLA+ for the facade | `bash crates/panoptes-sync/tla/check.sh`; every spec: `nix build -L .#checks.x86_64-linux.tla` |
| Aeneas | `charon cargo --preset=aeneas`, then `aeneas -backend lean -split-files FILE.llbc` |
| Rule scripts | `nix build -L .#checks.x86_64-linux.rules` |
| Deny-list | `CLIPPY_CONF_DIR=$PWD/nix/rules RUSTFLAGS="--cfg panoptes_loom" cargo clippy --workspace --lib --bins -- -D warnings` |
| Mutants | `bash nix/rules/mutants.sh DIFF`, where DIFF is a unified diff of the change |

- Put a crate's loom models in a module named `loom_models`, gated with
  `#[cfg(all(test, panoptes_loom))]`. Call `panoptes_sync::model`, so that
  only panoptes-sync depends on loom.
- Gate every other test that uses `panoptes_sync` with
  `#[cfg(not(panoptes_loom))]`.
- Use `--cfg panoptes_loom`, not loom's usual `--cfg loom`.
- Change the specs in `crates/panoptes-sync/tla` with the code between the
  `tla` markers in that crate's `src/lib.rs`, and put the new hash in
  `tla/source.sha256`.
- List each crate's policy files and threads files in its `Cargo.toml`:

  ```toml
  [package.metadata.panoptes]
  policy = ["src/lease.rs"]
  threads = ["src/owner.rs"]
  ```

- Where code outside panoptes-sync must name std's clock or threads, waive
  the deny-list on that function, with the reason:
  `#[cfg_attr(panoptes_loom, expect(clippy::disallowed_methods, reason = "..."))]`.
  A crate that is not split yet allows the deny-list at its root.
