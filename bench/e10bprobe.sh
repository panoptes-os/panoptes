#!/usr/bin/env bash
# E10b probe: what freezing and thawing an agent costs with no steps at all.
# One Cage + Chromium agent (proc flags, --app) shows PAGE. The agent's CPU
# is recorded over three 20 s windows: visible, cycles of 3.5 s frozen and
# 0.5 s thawed, and frozen. For the cycles, prints the thread names that
# used the most CPU, summed over threads of the same name.
# usage: e10bprobe.sh PAGE
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./pages.sh && . ./flags.sh
p=$1 id=e10bp port=9940
url[tick]=file://$B/pages/tick.html
url[spin]='data:text/html,<style>div{width:80px;height:80px;border:8px solid %23ccc;border-top-color:%23333;border-radius:50%25;animation:s 1s linear infinite}@keyframes s{to{transform:rotate(360deg)}}</style><div></div><p>loading</p>'
cg=/sys/fs/cgroup/glarb.slice/glarb-$id.service
cleanup() {
  echo 0 | sudo tee "$cg/cgroup.freeze" > /dev/null 2>&1
  sudo systemctl stop "glarb-$id"
  sudo systemctl reset-failed "glarb-$id" 2> /dev/null
}
trap 'exit 1' INT TERM
trap cleanup EXIT
usec() { awk '$1 == "usage_usec" {print $2}' "$cg/cpu.stat"; }
pct() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%.2f", (b - a) / 20e4}'; } # over 20 s
frz() { echo "$1" | sudo tee "$cg/cgroup.freeze" > /dev/null; }
threads() { # FILE: "tid name ticks" for every thread in the unit
  local q
  for q in $(cat "$cg/cgroup.procs"); do cat /proc/"$q"/task/*/stat 2> /dev/null; done |
    awk '{i = index($0, "("); j = match($0, /\) [A-Z] /)
      split(substr($0, j + 2), f, " "); n = substr($0, i + 1, j - i - 1)
      gsub(/ /, "_", n); print $1, n, f[12] + f[13]}' > "$1"
}
lean_flags proc
./start.sh cage "$id" 1280 720 "$port" "app:${url[$p]}" "${F[@]}" || exit 1
./waitload.sh "$port" 120 nav || echo "e10bprobe: load timeout" >&2
sleep 15
u0=$(usec); sleep 20; u1=$(usec)
t=$B/logs/e10bprobe-$p
threads "$t.0"
for _ in 1 2 3 4 5; do frz 1; sleep 3.5; frz 0; sleep 0.5; done
u2=$(usec)
threads "$t.1"
frz 1; sleep 20; frz 0
u3=$(usec)
printf 'p=%s visible_pct=%s cycle_pct=%s frozen_pct=%s\n' "$p" \
  "$(pct "$u0" "$u1")" "$(pct "$u1" "$u2")" "$(pct "$u2" "$u3")"
# Ticks are 10 ms; a thread that started during the cycles counts in full.
awk 'NR == FNR {a[$1] = $3; next} {d[$2] += $3 - a[$1]} END {for (n in d) if (d[n]) print d[n] * 10, n}' \
  "$t.0" "$t.1" | sort -rn | head -n 8 | awk '{printf "  %5d ms %s\n", $1, $2}'
