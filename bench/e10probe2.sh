#!/usr/bin/env bash
# E10 pilot 2: a kiosk page left hidden by a CDP freeze and resume. Does
# input still run at once, do captures stay fresh, and which CDP call makes
# the page visible again?
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./flags.sh
id=e10p port=9951 cg=/sys/fs/cgroup/glarb.slice/glarb-e10p.service
cpu() { awk '$1 == "usage_usec" {print $2}' "$cg/cpu.stat"; }
pct() { awk -v a="$1" -v b="$2" -v s="$3" 'BEGIN {printf "%.2f", (b - a) / (s * 1e4)}'; }
ev() { ws_call Runtime.evaluate "$(jq -cn --arg e "$1" '{expression: $e, returnByValue: true}')" |
  jq -c '.result.result.value // .error // .result'; }
ms() { awk -v a="$1" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}'; }
rate() { local a b t0; a=$(ev window.ticks); c0=$(cpu); t0=$EPOCHREALTIME; sleep 10
  b=$(ev window.ticks); echo "cpu_pct=$(pct "$c0" "$(cpu)" 10) ticks_per_s=$(( (b - a) / 10 )) vis=$(ev document.visibilityState)"; }
click() { # time from dispatch until the page's click handler ran
  ev 'window.clicked = 0; document.onclick = () => { window.clicked = performance.now() }; 0' > /dev/null
  local t0=$EPOCHREALTIME
  for ty in mousePressed mouseReleased; do
    ws_call Input.dispatchMouseEvent "{\"type\":\"$ty\",\"x\":640,\"y\":600,\"button\":\"left\",\"clickCount\":1}" > /dev/null
  done
  echo "click_ack_ms=$(ms "$t0") handler_ran=$(ev 'window.clicked > 0')"; }
shot() { local t0=$EPOCHREALTIME
  h=$(ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' | jq -r '.result.data // "ERR"' | md5sum | cut -c1-8)
  echo "shot_ms=$(ms "$t0") $h"; }
lean_flags proc
./start.sh cage "$id" 1280 720 "$port" "app:file://$B/pages/tick.html" "${F[@]}" || exit 1
./waitload.sh "$port" 60 nav
sleep 5
ws_open "$(ws_page "$port")"
echo "visible: $(rate) | $(click) | $(shot) | $(shot)"
ws_call Page.setWebLifecycleState '{"state":"frozen"}' > /dev/null
ws_call Page.setWebLifecycleState '{"state":"active"}' > /dev/null
echo "hidden-active: $(rate) | $(click) | $(shot) | $(shot)"
tid=$(curl -s "http://127.0.0.1:$port/json/list" | jq -r '[.[] | select(.type == "page")][0].id')
curl -s "http://127.0.0.1:$port/json/activate/$tid" > /dev/null
echo "after /json/activate: vis=$(ev document.visibilityState)"
ws_call Emulation.setFocusEmulationEnabled '{"enabled":true}' > /dev/null
echo "after focus emulation: vis=$(ev document.visibilityState)"
ws_call Emulation.setFocusEmulationEnabled '{"enabled":false}' > /dev/null
bws=$(curl -s "http://127.0.0.1:$port/json/version" | jq -r .webSocketDebuggerUrl)
wid=$(./cdp.sh "$port" Browser.getWindowForTarget "{\"targetId\":\"$tid\"}" browser | jq -r .windowId)
./cdp.sh "$port" Browser.setWindowBounds "{\"windowId\":$wid,\"bounds\":{\"windowState\":\"minimized\"}}" browser
sleep 0.5
echo "after minimize: vis=$(ev document.visibilityState)"
./cdp.sh "$port" Browser.setWindowBounds "{\"windowId\":$wid,\"bounds\":{\"windowState\":\"normal\"}}" browser
sleep 0.5
echo "after normal: vis=$(ev document.visibilityState) $(rate)"
ws_close
sudo systemctl stop "glarb-$id"
