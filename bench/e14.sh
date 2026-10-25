#!/usr/bin/env bash
# E14: one native GUI app alone in Cage, in a delegated transient unit.
# Measures memory, idle CPU, compositor capture, virtual input, the AT-SPI
# tree, a 20 s freeze, and a park into zswap with writeback off.
# usage: e14.sh APP [MODE] [R]
#   APP: kicad | blender | blender-rot | calc | ltspice   (see e14run.sh)
#   MODE: full (default) | cpu (start, settle, memory and CPU only)
#   R: pixman (default) | gles2
set -u
B=/var/tmp/glarb-bench E=$B/e14
. "$E/env.sh"
app=$1 mode=${2:-full} r=${3:-pixman}
id=e14-$app-$r-$RANDOM
unit=$id.service cg=/sys/fs/cgroup/e14.slice/$unit run=$E/run/$id
img=$E/img/$id
mkdir -p "$img"
res=$E/results/e14.tsv
mkdir -p "${res%/*}"
row="app=$app renderer=$r mode=$mode"
add() { row="$row $*"; echo "  $*" >&2; }
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
usage() { awk '$1 == "usage_usec" {print $2}' "$cg/cpu.stat"; }
cpu_pct() { # CPU of the unit over N seconds, in percent of one core
  local a b
  a=$(usage); sleep "$1"; b=$(usage)
  awk -v a="$a" -v b="$b" -v s="$1" 'BEGIN {printf "%.2f", (b - a) / (s * 1e4)}'
}
shot() { XDG_RUNTIME_DIR=$run WAYLAND_DISPLAY=wayland-0 timeout 10 grim "$1" 2> /dev/null; }
colors() { magick "$1" -format %k info: 2> /dev/null || echo 0; }
diffpx() { magick compare -metric AE "$1" "$2" null: 2>&1 | awk '{printf "%d", $1}'; }
wl() { XDG_RUNTIME_DIR=$run WAYLAND_DISPLAY=wayland-0 timeout 5 wlrctl "$@"; }
procs() { find "$cg" -name cgroup.procs -exec cat {} + 2> /dev/null | wc -l; }
vnc=$((5900 + RANDOM % 1000))
vd() { timeout 10 vncdo -s "127.0.0.1::$vnc" "$@"; }
# Absolute clicks go through wayvnc: wlrctl's relative pointer never reached
# the client in Cage ("virtual pointer cannot be mapped to an output").
# Wine under Xwayland ignored instant clicks, so press and release slowly.
click() { vd move "$1" "$2" pause 0.2 mousedown 1 pause 0.15 mouseup 1; }
act() { # the app's input probe
  case $app in
  # 1 + 2 = on the calculator's own buttons. Typing through wlrctl dropped
  # the first batch of keys, so the probe clicks instead.
  calc) click 366 636 && click 774 636 && click 502 636 && click 910 660 ;;
  # LTspice opens its release notes on top, which blocks the main window's
  # menu and ignores Escape; scroll the notes instead.
  ltspice) click 640 300 && vd key pgdn ;;
  *) click "$CX" "$CY" ;;
  esac
}
unact() { # undo the probe so the next one starts from the same screen
  case $app in
  calc) click 366 492 ;;
  ltspice) vd key pgup ;;
  *) vd key esc ;;
  esac
}
probe() { # act, then report whether the screen changed
  local a=$img/$1-a.png b=$img/$1-b.png
  shot "$a"; act; sleep 0.7; shot "$b"; unact; sleep 0.7
  echo "$(diffpx "$a" "$b")"
}
first_shot_after() { # ms from $1 until a capture with content succeeds, saved to $2
  local t0=$1 f=$2 t=-
  for _ in $(seq 400); do
    if shot "$f"; then
      t=$(ms "$t0" "$EPOCHREALTIME") # stop the clock before checking the image
      [ "$(colors "$f")" -gt 20 ] && break
    fi
    sleep 0.025
  done
  echo "$t"
}
mem() { sudo "$B/scripts/mem.sh" "$cg"; }

# Click target per app: a menu in its top bar (found from the first capture).
case $app in
kicad) CX=${CX:-22} CY=${CY:-46} ;;
blender*) CX=${CX:-40} CY=${CY:-12} ;;
ltspice) CX=${CX:-14} CY=${CY:-9} ;;
*) CX=${CX:-0} CY=${CY:-0} ;;
esac

t0=$EPOCHREALTIME
sudo systemd-run --quiet --unit="$id" --slice=e14.slice -p User=user -p Group=user \
  -p Delegate=yes -p KillMode=control-group --setenv=R="$r" --setenv=VNC="$vnc" \
  "$E/e14app.sh" "$id" "$app"
up=-
deadline=$((SECONDS + 120))
while [ $SECONDS -lt $deadline ]; do
  if shot "$img/start.png" && [ "$(colors "$img/start.png")" -gt 20 ]; then
    up=$(ms "$t0" "$EPOCHREALTIME"); break
  fi
  systemctl is-active --quiet "$unit" || break
  sleep 0.1
done
add "first_frame_ms=$up"
[ "$up" = - ] && { add "error=no-frame"; printf '%s\n' "$row" >> "$res"; sudo systemctl stop "$unit"; exit 1; }
# Without GL, eeschema opens behind a "falling back to software rendering"
# dialog; accept it so the editor itself is what gets measured.
[ "$app" = kicad-nogl ] && { sleep 3; click 268 152; vd key enter; sleep 1; shot "$img/accepted.png"; }
sleep 20
shot "$img/settled.png"
add "settled[$(mem)]"
add "idle_cpu_pct=$(cpu_pct 30)"
sudo "$B/scripts/mem.sh" "$cg" breakdown | sed 1d | head -8 | tr -s ' ' | sed 's/^ //' |
  awk '{printf "%s%s", (NR > 1 ? ";" : ""), $0} END {print ""}' | tr ' ' '_' | sed 's/^/breakdown=/' |
  { read -r b; add "$b"; }
if [ "$mode" = cpu ]; then
  printf '%s\n' "$row" >> "$res"; sudo systemctl stop "$unit"; exit 0
fi

lat=()
for i in 1 2 3 4 5; do
  a=$EPOCHREALTIME; shot "$img/lat$i.png"; lat+=("$(ms "$a" "$EPOCHREALTIME")")
done
add "grim_ms=$(printf '%s\n' "${lat[@]}" | sort -n | awk '{v[NR]=$1} END {print v[3] "(p50)," v[5] "(max)"}')"
add "capture_colors=$(colors "$img/settled.png")"
add "$(lua "$E/e14atspi.lua" "$(cat "$run/dbus.addr")" 3000)"
add "input_px=$(probe input0)"

# Freeze 20 s, thaw, first capture, input again.
n0=$(procs)
shot "$img/pre-freeze.png"
a=$EPOCHREALTIME
echo 1 | sudo tee "$cg/cgroup.freeze" > /dev/null
until grep -q '^frozen 1' "$cg/cgroup.events"; do sleep 0.001; done
add "freeze_ms=$(ms "$a" "$EPOCHREALTIME")"
sleep 20
a=$EPOCHREALTIME
echo 0 | sudo tee "$cg/cgroup.freeze" > /dev/null
add "thaw_first_shot_ms=$(first_shot_after "$a" "$img/post-freeze.png")"
add "thaw_vs_before_px=$(diffpx "$img/pre-freeze.png" "$img/post-freeze.png")"
add "thaw_input_px=$(probe input1)"
add "procs_freeze=$n0/$(procs)"

# Park: freeze, reclaim into zswap with writeback off, hold 20 s, thaw.
echo 0 | sudo tee "$cg/memory.zswap.writeback" > /dev/null
shot "$img/pre-park.png"
add "before_park[$(mem)]"
echo 1 | sudo tee "$cg/cgroup.freeze" > /dev/null
until grep -q '^frozen 1' "$cg/cgroup.events"; do sleep 0.001; done
a=$EPOCHREALTIME
sudo timeout -s INT 60 sh -c "echo 64G > $cg/memory.reclaim" 2> /dev/null
add "reclaim_ms=$(ms "$a" "$EPOCHREALTIME")"
add "parked[$(mem)]"
sleep 20
add "after_hold[$(mem)]"
a=$EPOCHREALTIME
echo 0 | sudo tee "$cg/cgroup.freeze" > /dev/null
add "park_first_shot_ms=$(first_shot_after "$a" "$img/post-park.png")"
add "park_vs_before_px=$(diffpx "$img/pre-park.png" "$img/post-park.png")"
add "park_input_px=$(probe input2)"
sleep 5
add "resumed[$(mem)]"
add "procs_park=$n0/$(procs) active=$(systemctl is-active "$unit")"
add "zs=$(cat /sys/module/zswap/parameters/zpool)/$(cat /sys/module/zswap/parameters/compressor)"
printf '%s\n' "$row" >> "$res"
sudo systemctl stop "$unit"
