#!/usr/bin/env bash
# E8: what the headless output's refresh rate costs and buys. N Cage +
# Chromium agents show an animated page (a CSS spinner, like a loading
# screen); for each refresh rate, record the slice's CPU use and one
# agent's screencopy latency (grim). Appends to results/e8.tsv.
# usage: e8.sh N "RATES_HZ"
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh
n=${1:-8} rates=${2:-"60 30 10 2"}
slice=/sys/fs/cgroup/glarb.slice out=$B/results/e8.tsv
page='data:text/html,<style>div{width:80px;height:80px;border:8px solid %23ccc;border-top-color:%23333;border-radius:50%25;animation:s 1s linear infinite}@keyframes s{to{transform:rotate(360deg)}}</style><div></div><p>loading</p>'
cpu_us() { awk '$1 == "usage_usec" {print $2}' "$slice/cpu.stat"; }
on() { XDG_RUNTIME_DIR=$B/run/e8n$1 WAYLAND_DISPLAY=wayland-0 "${@:2}"; }
for i in $(seq "$n"); do
  ./start.sh cage "e8n$i" 1280 720 $((9800 + i)) "$page" ||
    echo "e8: start $i failed" >&2
done
sleep 10
for hz in $rates; do
  for i in $(seq "$n"); do
    on "$i" wlr-randr --output HEADLESS-1 --custom-mode "1280x720@${hz}Hz"
  done
  sleep 5
  u0=$(cpu_us)
  sleep 30
  u1=$(cpu_us)
  # Screencopy waits for the next output frame, so latency tracks 1/Hz.
  on 1 hyperfine -N -w 2 -r 10 --export-json "$B/logs/e8-$hz.json" \
    "grim -t jpeg -q 80 $B/logs/e8-$hz.jpg" > /dev/null 2>&1
  grim_ms=$(jq '.results[0].summary.time_wall_clock.median * 1000 | round' "$B/logs/e8-$hz.json")
  printf 'n=%s hz=%s\tslice_cpu_pct=%s per_agent_cpu_pct=%s\tgrim_jpeg_ms=%s\n' \
    "$n" "$hz" \
    "$(awk -v a="$u0" -v b="$u1" 'BEGIN {printf "%.1f", (b - a) / 30e6 * 100}')" \
    "$(awk -v a="$u0" -v b="$u1" -v n="$n" 'BEGIN {printf "%.2f", (b - a) / 30e6 * 100 / n}')" \
    "$grim_ms" | tee -a "$out"
done
for i in $(seq "$n"); do echo "glarb-e8n$i"; done | xargs sudo systemctl stop
sleep 3
