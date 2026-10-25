#!/usr/bin/env bash
# E1c: memory per agent at density. Start N Cage + Chromium agents (pages
# rotate gh, wd, wiki), let them settle, and record the change in the
# slice's charge and in MemAvailable, summed PSS, processes, threads, and
# idle CPU. With KSM=1 each unit also gets MemoryKSM=yes and ksmd scans
# fast for a while; the KSM settings are restored afterwards.
# Appends to results/e1c.tsv. usage: e1c.sh N FLAGSET [KSM]
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./pages.sh && . ./flags.sh
n=$1 set=${2:-base} ksm=${3:-0}
out=$B/results/e1c.tsv slice=/sys/fs/cgroup/glarb.slice K=/sys/kernel/mm/ksm
avail() { awk '/^MemAvailable:/ {print int($2 / 1024)}' /proc/meminfo; }
cur() { echo $(($(cat "$slice/memory.current") >> 20)); }
cpu_us() { awk '$1 == "usage_usec" {print $2}' "$slice/cpu.stat"; }
lean_flags "$set"
rot=(gh wd wiki) props=
((ksm)) && props="-p MemoryKSM=yes"
c0=$(cur) a0=$(avail)
for i in $(seq "$n"); do
  p=${rot[$(((i - 1) % 3))]}
  u=${url[$p]}
  ((APP)) && u=app:$u
  EXTRA_PROPS=$props ./start.sh cage "e1c$i" 1280 720 $((9600 + i)) "$u" \
    "${F[@]}" || echo "e1c: start $i failed" >&2
done
for i in $(seq "$n"); do
  ./waitload.sh $((9600 + i)) 300 nav || echo "e1c: agent $i load timeout" >&2
done
sleep 30
u0=$(cpu_us)
sleep 60
u1=$(cpu_us) c1=$(cur) a1=$(avail)
pss=0 procs=0 thr=0
for i in $(seq "$n"); do
  cg=$slice/glarb-e1c$i.service
  m=$(sudo ./mem.sh "$cg")
  v=${m##*pss=}
  pss=$((pss + ${v%% *}))
  for p in $(sudo find "$cg" -name cgroup.procs -exec cat {} +); do
    t=$(awk '/^Threads:/ {print $2}' "/proc/$p/status" 2> /dev/null)
    thr=$((thr + ${t:-0})) procs=$((procs + 1))
  done
done
row=$(printf 'n=%s set=%s ksm=%s\td_current=%s d_avail=%s pss_sum=%s per_agent_current=%s per_agent_avail=%s per_agent_pss=%s procs=%s threads=%s idle_cpu_pct_per_agent=%s' \
  "$n" "$set" "$ksm" $((c1 - c0)) $((a0 - a1)) "$pss" $(((c1 - c0) / n)) \
  $(((a0 - a1) / n)) $((pss / n)) "$procs" "$thr" \
  "$(awk -v a="$u0" -v b="$u1" -v n="$n" 'BEGIN {printf "%.2f", (b - a) / 60e6 * 100 / n}')")
if ((ksm)); then
  old_run=$(cat $K/run) old_scan=$(cat $K/pages_to_scan)
  echo 5000 | sudo tee $K/pages_to_scan > /dev/null
  echo 1 | sudo tee $K/run > /dev/null
  sleep 240
  c2=$(cur) a2=$(avail)
  row+=$(printf '\tksm: pages_shared=%s pages_sharing=%s general_profit_mib=%s d_current=%s d_avail=%s per_agent_avail=%s' \
    "$(cat $K/pages_shared)" "$(cat $K/pages_sharing)" \
    $(($(cat $K/general_profit) >> 20)) $((c2 - c0)) $((a0 - a2)) \
    $(((a0 - a2) / n)))
  echo 2 | sudo tee $K/run > /dev/null
  for _ in $(seq 600); do
    [ "$(cat $K/pages_shared)" = 0 ] && break
    sleep 1
  done
  echo "$old_scan" | sudo tee $K/pages_to_scan > /dev/null
  echo "$old_run" | sudo tee $K/run > /dev/null
fi
printf '%s\n' "$row" | tee -a "$out"
for i in $(seq "$n"); do echo "glarb-e1c$i"; done | xargs sudo systemctl stop
sleep 5
