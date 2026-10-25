#!/usr/bin/env bash
# scale.sh NAME N DELAY_MS: panoptes-loop with the oracle over N invoices,
# desks under panoptes-resd (park after PARK_MS, default 10000), at most
# ACTING (default 40) thawed kiosks at once, ERP_CAP (default 25) in the erp
# phase, desks started at START_PER_S (default 10). Records panoptes top
# (TOP=1, default) to top.replay.jsonl with a pane capture every 15 s.
# A sampler writes sys.tsv every 2 s and creates the stop file, which halts
# new launches, if MemAvailable falls under 8 GiB or swap use passes 100 GiB.
# Results: panoptes-run/loop/NAME/. Needs jq.
set -uo pipefail

NAME=${1:?usage: scale.sh NAME N DELAY_MS} N=${2:?} DELAY=${3:?}
PARK_MS=${PARK_MS:-10000} ACTING=${ACTING:-40} ERP_CAP=${ERP_CAP:-25}
START_PER_S=${START_PER_S:-10} TOP=${TOP:-1}
B=/var/tmp/glarb-bench P=$B/panoptes/target/release L=$B/panoptes-run/loop D=$B/demo/data
SOCK=/run/panoptes/resd.sock R=$L/$NAME STOP=$L/$NAME/STOP
inv=$(seq -f 'inv-%04g' 1 "$N" | paste -sd,)
rm -rf "${R:?}" && mkdir -p "$R/oracle" "$R/caps"
bash $B/scripts/loop/clean.sh "$inv" | wc -l | sed 's/^/cleaned /'
ulimit -n 65536

sudo systemctl stop panoptes-resd 2> /dev/null; sudo rm -f /run/panoptes/resd.state
sudo systemd-run --quiet --collect --unit=panoptes-resd -p LimitNOFILE=65536 \
  -p StandardError=file:$R/resd.log \
  $P/panoptes-resd --tick-ms 500 --park-after-ms "$PARK_MS"
for _ in $(seq 50); do sudo test -S $SOCK && break; sleep 0.1; done
jq --arg n "$NAME" --arg f "--resd $SOCK --thumb-ms ${THUMB_MS:-1000}" --arg stop "$STOP" \
  --argjson cap "$ERP_CAP" --argjson act "$ACTING" --argjson rate "$START_PER_S" '.run_name = $n
  | .providers.oracle.max_concurrency = 256 | .phase_caps = {erp: $cap}
  | .acting_cap = $act | .start_per_s = $rate | .stop_file = $stop
  | .desk.start_timeout_s = 120
  | .desk.launch[-1] |= sub("--size 1280x800"; "--size 1280x800 \($f)")' \
  $B/scripts/loop/oracle.json > "$R.json"

$P/panoptes-oracle --truth $D/truth.jsonl --creds $D/creds.tsv --delay-ms "$DELAY" \
  --log-dir "$R/oracle" > "$R.oracle.log" 2>&1 & op=$!

# time load1 mem_used_kib mem_avail_kib swap_used_kib zswap_pool_b acting
# waiting parked starting parked_mean_kib desks inotify_user
(
  n=0
  while :; do
    s=$(sudo $P/panoptesctl resd $SOCK '{"op":"status"}' 2> /dev/null)
    st=$(jq -r '[.host.zswap_pool // 0,
                 ([.agents[] | select(.state == "acting")] | length),
                 ([.agents[] | select(.state == "waiting")] | length),
                 ([.agents[] | select(.state == "parked")] | length),
                 ([.agents[] | select(.state == "starting")] | length),
                 ([.agents[] | select(.state == "parked") | .memory_current // 0] | if length > 0 then add / length / 1024 | floor else 0 end)]
                | @tsv' <<< "$s" 2> /dev/null)
    [ -n "$st" ] || st=$'0\t0\t0\t0\t0\t0'
    if [ $((n % 5)) -eq 0 ]; then
      ino=$(for p in $(sudo find /proc/[0-9]*/fd -lname 'anon_inode:inotify' 2> /dev/null | cut -d/ -f3); do
              stat -c %U "/proc/$p" 2> /dev/null; done | grep -c '^user$')
    fi
    m=$(awk '/^MemTotal/ {t=$2} /^MemAvailable/ {a=$2} /^SwapTotal/ {st=$2} /^SwapFree/ {sf=$2}
             END {printf "%d\t%d\t%d", t - a, a, st - sf}' /proc/meminfo)
    # systemctl list-units fails with ENOBUFS past a few hundred units.
    desks=$(ls /sys/fs/cgroup/panoptes.slice 2> /dev/null | grep -c '^pdesk-loop-')
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$(date +%s)" "$(cut -d' ' -f1 /proc/loadavg)" "$m" "$st" "$desks" "${ino:-0}"
    read -r _ _ avail swap <<< "$(tr '\t' ' ' <<< "x $m")"
    if [ "$avail" -lt $((8 << 20)) ] || [ "$swap" -gt $((100 << 20)) ]; then
      [ -e "$STOP" ] || { touch "$STOP"; echo "STOP $(date +%s) avail=$avail swap=$swap" >&2; }
    fi
    n=$((n + 1))
    sleep 2
  done
) > "$R/sys.tsv" 2> "$R/stop.log" & sp=$!

# Wake probe: every 20 s, a screenshot from up to 3 parked kiosks.
# wake.tsv: agent wall_ms wake_ms capture_ms total_ms ok
(
  while :; do
    sleep 20
    for a in $(sudo $P/panoptesctl resd $SOCK '{"op":"status"}' 2> /dev/null \
        | jq -r '[.agents[] | select(.state == "parked") | .agent
                  | capture("pdesk-loop-(?<i>[0-9]+)").i] | .[0:3][]' 2> /dev/null); do
      s=$R/agent-$a/desk.sock
      [ -S "$s" ] || continue
      t0=$(date +%s%3N)
      r=$($P/panoptesctl desk "$s" screenshot '{"scale":0.5}' 2> /dev/null)
      t1=$(date +%s%3N)
      printf '%s\t%s\t%s\n' "$a" $((t1 - t0)) \
        "$(jq -r '[.timing.wake_ms, .timing.capture_ms, .timing.total_ms, .ok] | @tsv' <<< "$r" 2> /dev/null)"
    done
  done
) > "$R/wake.tsv" 2> /dev/null & wp=$!

tp=""
if [ "$TOP" = 1 ]; then
  tmux kill-session -t ptop 2> /dev/null
  tmux new-session -d -s ptop -x 200 -y 60 \
    "sudo $P/panoptes-top --run $R --resd $SOCK --record $R/top.replay.jsonl"
  ( i=0; while :; do sleep 15; i=$((i + 1))
      tmux capture-pane -t ptop -e -p > "$R/caps/$(printf %03d $i).ansi" 2> /dev/null
      tmux capture-pane -t ptop -p > "$R/caps/$(printf %03d $i).txt" 2> /dev/null; done ) & tp=$!
fi
trap 'kill $op $sp $tp $wp 2> /dev/null; tmux kill-session -t ptop 2> /dev/null; sudo systemctl stop panoptes-resd 2> /dev/null' EXIT
for _ in $(seq 50); do curl -fsS http://127.0.0.1:8099/v1/models > /dev/null 2>&1 && break; sleep 0.1; done

base=$(awk '/^MemTotal/ {t=$2} /^MemAvailable/ {a=$2} END {print t - a}' /proc/meminfo)
$P/panoptes-loop "$R.json" --invoices "$inv" > "$R/summary.out" 2> "$R.loop.log"
echo "loop exit $?"
sleep 3
(cd $B/demo && bash gen/verify.sh data "$R/assigned.txt") | tee "$R/verify.txt"
jq -c '{agents, claimed_done, verified, attached, verified_and_attached, claimed_not_verified,
        failed_checks, reloads, retypes, wall_s, acting_wait_ms: .acting_wait_ms.mean,
        phase_wait_ms: .phase_wait_ms.mean, step_ms: .step_ms, harness_ms: .harness_ms}' \
  "$R/summary.json" | tee "$R/scale.txt"
jq -s -c '{errors: (map(select(.verified != true or .attached != true)
             | ((.error // "none") | .[0:60]) + " | " + ((.note // "") | capture("phase=(?<p>[^;]*)").p // "?"))
             | group_by(.) | map({(.[0]): length}) | add)}' "$R/results.jsonl" | tee -a "$R/scale.txt"
awk -F'\t' -v base="$base" '{ if ($2 > l) l = $2; if ($3 > m) m = $3; if (a == 0 || $4 < a) a = $4;
    if ($5 > s) s = $5; if ($6 > z) z = $6; if ($9 > pk) pk = $9; if ($12 > d) d = $12;
    if ($13 > ino) ino = $13; if ($9 > 0 && $11 > 0) { ps += $11; pn++ } }
  END { printf "peak load1 %.1f; peak mem used %.1f GiB (%.1f before); min MemAvailable %.1f GiB; peak swap %.1f GiB; peak zswap pool %.1f GiB; peak parked %d; mean parked kiosk %.1f MiB; peak desks %d; peak user inotify instances %d\n",
        l, m / 1048576, base / 1048576, a / 1048576, s / 1048576, z / 1073741824, pk, (pn ? ps / pn / 1024 : 0), d, ino }' \
  "$R/sys.tsv" | tee -a "$R/scale.txt"
cat "$R/stop.log"
sort -t$'\t' -k2,2n "$R/wake.tsv" | awk -F'\t' '$6 == "true" { w[++n] = $2; k[n] = $3 }
  END { if (n) printf "wake probe: %d parked kiosks; wake-to-screenshot p50 %d ms, p95 %d ms, max %d ms\n",
        n, w[int((n + 1) / 2)], w[int(n * 0.95 + 0.5)], w[n] }' | tee -a "$R/scale.txt"
