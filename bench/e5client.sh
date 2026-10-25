#!/usr/bin/env bash
# E5 client: time one agent action N times over a persistent CDP socket.
# The action clicks a button, waits two animation frames, and captures a
# JPEG screenshot. Prints p50, p95, p99 and max in ms.
# usage: e5client.sh PORT N
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh
port=$1 n=${2:-100}
ws_open "$(ws_page "$port")"
act='{"expression":"document.getElementById(\"b\").click(); new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))","awaitPromise":true}'
for i in $(seq "$n"); do
  t0=$EPOCHREALTIME
  ws_call Runtime.evaluate "$act" > /dev/null
  ws_call Page.captureScreenshot '{"format":"jpeg","quality":80}' > /dev/null
  awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%.1f\n", (b - a) * 1000}'
done | sort -n | awk '{a[NR] = $1} END {
  printf "p50=%s p95=%s p99=%s max=%s\n", a[int(NR * .5 + .5)], a[int(NR * .95 + .5)], a[int(NR * .99 + .5)], a[NR]}'
ws_close
