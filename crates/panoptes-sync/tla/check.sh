#!/usr/bin/env bash
# Model-check panoptes-sync's loom channel and waits with TLC. The specs
# must pass, and each planted bug must fail for its own reason.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
src=$here/../src/lib.rs

# The specs model the text between the tla markers in lib.rs.
want=$(cat "$here/source.sha256")
got=$(sed -n '/^\/\/ tla: begin/,/^\/\/ tla: end/p' "$src" | sha256sum | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
  echo "FAIL: src/lib.rs changed between the tla markers." >&2
  echo "Update the specs in tla/, then put $got in tla/source.sha256." >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp "$here"/*.tla "$work"/
cd "$work"

# run CONFIG SPEC: TLC's verdict goes to log; the exit code is checked by
# expect, against what the case should find.
run() {
  rm -rf states
  tlc -deadlock -workers auto -metadir states -config "$1" "$2" > log 2>&1 || true
}
expect() {
  if grep -Eq "$2" log; then
    echo "ok: $1"
  else
    echo "FAIL: $1: TLC did not report /$2/" >&2
    cat log >&2
    exit 1
  fi
}
channel() {
  cat > c.cfg <<CFG
CONSTANTS
  Senders = {s1, s2}
  R = r
  None = none
  MaxSends = 2
  MaxClones = 1
  Bug = "$1"
SPECIFICATION Spec
INVARIANTS TypeOK Mutex Consistent
PROPERTIES RefinesStd RecvReturns
CFG
  run c.cfg LoomChannel.tla
}
waits() {
  printf 'CONSTANTS\n  MaxSends = 2\n  ReplyMode = "%s"\nSPECIFICATION Spec\nPROPERTIES RefinesLoom\n' "$1" > w.cfg
  run w.cfg RealWaits.tla
}

pass='Model checking completed. No error has been found'
channel none
expect "the channel is std's mpsc, and loses no wakeup" "$pass"
for bug in no_notify_on_last_drop no_notify_on_send; do
  channel "$bug"
  expect "planted $bug loses a wakeup" 'Temporal properties were violated'
done
for bug in closed_first no_recheck; do
  channel "$bug"
  expect "planted $bug breaks std's contract" 'Action property .* of module Mpsc is violated'
done
waits try
expect "loom explores every real wait" "$pass"
waits block
expect "a recv_reply with no limit misses the timeout" 'Action property .* of module LoomWaits is violated'
