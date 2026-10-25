#!/usr/bin/env bash
# E10b: two more ways to idle a page between agent steps, against E10's
# visible baseline and the output held at 2 Hz. N Cage + Chromium agents
# (proc flags, --app) show one page and take steps round-robin as in E10,
# 60 s per mode:
#   visible  output at 60 Hz throughout
#   2hz      output at 2 Hz throughout, so a step's frames wait on it too
#   hz       output at 2 Hz between an agent's steps and 60 Hz during them
#   freeze   the agent's unit frozen between its steps and thawed for each
# Timings start before the connection and page session, so show_ms
# includes both; E10's started after them. A root reader on a FIFO writes
# cgroup.freeze, so no step pays for sudo. hz pays for a wlr-randr process
# per change, which a compositor would not.
# First, E10's freshness check, plus visible captures taken right after the
# change, as the hidden ones are. Captures are kept as
# logs/e10b-PAGE-LABEL.png.b64. Appends to results/e10b.tsv.
# usage: [STEPS_S=60] [MODES="visible 2hz hz freeze"] e10b.sh PAGE [N]
#   PAGE: tick | spin | gh | wd | wiki | cnn
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./pages.sh && . ./flags.sh
p=$1 n=${2:-8} st=${STEPS_S:-60}
out=$B/results/e10b.tsv fifo=$B/run/e10b-freeze
url[tick]=file://$B/pages/tick.html
url[spin]='data:text/html,<style>div{width:80px;height:80px;border:8px solid %23ccc;border-top-color:%23333;border-radius:50%25;animation:s 1s linear infinite}@keyframes s{to{transform:rotate(360deg)}}</style><div></div><p>loading</p>'
url[cnn]=https://www.cnn.com
pt() { echo $((9920 + $1)); }
declare -A cg
for i in $(seq "$n"); do cg[$i]=/sys/fs/cgroup/glarb.slice/glarb-e10bn$i.service; done
cleanup() {
  local i
  [ -n "${FRZ:-}" ] && exec {FRZ}>&-
  for i in $(seq "$n"); do
    echo 0 | sudo tee "${cg[$i]}/cgroup.freeze" > /dev/null 2>&1
    echo "glarb-e10bn$i"
  done | xargs sudo systemctl stop
  sudo systemctl reset-failed 'glarb-e10bn*' 2> /dev/null
  rm -f "$fifo"
}
trap 'exit 1' INT TERM
trap cleanup EXIT
cpu_us() { # summed over the agents' units, so nothing else in the slice counts
  local i s=0
  for i in $(seq "$n"); do
    s=$((s + $(awk '$1 == "usage_usec" {print $2}' "${cg[$i]}/cpu.stat")))
  done
  echo "$s"
}
per_agent() { awk -v a="$1" -v b="$2" -v s="$3" -v n="$n" \
  'BEGIN {printf "%.2f", (b - a) / (s * 1e4) / n}'; }
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
hz1() { # RATE AGENT
  XDG_RUNTIME_DIR=$B/run/e10bn$2 WAYLAND_DISPLAY=wayland-0 \
    wlr-randr --output HEADLESS-1 --custom-mode "1280x720@${1}Hz"
}
hz() { local i; for i in $(seq "$n"); do hz1 "$1" "$i"; done; } # RATE
frz() { echo "${cg[$1]}/cgroup.freeze $2" >&"$FRZ"; } # AGENT 0|1, no reply
thawed() { # AGENT: true once its unit reports frozen 0; reads, never forks
  local k v
  while read -r k v; do [ "$k" = frozen ] && return "$v"; done < "${cg[$1]}/cgroup.events"
  return 1
}
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
clip() { # LABEL
  WS_TIMEOUT=10 ws_call Page.captureScreenshot \
  '{"format":"png","clip":{"x":0,"y":0,"width":10,"height":10,"scale":1}}' |
  jq -r '.result.data // "ERR"' | tee "$B/logs/e10b-$p-$1.png.b64" | md5sum | cut -c1-8; }
pcts() { # COLUMN FILE: p50/p95 of one column of a latency file
  awk -v c="$1" '{print $c}' "$2" | sort -n |
    awk '{a[NR] = $1} END {printf "%d/%d", a[int(NR * .5 + .5)], a[int(NR * .95 + .5)]}'
}
scroll='window.scrollBy(0, 300); new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))'
shot='{"format":"jpeg","quality":80}'
steps() { # MODE: visible | 2hz | hz | freeze
  local i r0 t0 t1 t2 t3 u0 s0 el sid end bad=0 lat=$B/logs/e10b-$p-$1.lat
  : > "$lat"
  case $1 in
    2hz | hz) hz 2 ;;
    freeze)
      for i in $(seq "$n"); do frz "$i" 1; done
      for i in $(seq "$n"); do while thawed "$i"; do sleep 0.01; done; done
      ;;
  esac
  sleep 5
  u0=$(cpu_us) s0=$EPOCHREALTIME end=$((SECONDS + st))
  while ((SECONDS < end)); do
    r0=$EPOCHREALTIME
    for i in $(seq "$n"); do
      t0=$EPOCHREALTIME
      case $1 in
        hz) hz1 60 "$i" ;;
        freeze) frz "$i" 0; until thawed "$i"; do :; done ;;
      esac
      ws_open "${bws[$i]}"
      sid=$(ws_call Target.attachToTarget "{\"targetId\":\"${tid[$i]}\",\"flatten\":true}" |
        jq -r .result.sessionId)
      t1=$EPOCHREALTIME
      scall "$sid" Page.captureScreenshot "$shot" | grep -q . || bad=$((bad + 1))
      t2=$EPOCHREALTIME
      scall "$sid" Runtime.evaluate "$(jq -cn --arg e "$scroll" \
        '{expression: $e, awaitPromise: true}')" | grep -q . || bad=$((bad + 1))
      sleep 0.3
      scall "$sid" Page.captureScreenshot "$shot" | grep -q . || bad=$((bad + 1))
      t3=$EPOCHREALTIME
      ws_call Target.detachFromTarget "{\"sessionId\":\"$sid\"}" > /dev/null
      ws_close
      case $1 in
        hz) hz1 2 "$i" ;;
        freeze) frz "$i" 1 ;;
      esac
      echo "$(ms "$t0" "$t1") $(ms "$t0" "$t2") $(ms "$t0" "$t3")" >> "$lat"
    done
    sleep "$(awk -v a="$r0" -v b="$EPOCHREALTIME" \
      'BEGIN {d = 4 - (b - a); print (d > 0 ? d : 0)}')"
  done
  # Slow steps stretch the cadence past 4 s, so report the mean round too.
  el=$(awk -v a="$s0" -v b="$EPOCHREALTIME" 'BEGIN {print b - a}')
  printf 'p=%s n=%s mode=steps-%s pos=%s\tper_agent_cpu_pct=%s steps=%s timeouts=%s round_s=%s\tshow_ms=%s first_shot_ms=%s step_ms=%s (p50/p95)\n' \
    "$p" "$n" "$1" "$pos" "$(per_agent "$u0" "$(cpu_us)" "$el")" "$(wc -l < "$lat")" "$bad" \
    "$(awk -v e="$el" -v s="$(wc -l < "$lat")" -v n="$n" 'BEGIN {printf "%.1f", e * n / s}')" \
    "$(pcts 1 "$lat")" "$(pcts 2 "$lat")" "$(pcts 3 "$lat")" | tee -a "$out"
  case $1 in
    2hz | hz) hz 60 ;;
    freeze) for i in $(seq "$n"); do frz "$i" 0; done ;;
  esac
}

lean_flags proc
for i in $(seq "$n"); do
  ./start.sh cage "e10bn$i" 1280 720 "$(pt "$i")" "app:${url[$p]}" "${F[@]}" ||
    echo "e10b: start $i failed" >&2
done
for i in $(seq "$n"); do
  ./waitload.sh "$(pt "$i")" 120 nav || echo "e10b: $p agent $i load timeout" >&2
done
sleep 20
rm -f "$fifo" && mkfifo "$fifo" || exit 1
sudo bash -c 'while read -r f v; do echo "$v" > "$f"; done < "$1"' _ "$fifo" &
exec {FRZ}> "$fifo"
declare -A tid pws bws
for i in $(seq "$n"); do
  pws[$i]=$(ws_page "$(pt "$i")")
  bws[$i]=$(curl -s "http://127.0.0.1:$(pt "$i")/json/version" | jq -r .webSocketDebuggerUrl)
  tid[$i]=$(curl -s "http://127.0.0.1:$(pt "$i")/json/list" |
    jq -r '[.[] | select(.type == "page")][0].id')
done
# Are captures fresh? Visible references after a 200 ms wait, then visible
# and hidden captures taken right after each change.
ws_open "${pws[1]}"
paint red; sleep 0.2; rr=$(clip vis-red); paint blue; sleep 0.2; rb=$(clip vis-blue)
paint red; nr=$(clip now-red); paint blue; nb=$(clip now-blue)
ws_call Page.setWebLifecycleState '{"state":"frozen"}' > /dev/null
ws_call Page.setWebLifecycleState '{"state":"active"}' > /dev/null
paint red; hr=$(clip hid-red); paint blue; hb=$(clip hid-blue)
ev "document.getElementById('e10px').remove(); 1" > /dev/null
ws_close
# Show the page again: minimize, then restore its window.
wid=$(./cdp.sh "$(pt 1)" Browser.getWindowForTarget "{\"targetId\":\"${tid[1]}\"}" browser |
  jq -r .windowId)
for s in minimized normal; do
  ./cdp.sh "$(pt 1)" Browser.setWindowBounds \
    "{\"windowId\":$wid,\"bounds\":{\"windowState\":\"$s\"}}" browser > /dev/null
done
same() { [ "$1" = "$3" ] && [ "$2" = "$4" ] && [ "$3" != "$4" ] && echo yes || echo no; }
printf 'p=%s n=%s mode=fresh\tfresh_now=%s fresh_hidden=%s\trefs=%s,%s now=%s,%s hidden=%s,%s\n' \
  "$p" "$n" "$(same "$nr" "$nb" "$rr" "$rb")" "$(same "$hr" "$hb" "$rr" "$rb")" \
  "$rr" "$rb" "$nr" "$nb" "$hr" "$hb" | tee -a "$out"
# A live page can keep working for minutes after it loads, which inflates
# whichever mode runs first; MODES reverses the order for a second run.
pos=0
for m in ${MODES:-visible 2hz hz freeze}; do pos=$((pos + 1)); steps "$m"; done
