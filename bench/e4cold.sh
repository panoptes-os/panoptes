#!/usr/bin/env bash
# E4 cold path: stop the agent entirely and relaunch it on the same profile.
# Times start -> page complete -> first screenshot. usage: e4cold.sh PAGE
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./pages.sh
p=${1:-wd} id=e4c$RANDOM port=9501
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
./start.sh cage "$id" 1280 720 "$port" "${url[$p]}" && ./waitload.sh "$port" 60
sleep 10
sudo systemctl stop "glarb-$id"
sleep 2
t0=$EPOCHREALTIME
KEEP=1 ./start.sh cage "$id" 1280 720 "$port" "${url[$p]}" || exit 1
t1=$(ms "$t0" "$EPOCHREALTIME")
./waitload.sh "$port" 60 nav
t2=$(ms "$t0" "$EPOCHREALTIME")
XDG_RUNTIME_DIR=$B/run/$id WAYLAND_DISPLAY=wayland-0 grim -t jpeg "$B/results/$id.jpg"
t3=$(ms "$t0" "$EPOCHREALTIME")
printf '%s\tcold-relaunch\tdevtools_ms=%s complete_ms=%s first_grim_ms=%s\n' "$p" "$t1" "$t2" "$t3" |
  tee -a "$B/results/e4.tsv"
sudo systemctl stop "glarb-$id"
