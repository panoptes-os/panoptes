#!/usr/bin/env bash
# E1d: the shared-browser model. One Chromium holds N agents as isolated
# BrowserContexts with one page each (pages rotate gh, wd, wiki). Compare
# per-agent cost with N separate browsers in E1c. Appends to
# results/e1d.tsv. usage: e1d.sh N VARIANT [FLAGSET]   VARIANT: headless | cage
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./pages.sh && . ./ws.sh && . ./flags.sh
n=$1 v=${2:-headless} set=${3:-base}
id=e1d port=9700 slice=/sys/fs/cgroup/glarb.slice
cg=$slice/glarb-$id.service out=$B/results/e1d.tsv
avail() { awk '/^MemAvailable:/ {print int($2 / 1024)}' /proc/meminfo; }
cur() { echo $(($(cat "$slice/memory.current") >> 20)); }
lean_flags "$set"
rot=(gh wd wiki)
c0=$(cur) a0=$(avail)
./start.sh "$v" "$id" 1280 720 "$port" about:blank "${F[@]}" || exit 1
sleep 10
m0=$(sudo ./mem.sh "$cg") c1=$(cur)
ws_open "$(curl -s "http://127.0.0.1:$port/json/version" |
  jq -r .webSocketDebuggerUrl)"
for i in $(seq "$n"); do
  p=${rot[$(((i - 1) % 3))]}
  ctx=$(ws_call Target.createBrowserContext '{"disposeOnDetach":false}' |
    jq -r .result.browserContextId)
  ws_call Target.createTarget "$(jq -cn --arg u "${url[$p]}" --arg c "$ctx" \
    '{url: $u, browserContextId: $c}')" > /dev/null
done
deadline=$((SECONDS + 120))
for w in $(curl -s "http://127.0.0.1:$port/json/list" |
  jq -r '.[] | select(.type == "page") | .webSocketDebuggerUrl'); do
  while ((SECONDS < deadline)); do
    s=$(./cdp.sh "$port" Runtime.evaluate \
      '{"expression":"document.readyState","returnByValue":true}' "$w" |
      jq -r '.result.value // empty' 2> /dev/null)
    [ "$s" = complete ] && break
    sleep 0.5
  done
done
sleep 60
m1=$(sudo ./mem.sh "$cg") c2=$(cur) a1=$(avail)
pages=$(curl -s "http://127.0.0.1:$port/json/list" |
  jq '[.[] | select(.type == "page")] | length')
printf 'n=%s variant=%s set=%s pages=%s\tempty[%s]\tfull[%s]\tper_agent_current=%s per_agent_avail=%s\n' \
  "$n" "$v" "$set" "$pages" "$m0" "$m1" $(((c2 - c1) / n)) \
  $(((a0 - a1) / n)) | tee -a "$out"
sudo ./mem.sh "$cg" breakdown > "$B/logs/e1d-$n-$v-$set.breakdown"
ws_close
sudo systemctl stop "glarb-$id"
sleep 5
