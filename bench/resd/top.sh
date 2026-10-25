#!/usr/bin/env bash
# top.sh (loop.sh + desk thumbnails for panoptes top) NAME N DELAY_MS [resd|none]: panoptes-loop with the oracle over N
# invoices, with desks under panoptes-resd (frozen between steps) or not.
# A sampler records each kiosk's frozen flag and CPU every 200 ms.
# Results: panoptes-run/loop/NAME/{samples.tsv,cpu.txt,...}. Needs jq.
set -uo pipefail

NAME=${1:?usage: loop.sh NAME N DELAY_MS [resd|none]} N=${2:?} DELAY=${3:?} MODE=${4:-resd}
B=/var/tmp/glarb-bench P=$B/panoptes/target/release L=$B/panoptes-run/loop D=$B/demo/data
SOCK=/run/panoptes/resd.sock CG=/sys/fs/cgroup/panoptes.slice
inv=$(seq -f 'inv-%04g' 1 "$N" | paste -sd,)
mkdir -p "$L/$NAME/oracle"
bash $B/scripts/loop/clean.sh "$inv" > /dev/null

flag=""
if [ "$MODE" = resd ]; then
  flag="--resd $SOCK --thumb-ms ${THUMB_MS:-1000}"
  sudo systemctl stop panoptes-resd 2> /dev/null; sudo rm -f /run/panoptes/resd.state
  sudo systemd-run --quiet --collect --unit=panoptes-resd -p StandardError=file:$L/$NAME/resd.log \
    $P/panoptes-resd --tick-ms 500 --park-after-ms ${PARK_MS:-30000}
  for _ in $(seq 50); do sudo test -S $SOCK && break; sleep 0.1; done
fi
jq --arg n "$NAME" --arg f "$flag" '.run_name = $n
  | .providers.oracle.max_concurrency = 64
  | .desk.launch[-1] |= sub("--size 1280x800"; "--size 1280x800 \($f)")' \
  $B/scripts/loop/oracle.json > "$L/$NAME.json"

$P/panoptes-oracle --truth $D/truth.jsonl --creds $D/creds.tsv --delay-ms "$DELAY" \
  --log-dir "$L/$NAME/oracle" > "$L/$NAME.oracle.log" 2>&1 & op=$!
# Sampler: time_ms unit frozen usage_usec, for every loop kiosk.
(
  while :; do
    t=$(date +%s%3N)
    for k in $CG/pdesk-loop-*.service/kiosk; do
      [ -d "$k" ] || continue
      u=${k#"$CG"/}; u=${u%%/*}
      printf '%s\t%s\t%s\t%s\n' "$t" "$u" "$(awk '/^frozen/ {print $2}' "$k/cgroup.events")" \
        "$(awk '/^usage_usec/ {print $2}' "$k/cpu.stat")"
    done
    sleep 0.2
  done
) > "$L/$NAME/samples.tsv" 2> /dev/null & sp=$!
trap 'kill $op $sp 2> /dev/null; sudo systemctl stop panoptes-resd 2> /dev/null' EXIT
for _ in $(seq 50); do curl -fsS http://127.0.0.1:8099/v1/models > /dev/null 2>&1 && break; sleep 0.1; done

$P/panoptes-loop "$L/$NAME.json" --invoices "$inv" > "$L/$NAME/summary.out" 2> "$L/$NAME.loop.log"
echo "loop exit $?"
kill $sp 2> /dev/null
(cd $B/demo && bash gen/verify.sh data "$L/$NAME/assigned.txt") | tail -3

# Per-kiosk CPU split by whether the kiosk was frozen at the interval's
# start, over the intervals where the agent was running its flow.
sort -k2,2 -k1,1n "$L/$NAME/samples.tsv" | awk -F'\t' '
  $2 == pu { dt = $1 - pt; du = $4 - pv; if (pf == 1) { tf += dt; cf += du } else { tt += dt; ct += du } }
  { pu = $2; pt = $1; pf = $3; pv = $4 }
  END {
    printf "time frozen %.1f%% of %.0f agent-seconds\n", 100 * tf / (tf + tt), (tf + tt) / 1000
    printf "CPU while frozen: %.3f%% of a core; while thawed: %.1f%%; overall: %.1f%% per agent\n",
      tf ? 100 * cf / (tf * 1000) : 0, tt ? 100 * ct / (tt * 1000) : 0, 100 * (cf + ct) / ((tf + tt) * 1000)
  }' | tee "$L/$NAME/cpu.txt"
# Model-wait vs observe cost from the loop's own trace.
jq -s '[.[] | select(.step > 0)] | {steps: length,
  observe_p50: (map(.observe_ms) | sort | .[length / 2 | floor]),
  observe_mean: (map(.observe_ms) | add / length | . * 10 | round / 10),
  desk_mean: (map(.desk_ms) | add / length | . * 10 | round / 10),
  model_mean: (map(.model_ms) | add / length | round)}' "$L/$NAME/trace.jsonl" | jq -c . | tee "$L/$NAME/trace-summary.txt"
if [ "$MODE" = resd ]; then
  cat "$L/$NAME"/agent-*/desk.log | awk '/woke kiosk/ { print $5 }' | sort -n | awk '
    { a[NR] = $1; s += $1 } END { printf "wakes %d: mean %.2f ms, p50 %.2f, p95 %.2f, max %.2f\n",
      NR, s / NR, a[int(NR * 0.5) + 1], a[int(NR * 0.95) + 1], a[NR] }' | tee "$L/$NAME/wakes.txt"
  grep -c -E 'thawing the kiosk itself|resd release' "$L/$NAME"/agent-*/desk.log | awk -F: '{ s += $2 } END { print "desk resd errors:", s }'
fi
