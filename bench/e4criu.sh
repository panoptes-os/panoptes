#!/usr/bin/env bash
# E4 CRIU probe: checkpoint one running agent (Cage + Chromium) with CRIU,
# once with Chromium's sandbox and once with --no-sandbox, and restore the
# dump when one is written. Appends one row per try to results/e4.tsv; the
# CRIU logs stay in criu/ID. usage: [SB="on off"] [GHOST=SIZE] e4criu.sh PAGE
#   GHOST raises --ghost-limit, CRIU's cap on an unlinked file it copies
#   into the image; wlroots keeps its frame in an unlinked /dev/shm file.
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./pages.sh
p=${1:-wd} criu=$(command -v criu) ghost=${GHOST:-}
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
for sb in ${SB:-on off}; do
  id=e4x$sb port=9510 d=$B/criu/e4x$sb flags=()
  [ "$sb" = off ] && flags=(--no-sandbox)
  ./start.sh cage "$id" 1280 720 "$port" "${url[$p]}" "${flags[@]}" || continue
  ./waitload.sh "$port" 60 nav
  sleep 10
  pid=$(systemctl show -p MainPID --value "glarb-$id")
  sudo rm -rf "$d" && sudo mkdir -p "$d"
  t0=$EPOCHREALTIME
  sudo "$criu" dump -t "$pid" -D "$d" --tcp-established --ext-unix-sk \
    --file-locks ${ghost:+--ghost-limit "$ghost"} -v2 -o dump.log
  drc=$? td=$(ms "$t0" "$EPOCHREALTIME")
  why=$(sudo grep -m1 -E 'Error|Can.t' "$d/dump.log" | cut -c1-160)
  img=$(sudo du -sm "$d" | cut -f1)
  rrc=- tr=- cdp=-
  if [ "$drc" = 0 ]; then
    sudo systemctl stop "glarb-$id" 2> /dev/null
    t0=$EPOCHREALTIME
    sudo "$criu" restore -D "$d" --tcp-established --ext-unix-sk \
      --file-locks -d -v2 -o restore.log
    rrc=$? tr=$(ms "$t0" "$EPOCHREALTIME")
    cdp=$(curl -s -m 5 "http://127.0.0.1:$port/json/version" | jq -r '.Browser // "none"')
    [ "$rrc" = 0 ] || why=$(sudo grep -m1 -E 'Error|Can.t' "$d/restore.log" | cut -c1-160)
    sudo kill -9 "$pid" 2> /dev/null
    sudo pkill -9 -f -- "--remote-debugging-port=$port" 2> /dev/null
  fi
  printf '%s\tcriu sandbox=%s ghost=%s\tdump_rc=%s dump_ms=%s image_mib=%s restore_rc=%s restore_ms=%s devtools_after=%s\t%s\n' \
    "$p" "$sb" "${ghost:-default}" "$drc" "$td" "$img" "$rrc" "$tr" "$cdp" "${why:-}" | tee -a "$B/results/e4.tsv"
  sudo systemctl stop "glarb-$id" 2> /dev/null
  sudo systemctl reset-failed "glarb-$id" 2> /dev/null
  sleep 3
done
