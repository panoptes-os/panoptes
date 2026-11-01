# Why the verification rules are what they are

Status: accepted, 2026-10-31; revised 2026-11-01. The rules are in
[verification.md](verification.md). This doc gives the reason for each
rule, the facts that the rules rest on, the tools left out, and what would
change the rules. Each fact carries the date it was checked.

## The bugs come where threads meet, and at the kernel and other processes

Panoptes has little arithmetic. Its hard parts are the order of events
between threads, and what the kernel, the compositor or another process
does. A test that passes once says little about either. So the rules split
each subsystem by what can prove each part: a pure policy that a test can
enumerate, threads that loom can schedule in every order, and I/O that only
the real system can check.

### A race that only a schedule checker finds

A first version of the lease in verification.md's example counts the
requests that started. A timer releases the resource when that count stays
the same for the grace period. Two requests overlap:

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
window. In the example's layout the policy counts requests in flight, and
one owner thread runs `Begin`, the request and `End`, so a release can come
only between requests. A sender that stops waiting for its reply cannot
leave a request counted. A planted bug that releases on the timer without
telling the lease fails the loom model.

## Policy

| Rule | Why |
|---|---|
| 1, 2. A pure `step`; the caller passes `now` | A function of state, input and time can be enumerated and replayed without threads or a clock |
| 3. No `unsafe`, `Cell`, `RefCell`, `Rc`, closures or trait objects | The state stays plain data that a test can copy and compare. Code that the policy does not own could do I/O or keep state that the test cannot see. This also keeps the policy within what Aeneas translates |
| 5. Integer ids | A small integer state is fast to enumerate. Names, paths and descriptors belong to the I/O |
| 6. `check` after each step in debug builds | Every test, example or exhaustive, then checks every invariant after every step |
| 8. An exhaustive test, under 10 s | It covers every order of inputs up to its length. The bet is the small-scope hypothesis: most protocol bugs show in a small world with few inputs. The time limit keeps the test in every debug run |

### Aeneas is optional

Aeneas proves an invariant for every input sequence, where the exhaustive
test stops at its length. It costs a Lean toolchain, proofs that change
with the code, and Lean models of std that we write and trust. The
exhaustive tests already check the same invariants up to a bound, so
Aeneas pays only where that bound is not enough and the policy is stable
enough for its proofs to last.

On 2026-11-01, Aeneas ab34bd28 and Charon 0.1.304 were run against the
desk's and resd's policies:

- A policy with only enums, structs, `Option` and integer arithmetic
  translated with no changes, and the Lean type-checked.
- A closure passed to `filter` or `any` stopped the translation with an
  internal error, and `slice::Iter::find` crashed it. The same loops
  written with an index translated.
- The Lean library models `Vec`, slices, arrays, `Option`, `Result`,
  ranges and a few iterator adapters. It does not model `BTreeMap`,
  `HashMap` or `VecDeque`. Each item it does not model becomes an axiom.
  resd's policy needed about 12 such models: `BTreeMap` `get`, `get_mut`,
  `insert`, `remove`, `contains_key`, `iter` and `values`; `VecDeque`
  `push_back`, `pop_front`, `retain`, `len` and `is_empty`; and slice
  `sort`.
- The Aeneas README adds the limits on `return`, `break`, `continue` and
  `&mut` generics that verification.md lists.

## Threads

| Rule | Why |
|---|---|
| 1. One owner thread per state | One thread decides and the others ask, so a loom model has few orders that matter, and gdb shows which thread holds the state |
| 2. Named threads | The name shows in gdb, `top -H` and panic messages |
| 3. Import from `panoptes_sync` | loom can schedule only code that uses its types. The facade swaps std for loom with one cfg, so the code that loom checks is the code that ships |
| 4. No decisions from an atomic | A decision from an atomic is a protocol across every memory order. A message to the owner is one step in its queue |
| 5. Short locks | A lock held across I/O or a wait makes every other thread wait for that I/O |
| 6. A limit on every reply wait | A lost reply becomes an error that names what the caller waited for, not a hang. Under loom the timeout can fire at any point, so a model runs the caller's code after it too |
| 7. Owners wait with no limit, or until a deadline | An owner with nothing to time is idle, not stuck |
| 8, 9. Time from `sync::now()`; durations of zero in models | loom has no clock. With `now()` at 0 and every duration at zero, the model tries every order that a real clock allows |
| 10. Scoped fan-outs stay out of models | loom has no `scope` |
| 11. The owner keeps the receiver and the reply senders | When the owner exits, its callers' sends and waits fail at once |
| 12. Small models; bound or split | loom's run time grows fast with threads and steps |
| 13. Plant a bug first | A check that runs no model, or a model that asserts nothing, passes. On 2026-10-31 a loom check that ran `--lib` skipped the desk, a binary crate, and still passed |

The facade's loom build differs from loom's own types in three places,
checked against loom 0.7 on 2026-10-31:

- loom's `Sender` has no `Drop`, so loom's channel never disconnects. An
  owner that ends when its senders go would hang every model of it. The
  facade's own channel, on loom's `Mutex` and `Condvar`, keeps std's rules.
- loom's `recv_timeout` is `unimplemented!`. Under loom, `recv_timer` uses
  `try_recv` and returns the timer at once when nothing is queued, so loom
  tries the timer at every point.
- For the same reason, `recv_reply` under loom uses `try_recv` and fails
  at once when no reply is queued. Until 2026-11-01 it waited with no
  limit, so no model ran a caller's code after a reply timeout. A TLA+
  model showed it in 3 steps: a real reply wait times out while its replier
  lives and nothing is queued. The cost of the fix: an owner that never
  replies shows in a model as a timeout error, not as a loom deadlock.

## The facade's channel is checked by loom models

Every loom model trusts the facade's loom build, and its channel is the one
part that loom does not supply. So the facade has loom models of its own, on
the Rust that every other model runs on. On 2026-11-01, each bug planted in
`src/lib.rs` failed one:

| Planted bug | The model that failed, and how |
|---|---|
| `send` without `notify_one` | `a_send_wakes_a_recv_while_a_sender_lives`: deadlock |
| The last `Sender` drop without `notify_one` | `the_last_sender_closes_the_channel_after_its_messages`: deadlock |
| `recv` checks for Closed before the queue | `the_last_sender_closes_the_channel_after_its_messages`: wrong result |
| `Drop for Receiver` drops the queue under the lock | `a_queued_message_can_hold_a_sender`: deadlock |
| `recv_reply` with no limit | `a_reply_can_come_before_or_after_its_timeout`: loom tried one order |
| `recv` or `try_recv` takes the newest message | `messages_come_out_in_order`: wrong result |
| `try_recv` never reports a closed channel; `recv_timer` or `recv_reply` times out on one | `a_closed_channel_ends_each_wait`: wrong result |

A `recv` with `if` where the code has `loop` fails no model, and cannot. loom
0.7.2's `Condvar` never wakes spuriously (`src/rt/condvar.rs`), and the one
receiver thread is the only one that takes messages. There is one receiver
thread because std's `Receiver` is not `Sync`, so code that shares one does
not build without loom. A `Sender` dropped while its thread panics skips the
count, and a loom model fails at its first panic anyway.

The facade had TLA+ specs of this channel and its waits until 2026-11-01. They
found the `recv_reply` gap above. They went because TLC checked a copy written
by hand, and these models run the Rust. Each bug planted in the specs fails a
model here, except `if` for `loop`, and the queue drop is one the specs could
not express. The cost: TLC tried every sequence of operations within small
bounds, and these models try only the sequences they name. The description of
[PR #9](https://github.com/panoptes-os/panoptes/pull/9) keeps the specs and
TLC's results.

## I/O

| Rule | Why |
|---|---|
| 1. No decisions | The VM check then covers only the mapping between the system and the policy's inputs and effects. The decisions are proven in the layers above |
| 2. A VM check against the real thing | A fake shows how we think the kernel or a peer fails. Only the real one shows how it does |
| 3. Kill during an operation | A recovery path that never ran is not known to work |
| 4. `unsafe` in named modules, with `// SAFETY:` | A reviewer finds every unsafe block where they expect it. Clippy's `undocumented_unsafe_blocks` warns on a block without the comment, and CI fails on warnings |
| 5. Shipped C under ASan and UBSan | Rust's checks do not cover C. The sanitizers find memory errors and undefined behavior that a passing test hides |

## tokio

A thread's stack shows in gdb and in a panic message; a task's does not.
loom checks code written against std, and cannot see inside tokio. The
speed that async would gain Panoptes is not measured. So tokio runs only
where a network library needs it, on one thread, and the edge only
forwards.

The cfg is `panoptes_loom`, not loom's usual `loom`. tokio 1.54.1 gates its
`net` module on `not(loom)` (`src/net/mod.rs:1`). On 2026-10-31, with
`--cfg loom`, hyper-util and tokio-tungstenite failed to compile.

## The checks, and the facts they rest on

verification.md lists which check enforces which rule. These facts were
checked on 2026-11-01.

- The deny-list runs only under `--cfg panoptes_loom`. In a normal build
  panoptes-sync re-exports std's types, and clippy names a type by where it
  is defined, so it flagged `sync::Mutex` as `std::sync::Mutex`. Under the
  loom cfg the facade's types are loom's, and only raw std use was flagged.
- The deny-list lives in `nix/rules/`, which is above no crate, and the
  loom pass names it with `CLIPPY_CONF_DIR`. clippy also reads a
  `clippy.toml` from any directory above a crate: a stray one above a
  scratch workspace changed its results.
- A waiver is `cfg_attr(panoptes_loom, expect(...))`. Plain clippy, and
  rustc under the loom cfg, ignored it. The loom pass failed on a waiver
  with nothing left to waive (clippy 0.1.99).
- A waiver sits on the statement. On a function, it also hid a
  `std::thread::spawn` and a `SystemTime::now` planted in `call_tool`.
- The first deny-list run over the workspace found 53 uses. 39 were in
  panoptes-loop, panoptes-top and panoptes-oracle, which are not split and
  allow the deny-list at their roots. Of the 14 in desk, resd and cgroup, 4
  moved to `sync::now()` with the same meaning, and 10 are waived: 8 time
  replies and logs in fractions of a ms, and 2 in panoptes-cgroup also bound
  a kernel wait. Adding `thread::sleep` and `OnceLock` to the list later
  waived 3 sleeps, in the tick thread and two I/O paths, and the `OnceLock`
  in a Wayland proxy's user data.
- The policy check reads text, not types. A policy that reaches a
  forbidden item through a name defined elsewhere passes it. A policy
  crate with `#![no_std]` and no dependencies would make the compiler
  enforce policy 2; that split is not done.
- cargo-mutants checks only the lines a pull request changes, because a
  run over every line fails today. On 2026-11-02 these mutants survive every
  test and model: in resd's `policy.rs`, 7, at lines 199, 256, 276, 311 and
  371; in `worker.rs`, 4, at lines 60 and 64; in `owner.rs`, 18, in
  `discover`, `found`, `save`, `note`, `snapshot`, `request`, `done` and
  `run`; in `conn.rs`, 14, in `connection`, `answer`, `peer_agent` and
  `status`; in desk's `keymap.rs`, 5, at lines 28, 155, 196 and 370; and in
  `handoff.rs`, 9, at lines 80, 85, 106, 110, 125 and 215. desk's loom models
  caught every mutant of `owner.rs`. The `resd` VM check exercises
  `peer_agent`, but cargo-mutants runs no VM.
- A threads mutant fails the check only if the crate's tests and its loom
  models both miss it, because each catches what the other cannot. In resd
  over every line, the tests alone caught 17, in `conn.rs`'s request parsing
  and connection limits, and the models alone caught 11, in the protocol of
  `owner.rs` and `worker.rs`.
- cargo-mutants skips `cfg(test)` code but not `cfg(all(test,
  panoptes_loom))`, so `.cargo/mutants.toml` skips `loom_models`. It also
  skips hand-written `Debug` impls and an emptied `check`, which no test
  can tell apart.
- On GitHub's runner, the deny-list pass took 1 min 25 s. cargo-mutants
  on a diff that touched desk's `lease.rs` and `owner.rs` took 4 min
  45 s: 13 policy mutants in 2 min after a 42 s build, then 3 threads
  mutants in 3 min after a 74 s release build. That run had no tests pass
  for threads files. With that pass, the diff from main to this check's
  commit took 31 min: 3 min of tests and 11 min of models for desk's threads
  files, and 4 min and 7 min for resd's.
- Each check failed on a planted violation: a raw `std::thread::spawn` in
  resd's owner, a stale waiver, a policy file that imports `std::fs` and
  takes an `impl Fn`, an agents' copy with one rule fewer, and a loom log
  in which resd ran no model.

## Tools left out

| Tool | Why not |
|---|---|
| Kani | Not in nixpkgs at the flake's pin. Exhaustive tests cover the small worlds a policy needs without another toolchain |
| ThreadSanitizer | Safe Rust cannot data-race, and loom checks atomics more thoroughly |
| ASan and Miri on Rust | Both need a nightly toolchain; the flake pins a stable one. Miri also cannot run raw system calls such as clone3 |
| Veil, TLA+ | They check a model written by hand in a second language, which can drift from the code. The facade's case is above |
| shuttle | Random schedules for protocols too large for loom. Split the protocol first |

## What would change the rules

| If | Then |
|---|---|
| loom's channel disconnects when its senders drop, and loom implements `recv_timeout` | The facade can re-export loom's channel and timed wait |
| tokio stops gating `net` on `cfg(loom)` | The cfg can be loom's usual `loom` |
| Aeneas translates closures and models `BTreeMap` and `VecDeque` | Aeneas costs less; reconsider it for stable policies |
| Kani is in nixpkgs at the flake's pin | It could replace hand-written enumeration in the exhaustive tests |
| A protocol stays too large for loom after it is split | shuttle |
| A measurement shows that a threads layer needs async | The tokio rules |
| Each policy moves to a `#![no_std]` crate with no dependencies | The compiler can enforce policy 2, and the policy script shrinks |
| The gaps above get tests | cargo-mutants can check every line, not only changed ones |
