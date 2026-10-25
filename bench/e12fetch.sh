#!/usr/bin/env bash
# fetch.sh URL SECS OUT: fetch URL in a loop for SECS (or MAXN times).
# One line per fetch: start_epoch time_total bytes http_code curl_rc
url=$1 end=$(( $(date +%s) + $2 )) out=$3 n=0
: > "$out"
while [ "$(date +%s)" -lt "$end" ] && [ "$n" -lt "${MAXN:-1000000}" ]; do
  t=$(date +%s.%N)
  r=$(curl -s -o /dev/null --max-time "${MAXT:-10}" --connect-timeout 3 \
    -w '%{time_total} %{size_download} %{http_code}' "$url")
  rc=$?
  echo "$t $r $rc" >> "$out"
  n=$((n + 1))
  [ "$rc" -ne 0 ] && sleep 0.2
done
