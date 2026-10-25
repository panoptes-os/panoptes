#!/usr/bin/env bash
# E6: where an agent step's network time goes. TLS setup to model APIs,
# loopback HTTP, and bytes moved by one page load (CDP Network events).
# usage: e6.sh
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./pages.sh
out=$B/results/e6.tsv
med() { sort -n | awk '{a[NR] = $1} END {print a[int((NR + 1) / 2)]}'; }
for h in api.openai.com api.anthropic.com generativelanguage.googleapis.com; do
  for _ in $(seq 10); do
    curl -so /dev/null -w '%{time_connect} %{time_appconnect} %{time_starttransfer}\n' "https://$h/" --max-time 10
  done > "$B/logs/e6-$h.txt"
  printf '%s\ttcp_ms=%s tls_done_ms=%s first_byte_ms=%s\n' "$h" \
    "$(awk '{print $1 * 1000}' "$B/logs/e6-$h.txt" | med)" \
    "$(awk '{print $2 * 1000}' "$B/logs/e6-$h.txt" | med)" \
    "$(awk '{print $3 * 1000}' "$B/logs/e6-$h.txt" | med)" | tee -a "$out"
done
port=9700
./start.sh headless e6n 1280 720 "$port" about:blank || exit 1
lo=$(for _ in $(seq 50); do curl -so /dev/null -w '%{time_total}\n' "http://127.0.0.1:$port/json/version"; done |
  awk '{print $1 * 1000000}' | med)
printf 'loopback_http\tdevtools_json_version_us=%s\n' "$lo" | tee -a "$out"
ws_open "$(ws_page "$port")"
for p in gh wd wiki; do
  ws_call Network.enable > /dev/null
  ws_call Network.setCacheDisabled '{"cacheDisabled":true}' > /dev/null
  WS_ID=$((WS_ID + 1))
  jq -cn --argjson i "$WS_ID" --arg u "${url[$p]}" '{id: $i, method: "Page.navigate", params: {url: $u}}' >&"$WS_OUT"
  t0=$EPOCHREALTIME
  # Sum encodedDataLength until the socket is quiet for 8 s.
  while IFS= read -r -t 8 line <&"$WS_IN"; do
    case $line in *'"method":"Network.loadingFinished"'*) echo "$line" ;; esac
  done | jq -s '[length, (map(.params.encodedDataLength) | add)] | @tsv' -r > "$B/logs/e6-$p.bytes"
  read -r nreq bytes < "$B/logs/e6-$p.bytes"
  printf '%s\trequests=%s bytes=%s\n' "$p" "$nreq" "$bytes" | tee -a "$out"
done
ws_close
sudo systemctl stop glarb-e6n
