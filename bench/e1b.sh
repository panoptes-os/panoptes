#!/usr/bin/env bash
# E1b: memory of one Cage + Chromium agent under each cumulative lean flag
# set (flags.sh), once after load and again after a forced V8 purge and a
# simulated critical memory pressure signal. Appends to results/e1b.tsv.
# usage: e1b.sh REPS "SETS" "PAGES"
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./pages.sh && . ./flags.sh
reps=${1:-2} sets=${2:-"base app quiet proc lowend lite noiso"}
pages=${3:-"blank gh wd wiki"}
out=$B/results/e1b.tsv
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
k=0
for rep in $(seq "$reps"); do
  for s in $sets; do
    lean_flags "$s"
    for p in $pages; do
      k=$((k + 1)) id=e1b$k port=9300 u=${url[$p]}
      ((APP)) && u=app:$u
      t0=$EPOCHREALTIME
      ./start.sh cage "$id" 1280 720 "$port" "$u" "${F[@]}" || continue
      ./waitload.sh "$port" 60 || echo "e1b: $s $p load timeout" >&2
      tl=$(ms "$t0" "$EPOCHREALTIME")
      sleep 15
      cg=/sys/fs/cgroup/glarb.slice/glarb-$id.service
      m1=$(sudo ./mem.sh "$cg")
      heap=$(./cdp.sh "$port" Runtime.getHeapUsage |
        jq -r '.usedSize / 1048576 | floor')
      sudo ./mem.sh "$cg" breakdown > "$B/logs/e1b-$id-$s-$p.breakdown"
      ./cdp.sh "$port" Memory.forciblyPurgeJavaScriptMemory > /dev/null
      ./cdp.sh "$port" Memory.simulatePressureNotification \
        '{"level":"critical"}' > /dev/null
      sleep 10
      m2=$(sudo ./mem.sh "$cg")
      printf '%s\t%s\t%s\tready_ms=%s heap=%s\t%s\tpurged[%s]\n' \
        "$rep" "$s" "$p" "$tl" "$heap" "$m1" "$m2" | tee -a "$out"
      sudo systemctl stop "glarb-$id"
      sleep 3
    done
  done
done
