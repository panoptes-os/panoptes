#!/usr/bin/env bash
# E7 inputs: distinct 1280x720 JPEG screenshots of real pages, taken while
# scrolling, so that no two simulated agents send the same image sequence.
# Writes results/shots/PAGE-K.jpg and drops exact duplicates.
# usage: e7shots.sh PER_PAGE
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./pages.sh
k=${1:-24} id=e7s port=9550 d=$B/results/shots
mkdir -p "$d" && rm -f "$d"/*.jpg
for p in wiki gh wd; do
  ./start.sh cage "$id" 1280 720 "$port" "${url[$p]}" || continue
  ./waitload.sh "$port" 60 nav
  sleep 8
  ws_open "$(ws_page "$port")"
  for i in $(seq "$k"); do
    ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' |
      jq -r .result.data | base64 -d > "$d/$p-$i.jpg"
    ws_call Runtime.evaluate '{"expression":"window.scrollBy(0, 500); new Promise(r => setTimeout(r, 300))","awaitPromise":true}' > /dev/null
  done
  ws_close
  sudo systemctl stop "glarb-$id"
  sleep 2
done
find "$d" -name "*.jpg" -size 0 -delete
md5sum "$d"/*.jpg | sort | awk 'seen[$1]++ {print $2}' | xargs -r rm
echo "shots: $(ls "$d" | wc -l) unique"
