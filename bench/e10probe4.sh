#!/usr/bin/env bash
# E10 pilot 4: the first capture after a minimize. Raw reply and timing.
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./flags.sh
id=e10p port=9951
trap 'sudo systemctl stop glarb-e10p' EXIT
ms() { awk -v a="$1" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}'; }
raw() { local t0=$EPOCHREALTIME r
  r=$(WS_TIMEOUT=1 ws_call Page.captureScreenshot '{"format":"png","clip":{"x":0,"y":0,"width":10,"height":10,"scale":1}}')
  echo "$(ms "$t0") ms: $(echo "$r" | cut -c1-90)"; }
lean_flags proc
./start.sh cage "$id" 1280 720 "$port" "app:file://$B/pages/tick.html" "${F[@]}" || exit 1
./waitload.sh "$port" 60 nav
sleep 20
tid=$(curl -s "http://127.0.0.1:$port/json/list" | jq -r '[.[] | select(.type == "page")][0].id')
wid=$(./cdp.sh "$port" Browser.getWindowForTarget "{\"targetId\":\"$tid\"}" browser | jq -r .windowId)
ws_open "$(ws_page "$port")"
echo "visible: $(raw)"
for d in 0.3 1 3; do
  ./cdp.sh "$port" Browser.setWindowBounds "{\"windowId\":$wid,\"bounds\":{\"windowState\":\"minimized\"}}" browser > /dev/null
  sleep "$d"
  echo "minimized +${d}s, first: $(raw)"
  echo "minimized +${d}s, second: $(raw)"
  ./cdp.sh "$port" Browser.setWindowBounds "{\"windowId\":$wid,\"bounds\":{\"windowState\":\"normal\"}}" browser > /dev/null
  sleep 1
done
ws_close
