#!/usr/bin/env bash
# E11: park one agent inside a shared browser. One headless Chromium (proc
# flags, hang monitor off) holds N agents as BrowserContexts, one page each,
# pages rotating gh, wd, wiki. systemd starts the browser in the unit's leaf
# cgroup browser/ (DelegateSubgroup), and each agent gets a sibling,
# agent-N/. Agents are created one at a time while a root watcher moves
# every new renderer into that agent's cgroup. A memcg charge stays with the
# cgroup that first charged a page, so a renderer must move before it
# allocates for agent-N/ to hold its memory; the result reports how much of
# each agent's anonymous memory landed there. In production, CDP trace
# events would map renderers to agents instead, since a later navigation
# can spawn a renderer at any time.
# Then park agent 1: drop its context's HTTP cache, which lives in the
# browser process, freeze its cgroup, reclaim it into zswap (zsmalloc, zstd,
# writeback off), page out its renderers' remaining private memory with
# process_madvise (pageout.lua), hold, thaw, and time its first CDP call
# and screenshot.
# Another agent's screenshot is timed during the hold. Last, freeze agent 2
# for LONG_S and check that its renderer survives. Appends to
# results/e11.tsv. usage: [HOLD_S=20] [LONG_S=180] e11.sh N
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./pages.sh && . ./flags.sh
n=${1:-16} hold=${HOLD_S:-20} long=${LONG_S:-180}
id=e11 port=9750 out=$B/results/e11.tsv
cg=/sys/fs/cgroup/glarb.slice/glarb-$id.service
cur=$B/run/e11-agent
cleanup() {
  [ -n "${watcher:-}" ] && sudo kill "$watcher" 2> /dev/null
  for f in "$cg"/agent-*/cgroup.freeze; do echo 0 | sudo tee "$f" > /dev/null; done 2> /dev/null
  sudo systemctl stop "glarb-$id"
  sudo systemctl reset-failed "glarb-$id" 2> /dev/null
  ./zswap.sh zbud lz4 > /dev/null
}
trap 'exit 1' INT TERM
trap cleanup EXIT
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
mib() { echo $(($(cat "$1/memory.current") >> 20)); }
avail() { awk '/^MemAvailable:/ {print int($2 / 1024)}' /proc/meminfo; }
pool() { sudo awk '{printf "%d", $1 / 1048576}' /sys/kernel/debug/zswap/pool_total_size; }
renderers() { # CGROUP: renderer PIDs in it
  local p
  for p in $(cat "$1/cgroup.procs"); do
    tr '\0' ' ' < "/proc/$p/cmdline" 2> /dev/null | grep -q -- '--type=renderer' && echo "$p"
  done
}
anon_split() { # CGROUP: its renderers' resident anon MiB / anon charged here
  local p s=0
  for p in $(renderers "$1"); do
    s=$((s + $(awk '/^RssAnon:/ {print $2}' "/proc/$p/status" 2> /dev/null || echo 0)))
  done
  echo "$((s >> 10))/$(awk '$1 == "anon" {print int($2 / 1048576)}' "$1/memory.stat")"
}
./zswap.sh zsmalloc zstd > /dev/null
lean_flags proc
F+=(--disable-hang-monitor)
rot=(gh wd wiki)
EXTRA_PROPS="-p DelegateSubgroup=browser" \
  ./start.sh headless "$id" 1280 720 "$port" about:blank "${F[@]}" || exit 1
sleep 5
[ -d "$cg/browser" ] && [ -z "$(cat "$cg/cgroup.procs")" ] ||
  { echo "e11: browser/ leaf missing or unit cgroup not empty" >&2; exit 1; }
echo '+memory +cpu' | sudo tee "$cg/cgroup.subtree_control" > /dev/null || exit 1
for i in $(seq "$n"); do sudo mkdir "$cg/agent-$i"; done
echo 0 > "$cur"
# The watcher moves each new renderer into the cgroup of the agent being
# created; renderers that appear while no agent is being created stay put.
sudo ./e11watch.sh "$cg" "$cur" &
watcher=$!
bws=$(curl -s "http://127.0.0.1:$port/json/version" | jq -r .webSocketDebuggerUrl)
m0=$(mib "$cg") b0=$(mib "$cg/browser") a1=$(avail)
declare -A tgt pws
for i in $(seq "$n"); do
  p=${rot[$(((i - 1) % 3))]}
  echo "$i" > "$cur"
  ws_open "$bws"
  ctx=$(ws_call Target.createBrowserContext '{"disposeOnDetach":false}' |
    jq -r .result.browserContextId)
  tgt[$i]=$(ws_call Target.createTarget "$(jq -cn --arg u "${url[$p]}" --arg c "$ctx" \
    '{url: $u, browserContextId: $c}')" | jq -r .result.targetId)
  ws_close
  pws[$i]=ws://127.0.0.1:$port/devtools/page/${tgt[$i]}
  for _ in $(seq 240); do
    s=$(./cdp.sh "$port" Runtime.evaluate \
      '{"expression":"document.readyState + \" \" + location.href","returnByValue":true}' "${pws[$i]}" |
      jq -r '.result.value // empty' 2> /dev/null)
    case $s in "complete about:blank" | "") ;; "complete "*) break ;; esac
    sleep 0.5
  done
  sleep 2
  echo 0 > "$cur"
done
sudo kill "$watcher"
watcher=
sleep 60
m1=$(mib "$cg") b1=$(mib "$cg/browser") a2=$(avail)
rcount=() acur=() split=()
for i in $(seq "$n"); do
  rcount+=("$(renderers "$cg/agent-$i" | wc -l)")
  acur+=("$(mib "$cg/agent-$i")")
  split+=("$(anon_split "$cg/agent-$i")")
done
printf 'n=%s\tunit_mib=%s browser_mib=%s->%s per_agent_unit=%s per_agent_browser=%s per_agent_avail=%s\tagent_mib=%s renderers=%s browser_renderers=%s rss_anon/charged_anon=%s\n' \
  "$n" "$m1" "$b0" "$b1" $(((m1 - m0) / n)) $(((b1 - b0) / n)) $(((a1 - a2) / n)) \
  "$(IFS=,; echo "${acur[*]}")" "$(IFS=,; echo "${rcount[*]}")" \
  "$(renderers "$cg/browser" | wc -l)" "$(IFS=,; echo "${split[*]}")" | tee -a "$out"
sudo ./mem.sh "$cg" breakdown > "$B/logs/e11-$n.breakdown"

# Park agent 1 and time its return, and agent 3's screenshot meanwhile.
g=$cg/agent-1
pids1=$(renderers "$g" | sort | paste -sd,)
bc0=$(mib "$cg/browser")
ws_open "${pws[1]}"
ws_call Network.enable > /dev/null
ws_call Network.clearBrowserCache > /dev/null
ws_call Network.disable > /dev/null
ws_close
sleep 2
bc1=$(mib "$cg/browser")
echo 0 | sudo tee "$g/memory.zswap.writeback" > /dev/null
c0=$(mib "$g") z0=$(pool) av0=$(avail)
t0=$EPOCHREALTIME
echo 1 | sudo tee "$g/cgroup.freeze" > /dev/null
until grep -q '^frozen 1' "$g/cgroup.events"; do sleep 0.001; done
tf=$(ms "$t0" "$EPOCHREALTIME")
arg="$(awk '$1 == "anon" || $1 == "shmem" {s += $2} END {print s}' "$g/memory.stat") swappiness=200"
t0=$EPOCHREALTIME
sudo timeout -s INT 60 sh -c "echo '$arg' > $g/memory.reclaim" 2> /dev/null
tr=$(ms "$t0" "$EPOCHREALTIME")
c1=$(mib "$g") z1=$(pool) av1=$(avail) u1=$(mib "$cg") w1=$(mib "$cg/browser")
# The renderers' memory charged outside agent-1/ is out of the cgroup
# reclaim's reach; page it out per process.
t0=$EPOCHREALTIME
po=$(sudo "$(command -v luajit)" ./pageout.lua $(renderers "$g") |
  awk '{s += $2; e += $3} END {printf "%d errors=%d", s, e}')
tp=$(ms "$t0" "$EPOCHREALTIME")
z2=$(pool) av2=$(avail) u2=$(mib "$cg") w2=$(mib "$cg/browser")
ws_open "${pws[3]}"
t0=$EPOCHREALTIME
ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' > /dev/null
o1=$(ms "$t0" "$EPOCHREALTIME")
ws_close
sleep "$hold"
alive=$(ws_open "$bws"; ws_call Target.getTargets | jq --arg t "${tgt[1]}" \
  '[.result.targetInfos[] | select(.targetId == $t)] | length'; ws_close)
t0=$EPOCHREALTIME
echo 0 | sudo tee "$g/cgroup.freeze" > /dev/null
until grep -q '^frozen 0' "$g/cgroup.events"; do sleep 0.001; done
ws_open "${pws[1]}"
ws_call Runtime.evaluate '{"expression":"1+1","returnByValue":true}' > /dev/null
t1=$(ms "$t0" "$EPOCHREALTIME")
ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' > /dev/null
t2=$(ms "$t0" "$EPOCHREALTIME")
ws_close
sleep 2
c2=$(mib "$g")
printf 'n=%s park=agent-1 hold_s=%s\thttp_cache_clear_browser_mib=%s->%s freeze_ms=%s reclaim_ms=%s charge_mib=%s->%s->%s pool_mib=+%s avail_mib=+%s\tpageout_ms=%s range_mib=%s unit_mib=%s->%s browser_mib=%s->%s pool_mib=+%s avail_mib=+%s\tother_shot_ms=%s target_alive=%s first_cdp_ms=%s first_shot_ms=%s renderers_same=%s\n' \
  "$n" "$hold" "$bc0" "$bc1" "$tf" "$tr" "$c0" "$c1" "$c2" $((z1 - z0)) $((av1 - av0)) \
  "$tp" "$po" "$u1" "$u2" "$w1" "$w2" $((z2 - z1)) $((av2 - av1)) \
  "$o1" "$alive" "$t1" "$t2" \
  "$([ "$(renderers "$g" | sort | paste -sd,)" = "$pids1" ] && echo yes || echo no)" |
  tee -a "$out"

# A long freeze: does the browser keep a frozen renderer?
g=$cg/agent-2
pids2=$(renderers "$g" | sort | paste -sd,)
echo 1 | sudo tee "$g/cgroup.freeze" > /dev/null
until grep -q '^frozen 1' "$g/cgroup.events"; do sleep 0.001; done
sleep "$long"
echo 0 | sudo tee "$g/cgroup.freeze" > /dev/null
until grep -q '^frozen 0' "$g/cgroup.events"; do sleep 0.001; done
ws_open "${pws[2]}"
t0=$EPOCHREALTIME
r=$(WS_TIMEOUT=30 ws_call Runtime.evaluate '{"expression":"document.readyState","returnByValue":true}' |
  jq -r '.result.result.value // "none"')
t1=$(ms "$t0" "$EPOCHREALTIME")
ws_close
printf 'n=%s park=agent-2 freeze_only_s=%s\tready_after=%s first_cdp_ms=%s renderers_same=%s\n' \
  "$n" "$long" "$r" "$t1" \
  "$([ "$(renderers "$g" | sort | paste -sd,)" = "$pids2" ] && echo yes || echo no)" |
  tee -a "$out"
