#!/usr/bin/env bash
# E2: what screen resolution costs one Cage + Chromium agent: memory,
# capture latency and size (wlr-screencopy via grim, and CDP), and the size
# of text observations (accessibility tree, innerText).
# usage: e2.sh "RESOLUTIONS" "PAGES"
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh && . ./ws.sh && . ./pages.sh
res=${1:-"1024x768 1280x720 1280x800 1440x900 1920x1080"}
pages=${2:-"gh wd wiki"}
out=$B/results/e2.tsv obs=$B/results/obs
mkdir -p "$obs"
median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
grim_ms() { # median of 10 runs, ms
  hyperfine -N -w 2 -r 10 --export-json "$obs/h.json" "$1" > /dev/null 2>&1
  jq '.results[0].summary.time_wall_clock.median * 1000 | round' "$obs/h.json"
}
cdp_shot() { # FORMAT_PARAMS FILE -> median ms of 10 captures; writes FILE
  local i t0 r ts=()
  for i in $(seq 10); do
    t0=$EPOCHREALTIME
    r=$(ws_call Page.captureScreenshot "$1")
    ts+=("$(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}')")
  done
  jq -r '.result.data' <<< "$r" | base64 -d > "$2"
  printf '%s\n' "${ts[@]}" | median
}
k=0
for r in $res; do
  w=${r%x*} h=${r#*x}
  for p in $pages; do
    k=$((k + 1)) id=e2n$k port=9400
    ./start.sh cage "$id" "$w" "$h" "$port" "${url[$p]}" || continue
    ./waitload.sh "$port" 120 nav || echo "e2: $r $p load timeout" >&2
    sleep 15
    cg=/sys/fs/cgroup/glarb.slice/glarb-$id.service
    m=$(sudo ./mem.sh "$cg")
    export XDG_RUNTIME_DIR=$B/run/$id WAYLAND_DISPLAY=wayland-0
    gp=$(grim_ms "grim -t png $obs/$p-$r.grim.png")
    gj=$(grim_ms "grim -t jpeg -q 80 $obs/$p-$r.grim.jpg")
    dims=$(file -b "$obs/$p-$r.grim.png" | grep -oE '[0-9]+ x [0-9]+' | tr -d ' ')
    ws_open "$(ws_page "$port")"
    # Client overhead floor: a no-op call through the same path.
    noop=$(for i in $(seq 10); do t0=$EPOCHREALTIME
      ws_call Runtime.evaluate '{"expression":"1"}' > /dev/null
      awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d\n", (b - a) * 1000}'
    done | median)
    cp=$(cdp_shot '{"format":"png"}' "$obs/$p-$r.cdp.png")
    cj=$(cdp_shot '{"format":"jpeg","quality":80}' "$obs/$p-$r.cdp.jpg")
    cw=$(cdp_shot '{"format":"webp","quality":80}' "$obs/$p-$r.cdp.webp")
    ws_call Accessibility.getFullAXTree > "$obs/$p-$r.ax.json"
    jq -r '.result.nodes[] | select(.ignored | not)
      | [.role.value, (.name.value // "")] | join(" ")' \
      "$obs/$p-$r.ax.json" > "$obs/$p-$r.ax.txt"
    ws_call Runtime.evaluate \
      '{"expression":"document.body.innerText","returnByValue":true}' |
      jq -r '.result.result.value' > "$obs/$p-$r.text.txt"
    ws_close
    sz() { stat -c %s "$obs/$p-$r.$1"; }
    printf '%s\t%s\t%s\t%s\tgrim_png_ms=%s grim_png_b=%s grim_jpg_ms=%s grim_jpg_b=%s cdp_png_ms=%s cdp_png_b=%s cdp_jpg_ms=%s cdp_jpg_b=%s cdp_webp_ms=%s cdp_webp_b=%s ax_nodes=%s ax_txt_b=%s text_b=%s cdp_noop_ms=%s\n' \
      "$r" "$dims" "$p" "$m" "$gp" "$(sz grim.png)" "$gj" "$(sz grim.jpg)" \
      "$cp" "$(sz cdp.png)" "$cj" "$(sz cdp.jpg)" "$cw" "$(sz cdp.webp)" \
      "$(wc -l < "$obs/$p-$r.ax.txt")" "$(sz ax.txt)" "$(sz text.txt)" "$noop" |
      tee -a "$out"
    sudo systemctl stop "glarb-$id"
    sleep 3
  done
done
