#!/usr/bin/env bash
# E10: what a page costs between agent steps, and what hiding it saves.
# N Cage + Chromium agents (proc flags, --app) show one page. The agents'
# CPU is recorded for 30 s in each steady mode: visible with the output at
# 60, 10, and 2 Hz, and hidden two ways. Hidden pages fall under Chromium's
# background policy: timers wake once a second and nothing renders.
#   hidden-min  the window is minimized through CDP. Captures of a minimized
#               window can hang until it is restored.
#   hidden-cdp  a CDP freeze then resume (Page.setWebLifecycleState) leaves
#               the page hidden in a normal window; captures and input work.
# Then agents take steps round-robin for 60 s, one step per agent about
# every 4 s: capture, scroll 300 px and wait two frames, wait 300 ms,
# capture. The page is shown for its own step and hidden after: hide-min
# restores and minimizes the window; hide-cdp turns focus emulation on and
# off over a hidden-cdp page. Each step talks over one browser-level
# connection with a flattened page session. Appends to results/e10.tsv.
# usage: [STEADY_S=30] [STEPS_S=60] e10.sh PAGE [N]
#   PAGE: tick | spin | gh | wd | wiki | cnn
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./pages.sh && . ./flags.sh
p=$1 n=${2:-8} ss=${STEADY_S:-30} st=${STEPS_S:-60}
out=$B/results/e10.tsv
url[tick]=file://$B/pages/tick.html
url[spin]='data:text/html,<style>div{width:80px;height:80px;border:8px solid %23ccc;border-top-color:%23333;border-radius:50%25;animation:s 1s linear infinite}@keyframes s{to{transform:rotate(360deg)}}</style><div></div><p>loading</p>'
url[cnn]=https://www.cnn.com
pt() { echo $((9900 + $1)); }
cleanup() {
  for i in $(seq "$n"); do echo "glarb-e10n$i"; done | xargs sudo systemctl stop
  sudo systemctl reset-failed 'glarb-e10n*' 2> /dev/null
}
trap 'exit 1' INT TERM
trap cleanup EXIT
cpu_us() { # summed over the agents' units, so nothing else in the slice counts
  local i s=0
  for i in $(seq "$n"); do
    s=$((s + $(awk '$1 == "usage_usec" {print $2}' \
      "/sys/fs/cgroup/glarb.slice/glarb-e10n$i.service/cpu.stat")))
  done
  echo "$s"
}
per_agent() { awk -v a="$1" -v b="$2" -v s="$3" -v n="$n" \
  'BEGIN {printf "%.2f", (b - a) / (s * 1e4) / n}'; }
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
hz() {
  local i
  for i in $(seq "$n"); do
    XDG_RUNTIME_DIR=$B/run/e10n$i WAYLAND_DISPLAY=wayland-0 \
      wlr-randr --output HEADLESS-1 --custom-mode "1280x720@${1}Hz"
  done
}
bounds() { echo "{\"windowId\":${wid[$1]},\"bounds\":{\"windowState\":\"$2\"}}"; }
win() { # STATE [AGENT]: minimized | normal, for one agent or all
  local i
  for i in ${2:-$(seq "$n")}; do
    ./cdp.sh "$(pt "$i")" Browser.setWindowBounds "$(bounds "$i" "$1")" browser > /dev/null
  done
}
cdphide() { # [AGENT]: hide pages through a CDP freeze and resume
  local i
  for i in ${1:-$(seq "$n")}; do
    ws_open "${pws[$i]}"
    ws_call Page.setWebLifecycleState '{"state":"frozen"}' > /dev/null
    ws_call Page.setWebLifecycleState '{"state":"active"}' > /dev/null
    ws_close
  done
}
unhide() { win minimized "${1:-}"; win normal "${1:-}"; } # [AGENT]
scall() { # SESSION METHOD [PARAMS]: a call on a flattened page session
  local params=${3:-'{}'} id=$(( ${EPOCHREALTIME/./} % 2147483647 + 1 ))
  jq -cn --argjson i "$id" --arg m "$2" --argjson p "$params" --arg s "$1" \
    '{id: $i, method: $m, params: $p, sessionId: $s}' >&"$WS_OUT"
  timeout 10 grep -m1 "^{\"id\":$id," <&"$WS_IN"
}
ev() { ws_call Runtime.evaluate "$(jq -cn --arg e "$1" \
  '{expression: $e, returnByValue: true, awaitPromise: true}')"; }
# A 20 px overlay in the corner, and a hash of a 10 px capture of it.
paint() {
  ev "(() => { let d = document.getElementById('e10px'); if (!d) {
    d = document.createElement('div'); d.id = 'e10px';
    d.style.cssText = 'position:fixed;left:0;top:0;width:20px;height:20px;z-index:2147483647';
    document.body.append(d) } d.style.background = '$1'; return 1 })()" > /dev/null
}
clip() { # LABEL: also keeps the capture as logs/e10-PAGE-LABEL.png.b64
  WS_TIMEOUT=10 ws_call Page.captureScreenshot \
  '{"format":"png","clip":{"x":0,"y":0,"width":10,"height":10,"scale":1}}' |
  jq -r '.result.data // "ERR"' | tee "$B/logs/e10-$p-$1.png.b64" | md5sum | cut -c1-8; }
pcts() { # COLUMN FILE: p50/p95 of one column of a latency file
  awk -v c="$1" '{print $c}' "$2" | sort -n |
    awk '{a[NR] = $1} END {printf "%d/%d", a[int(NR * .5 + .5)], a[int(NR * .95 + .5)]}'
}
steady() { # MODE
  local u0
  sleep 5
  u0=$(cpu_us)
  sleep "$ss"
  printf 'p=%s n=%s mode=%s\tper_agent_cpu_pct=%s\n' "$p" "$n" "$1" \
    "$(per_agent "$u0" "$(cpu_us)" "$ss")" | tee -a "$out"
}
scroll='window.scrollBy(0, 300); new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))'
shot='{"format":"jpeg","quality":80}'
steps() { # MODE: visible | hide-min | hide-cdp
  local i r0 t0 t1 t2 t3 u0 s0 sid end bad=0 lat=$B/logs/e10-$p-$1.lat
  : > "$lat"
  u0=$(cpu_us) s0=$EPOCHREALTIME end=$((SECONDS + st))
  while ((SECONDS < end)); do
    r0=$EPOCHREALTIME
    for i in $(seq "$n"); do
      ws_open "${bws[$i]}"
      sid=$(ws_call Target.attachToTarget "{\"targetId\":\"${tid[$i]}\",\"flatten\":true}" |
        jq -r .result.sessionId)
      t0=$EPOCHREALTIME
      case $1 in
        hide-min) ws_call Browser.setWindowBounds "$(bounds "$i" normal)" > /dev/null ;;
        hide-cdp) scall "$sid" Emulation.setFocusEmulationEnabled '{"enabled":true}' > /dev/null ;;
      esac
      t1=$EPOCHREALTIME
      scall "$sid" Page.captureScreenshot "$shot" | grep -q . || bad=$((bad + 1))
      t2=$EPOCHREALTIME
      scall "$sid" Runtime.evaluate "$(jq -cn --arg e "$scroll" \
        '{expression: $e, awaitPromise: true}')" | grep -q . || bad=$((bad + 1))
      sleep 0.3
      scall "$sid" Page.captureScreenshot "$shot" | grep -q . || bad=$((bad + 1))
      t3=$EPOCHREALTIME
      case $1 in
        hide-min) ws_call Browser.setWindowBounds "$(bounds "$i" minimized)" > /dev/null ;;
        hide-cdp) scall "$sid" Emulation.setFocusEmulationEnabled '{"enabled":false}' > /dev/null ;;
      esac
      ws_call Target.detachFromTarget "{\"sessionId\":\"$sid\"}" > /dev/null
      ws_close
      echo "$(ms "$t0" "$t1") $(ms "$t0" "$t2") $(ms "$t0" "$t3")" >> "$lat"
    done
    sleep "$(awk -v a="$r0" -v b="$EPOCHREALTIME" \
      'BEGIN {d = 4 - (b - a); print (d > 0 ? d : 0)}')"
  done
  printf 'p=%s n=%s mode=steps-%s\tper_agent_cpu_pct=%s steps=%s timeouts=%s\tshow_ms=%s first_shot_ms=%s step_ms=%s (p50/p95)\n' \
    "$p" "$n" "$1" \
    "$(per_agent "$u0" "$(cpu_us)" "$(awk -v a="$s0" -v b="$EPOCHREALTIME" 'BEGIN {print b - a}')")" \
    "$(wc -l < "$lat")" "$bad" "$(pcts 1 "$lat")" "$(pcts 2 "$lat")" "$(pcts 3 "$lat")" |
    tee -a "$out"
}

lean_flags proc
for i in $(seq "$n"); do
  ./start.sh cage "e10n$i" 1280 720 "$(pt "$i")" "app:${url[$p]}" "${F[@]}" ||
    echo "e10: start $i failed" >&2
done
for i in $(seq "$n"); do
  ./waitload.sh "$(pt "$i")" 120 nav || echo "e10: $p agent $i load timeout" >&2
done
sleep 20
declare -A wid tid pws bws
for i in $(seq "$n"); do
  pws[$i]=$(ws_page "$(pt "$i")")
  bws[$i]=$(curl -s "http://127.0.0.1:$(pt "$i")/json/version" | jq -r .webSocketDebuggerUrl)
  tid[$i]=$(curl -s "http://127.0.0.1:$(pt "$i")/json/list" |
    jq -r '[.[] | select(.type == "page")][0].id')
  wid[$i]=$(./cdp.sh "$(pt "$i")" Browser.getWindowForTarget \
    "{\"targetId\":\"${tid[$i]}\"}" browser | jq -r .windowId)
done
# Are captures of a hidden-cdp page fresh? Compare with visible references.
ws_open "${pws[1]}"
paint red; sleep 0.2; rr=$(clip vis-red); paint blue; sleep 0.2; rb=$(clip vis-blue)
ws_close
cdphide 1
ws_open "${pws[1]}"
paint red; hr=$(clip hid-red); paint blue; hb=$(clip hid-blue)
vis=$(ev document.visibilityState | jq -r .result.result.value)
ev "document.getElementById('e10px').remove(); 1" > /dev/null
ws_close
unhide 1
fresh=no
[ "$hr" = "$rr" ] && [ "$hb" = "$rb" ] && [ "$rr" != "$rb" ] && fresh=yes
printf 'p=%s n=%s mode=fresh\thidden_state=%s capture_fresh=%s refs=%s,%s hidden=%s,%s\n' \
  "$p" "$n" "$vis" "$fresh" "$rr" "$rb" "$hr" "$hb" | tee -a "$out"
steady visible60
hz 10; steady visible10
hz 2; steady visible2
hz 60
win minimized; steady hidden-min
steps hide-min
win normal
cdphide; steady hidden-cdp
steps hide-cdp
unhide
steps visible
