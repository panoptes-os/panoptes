#!/usr/bin/env bash
# E10 pilot 3: are captures of a hidden kiosk page fresh, does focus
# emulation toggle visibility both ways, and what does a restore cost?
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./flags.sh
id=e10p port=9951
ev() { ws_call Runtime.evaluate "$(jq -cn --arg e "$1" '{expression: $e, returnByValue: true}')" |
  jq -c '.result.result.value // .error // .result'; }
ms() { awk -v a="$1" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}'; }
tps() { local a b; a=$(ev window.ticks); sleep 5; b=$(ev window.ticks); echo "ticks_per_s=$(( (b - a) / 5 )) vis=$(ev document.visibilityState)"; }
# A 20 px overlay in the corner; hash a 10 px capture of it.
paint() { ev "(() => { let d = document.getElementById('px'); if (!d) { d = document.createElement('div'); d.id = 'px';
  d.style.cssText = 'position:fixed;left:0;top:0;width:20px;height:20px;z-index:2147483647'; document.body.append(d) }
  d.style.background = '$1'; return 1 })()" > /dev/null; }
clip() { ws_call Page.captureScreenshot '{"format":"png","clip":{"x":0,"y":0,"width":10,"height":10,"scale":1}}' |
  jq -r '.result.data // "ERR"' | md5sum | cut -c1-8; }
win() { ./cdp.sh "$port" Browser.setWindowBounds "{\"windowId\":$wid,\"bounds\":{\"windowState\":\"$1\"}}" browser > /dev/null; }
lean_flags proc
./start.sh cage "$id" 1280 720 "$port" "app:file://$B/pages/tick.html" "${F[@]}" || exit 1
./waitload.sh "$port" 60 nav
sleep 3
tid=$(curl -s "http://127.0.0.1:$port/json/list" | jq -r '[.[] | select(.type == "page")][0].id')
wid=$(./cdp.sh "$port" Browser.getWindowForTarget "{\"targetId\":\"$tid\"}" browser | jq -r .windowId)
ws_open "$(ws_page "$port")"
paint red; sleep 0.2; rr=$(clip); paint blue; sleep 0.2; rb=$(clip)
echo "visible refs: red=$rr blue=$rb; $(tps)"
win minimized; sleep 0.3
paint red; hr=$(clip); paint blue; hb=$(clip)
echo "minimized: red=$hr blue=$hb fresh=$([ "$hr" = "$rr" ] && [ "$hb" = "$rb" ] && echo yes || echo no); $(tps)"
ws_call Emulation.setFocusEmulationEnabled '{"enabled":true}' > /dev/null; sleep 0.3
echo "focus emulation on: $(tps)"
ws_call Emulation.setFocusEmulationEnabled '{"enabled":false}' > /dev/null; sleep 0.3
echo "focus emulation off: $(tps)"
win normal; sleep 0.3
echo "normal: $(tps)"
t0=$EPOCHREALTIME; ./cdp.sh "$port" Browser.getVersion '{}' browser > /dev/null; echo "cdp.sh noop_ms=$(ms "$t0")"
for k in 1 2 3 4 5; do
  win minimized; sleep 1
  t0=$EPOCHREALTIME; win normal; a=$(ms "$t0")
  until [ "$(ev document.visibilityState)" = '"visible"' ]; do :; done; b=$(ms "$t0")
  paint red; c=$(clip); d=$(ms "$t0")
  echo "restore $k: call_ms=$a visible_ms=$b fresh_shot_ms=$d fresh=$([ "$c" = "$rr" ] && echo yes || echo no)"
done
ws_close
sudo systemctl stop "glarb-$id"
