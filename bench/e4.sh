#!/usr/bin/env bash
# E4: park one agent and resume it. Freeze its cgroup and/or reclaim its
# memory into zswap, hold, then thaw and time the first actions after resume.
# usage: [WB=0|1] e4.sh PAGE HOLD_S MODE
#   MODE: freeze-reclaim | freeze-reclaim-anon | freeze-reclaim-anonfit |
#         reclaim-only | freeze-only
#   -anon reclaims with swappiness=200 so shared file pages stay cached.
#   -anonfit also sizes the target to the cgroup's anon plus shmem, since
#   on 6.12 an oversized target falls back to file pages once anon is gone.
#   pool= and stored= are the global zswap pool's growth over the reclaim,
#   from debugfs: the RAM the pool really uses, and the pages it holds.
#   WB=0 sets memory.zswap.writeback=0: parked pages stay in RAM, compressed.
#   FLAGS=SET starts Chromium with that lean flag set from flags.sh; the
#   default is Chromium's own flags.
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./pages.sh && . ./flags.sh
p=${1:-wd} hold=${2:-30} mode=${3:-freeze-reclaim} wb=${WB:-1}
flags=${FLAGS:-default} F=() APP=0
[ "$flags" = default ] || lean_flags "$flags"
id=e4n$RANDOM port=9500
cg=/sys/fs/cgroup/glarb.slice/glarb-$id.service
out=$B/results/e4.tsv
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
swap_mib() { awk 'NR > 1 {s += $4} END {print int(s / 1024)}' /proc/swaps; }
pool() { sudo awk '{printf "%d", $1 / 1048576}' /sys/kernel/debug/zswap/pool_total_size; }
stored() { sudo awk '{printf "%d", $1 * 4 / 1024}' /sys/kernel/debug/zswap/stored_pages; }
ev() { awk '$1 ~ /^(zswpout|zswpin|zswpwb|pswpout|pswpin)$/ {printf "%s=%s ", $1, $2}' "$cg/memory.stat"; }
zs=$(cat /sys/module/zswap/parameters/zpool)/$(cat /sys/module/zswap/parameters/compressor)
pre=
((APP)) && pre=app:
./start.sh cage "$id" 1280 720 "$port" "$pre${url[$p]}" "${F[@]}" || exit 1
./waitload.sh "$port" 120 nav || echo "e4: $p load timeout" >&2
sleep 20
echo "$wb" | sudo tee "$cg/memory.zswap.writeback" > /dev/null
m0=$(sudo ./mem.sh "$cg") s0=$(swap_mib) p0=$(pool) q0=$(stored) tf=- tr=-
case $mode in freeze-*)
  t0=$EPOCHREALTIME
  echo 1 | sudo tee "$cg/cgroup.freeze" > /dev/null
  until grep -q '^frozen 1' "$cg/cgroup.events"; do sleep 0.001; done
  tf=$(ms "$t0" "$EPOCHREALTIME") ;;
esac
case $mode in *reclaim*)
  arg=64G
  [ "$mode" = freeze-reclaim-anon ] && arg="64G swappiness=200"
  [ "$mode" = freeze-reclaim-anonfit ] && arg="$(awk '$1 == "anon" || $1 == "shmem" {s += $2} END {print s}' "$cg/memory.stat") swappiness=200"
  t0=$EPOCHREALTIME
  # The target exceeds the cgroup, so the write ends with EAGAIN once
  # nothing is left. A cgroup with no LRU pages can spin instead (seen on
  # 6.12), so a timeout interrupts the write.
  sudo timeout -s INT 60 sh -c "echo '$arg' > $cg/memory.reclaim" 2> /dev/null
  tr=$(ms "$t0" "$EPOCHREALTIME") ;;
esac
m1=$(sudo ./mem.sh "$cg") s1=$(swap_mib) e1="$(ev)pool=$(($(pool) - p0)) stored=$(($(stored) - q0))"
sleep "$hold"
m2=$(sudo ./mem.sh "$cg")
t0=$EPOCHREALTIME
echo 0 | sudo tee "$cg/cgroup.freeze" > /dev/null
until grep -q '^frozen 0' "$cg/cgroup.events"; do sleep 0.001; done
tt=$(ms "$t0" "$EPOCHREALTIME")
ws_open "$(ws_page "$port")"
ws_call Runtime.evaluate '{"expression":"1+1","returnByValue":true}' > /dev/null
t1=$(ms "$t0" "$EPOCHREALTIME")
XDG_RUNTIME_DIR=$B/run/$id WAYLAND_DISPLAY=wayland-0 grim -t jpeg "$B/results/$id.jpg"
t2=$(ms "$t0" "$EPOCHREALTIME")
step='{"expression":"window.scrollBy(0, 600); new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))","awaitPromise":true}'
ws_call Runtime.evaluate "$step" > /dev/null
ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' > /dev/null
t3=$(ms "$t0" "$EPOCHREALTIME")
t4a=$EPOCHREALTIME
ws_call Runtime.evaluate "$step" > /dev/null
ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' > /dev/null
t4=$(ms "$t4a" "$EPOCHREALTIME")
ws_close
sleep 5
m3=$(sudo ./mem.sh "$cg") s3=$(swap_mib) e3=$(ev)
printf '%s\t%s\t%s\tzswap=%s wb=%s flags=%s\tbefore[%s] swap=%s\tfreeze_ms=%s reclaim_ms=%s\tparked[%s] swap=%s %s\tafter_hold[%s]\tthaw_ms=%s first_cdp_ms=%s first_grim_ms=%s scroll_shot_ms=%s next_scroll_shot_ms=%s\tresumed[%s] swap=%s %s\n' \
  "$p" "$mode" "$hold" "$zs" "$wb" "$flags" "$m0" "$s0" "$tf" "$tr" "$m1" "$s1" "$e1" \
  "$m2" "$tt" "$t1" "$t2" "$t3" "$t4" "$m3" "$s3" "$e3" | tee -a "$out"
sudo systemctl stop "glarb-$id"
sleep 3
