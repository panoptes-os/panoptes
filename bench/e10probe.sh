#!/usr/bin/env bash
# E10 pilot: what Page.setWebLifecycleState does to a visible kiosk page on
# this Chromium. One Cage kiosk on the tick page; prints CPU, the page's
# timer count, visibility, and whether captures change, around a freeze.
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./flags.sh
id=e10p port=9951 cg=/sys/fs/cgroup/glarb.slice/glarb-e10p.service
cpu() { awk '$1 == "usage_usec" {print $2}' "$cg/cpu.stat"; }
pct() { awk -v a="$1" -v b="$2" -v s="$3" 'BEGIN {printf "%.2f", (b - a) / (s * 1e4)}'; }
ev() { ws_call Runtime.evaluate "$(jq -cn --arg e "$1" '{expression: $e, returnByValue: true}')" |
  jq -c '.result.result.value // .error // .result'; }
shot() { ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' |
  jq -r '.result.data // "ERR"' | md5sum | cut -c1-8; }
lean_flags proc
./start.sh cage "$id" 1280 720 "$port" "app:file://$B/pages/tick.html" "${F[@]}" || exit 1
./waitload.sh "$port" 60 nav
sleep 5
ws_open "$(ws_page "$port")"
c0=$(cpu); sleep 10; c1=$(cpu)
echo "active: cpu_pct=$(pct "$c0" "$c1" 10) ticks=$(ev window.ticks) vis=$(ev document.visibilityState) shots=$(shot),$(sleep 0.3; shot)"
t0=$EPOCHREALTIME
r=$(ws_call Page.setWebLifecycleState '{"state":"frozen"}' | cut -c1-120)
echo "freeze reply: $r ($(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}') ms)"
c0=$(cpu); sleep 10; c1=$(cpu)
echo "frozen: cpu_pct=$(pct "$c0" "$c1" 10)"
echo "frozen eval: ticks=$(timeout 5 bash -c ". ./ws.sh; true" ; ev window.ticks) vis=$(ev document.visibilityState)"
t0=$EPOCHREALTIME
r=$(ws_call Page.setWebLifecycleState '{"state":"active"}' | cut -c1-120)
echo "resume reply: $r ($(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}') ms)"
t0=$EPOCHREALTIME; s1=$(shot); t1=$EPOCHREALTIME
echo "after resume: shot_ms=$(awk -v a="$t0" -v b="$t1" 'BEGIN {printf "%d", (b - a) * 1000}') ticks=$(ev window.ticks) vis=$(ev document.visibilityState) shots=$s1,$(sleep 0.3; shot)"
c0=$(cpu); sleep 10; c1=$(cpu)
echo "after resume: cpu_pct=$(pct "$c0" "$c1" 10) ticks=$(ev window.ticks) vis=$(ev document.visibilityState)"
ws_call Page.bringToFront > /dev/null
echo "after bringToFront: vis=$(ev document.visibilityState) shots=$(shot),$(sleep 0.3; shot)"
ws_close
sudo systemctl stop "glarb-$id"
