#!/usr/bin/env bash
# hard.sh NAME N DELAY_MS [ERP_CAP]: panoptes-loop with the oracle over N
# invoices, desks under panoptes-resd, at most ERP_CAP agents (default 25)
# in the erp phase. Cancels the invoices' earlier test entries first.
# Samples load average and memory every second (sys.tsv), then prints
# verify.sh, the loop's claimed-vs-verified summary, and the peaks.
# Results: panoptes-run/loop/NAME/. Needs jq.
set -uo pipefail

NAME=${1:?usage: hard.sh NAME N DELAY_MS [ERP_CAP]} N=${2:?} DELAY=${3:?} CAP=${4:-25}
B=/var/tmp/glarb-bench P=$B/panoptes/target/release L=$B/panoptes-run/loop D=$B/demo/data
SOCK=/run/panoptes/resd.sock
inv=$(seq -f 'inv-%04g' 1 "$N" | paste -sd,)
rm -rf "${L:?}/$NAME" && mkdir -p "$L/$NAME/oracle"
bash $B/scripts/loop/clean.sh "$inv" | wc -l | sed 's/^/cleaned /'

sudo systemctl stop panoptes-resd 2> /dev/null; sudo rm -f /run/panoptes/resd.state
sudo systemd-run --quiet --collect --unit=panoptes-resd -p StandardError=file:$L/$NAME/resd.log \
  $P/panoptes-resd --tick-ms 500
for _ in $(seq 50); do sudo test -S $SOCK && break; sleep 0.1; done
jq --arg n "$NAME" --arg f "--resd $SOCK" --argjson cap "$CAP" '.run_name = $n
  | .providers.oracle.max_concurrency = 64 | .phase_caps = {erp: $cap}
  | .desk.launch[-1] |= sub("--size 1280x800"; "--size 1280x800 \($f)")' \
  $B/scripts/loop/oracle.json > "$L/$NAME.json"

$P/panoptes-oracle --truth $D/truth.jsonl --creds $D/creds.tsv --delay-ms "$DELAY" \
  --log-dir "$L/$NAME/oracle" > "$L/$NAME.oracle.log" 2>&1 & op=$!
# time_s load1 mem_used_kib (MemTotal - MemAvailable) swap_used_kib
(
  while :; do
    printf '%s\t%s\t%s\n' "$(date +%s)" "$(cut -d' ' -f1 /proc/loadavg)" \
      "$(awk '/^MemTotal/ {t=$2} /^MemAvailable/ {a=$2} /^SwapTotal/ {st=$2} /^SwapFree/ {sf=$2}
              END {printf "%d\t%d", t - a, st - sf}' /proc/meminfo)"
    sleep 1
  done
) > "$L/$NAME/sys.tsv" & sp=$!
trap 'kill $op $sp 2> /dev/null; sudo systemctl stop panoptes-resd 2> /dev/null' EXIT
for _ in $(seq 50); do curl -fsS http://127.0.0.1:8099/v1/models > /dev/null 2>&1 && break; sleep 0.1; done

base=$(awk '/^MemTotal/ {t=$2} /^MemAvailable/ {a=$2} END {print t - a}' /proc/meminfo)
$P/panoptes-loop "$L/$NAME.json" --invoices "$inv" > "$L/$NAME/summary.out" 2> "$L/$NAME.loop.log"
echo "loop exit $?"
kill $sp 2> /dev/null
(cd $B/demo && bash gen/verify.sh data "$L/$NAME/assigned.txt") | tee "$L/$NAME/verify.txt"
jq -c '{agents, claimed_done, verified, attached, verified_and_attached, claimed_not_verified,
        failed_checks, reloads, retypes, wall_s, phase_wait_ms: .phase_wait_ms.mean}' \
  "$L/$NAME/summary.json" | tee "$L/$NAME/hard.txt"
# Steps per invoice, over the verified ones; endings of the rest.
jq -s -c '(map(select(.verified == true) | .steps) | sort) as $s
  | {steps_verified: {n: ($s | length), p50: $s[($s | length) / 2 | floor], max: $s[-1],
                      mean: (if ($s | length) > 0 then ($s | add / length * 10 | round / 10) else null end)},
     endings: (map(select(.verified != true or .attached != true) | (.note | capture("phase=(?<p>[^;]*)").p // "?") + " " + (.failed // [] | join("+")) + (if .error then " err:" + (.error | .[0:40]) else "" end))
               | group_by(.) | map({(.[0]): length}) | add)}' \
  "$L/$NAME/results.jsonl" | tee -a "$L/$NAME/hard.txt"
awk -F'\t' -v base="$base" '{ if ($2 > l) l = $2; if ($3 > m) m = $3; if ($4 > s) s = $4 }
  END { printf "peak load1 %.1f; peak mem used %.1f GiB (%.1f before the run); peak swap used %.1f GiB\n",
        l, m / 1048576, base / 1048576, s / 1048576 }' "$L/$NAME/sys.tsv" | tee -a "$L/$NAME/hard.txt"
