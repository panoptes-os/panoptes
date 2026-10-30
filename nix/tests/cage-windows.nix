# Cage's panoptes_window_v1 end to end, for a Wayland window (foot) and an
# X11 one (xeyes): each is listed with its pid, app_id, title and box;
# focus follows mapping and activate; a title change and a close reach a
# client that was watching before the windows mapped, and a client that
# binds afterwards sees the same state.
{
  runCommand,
  makeFontsConf,
  dejavu_fonts,
  cage,
  foot,
  xeyes,
}:

let
  fonts = makeFontsConf { fontDirectories = [ dejavu_fonts ]; };
in
runCommand "cage-windows"
  {
    nativeBuildInputs = [
      cage
      foot
      xeyes
    ];
  }
  ''
    export XDG_RUNTIME_DIR=$TMPDIR/xdg WLR_BACKENDS=headless WLR_RENDERER=pixman \
      WLR_LIBINPUT_NO_DEVICES=1 FONTCONFIG_FILE=${fonts}
    mkdir -m 700 "$XDG_RUNTIME_DIR"

    fail() {
      echo "FAIL: $*" >&2
      for f in watch.log cage.log foot.log xeyes.log; do
        echo "--- $f" >&2
        cat "$f" >&2 || true
      done
      exit 1
    }
    # within N CMD...: retry CMD for up to N tenths of a second.
    within() {
      n=$1
      shift
      for _ in $(seq "$n"); do
        "$@" && return 0
        sleep 0.1
      done
      return 1
    }
    # listed RE / watched RE: a fresh listing, or the watcher's log so far,
    # has a line matching RE. latest PID RE: the watcher's newest line for
    # PID's window matches RE.
    listed() { panoptes-windows | grep -Eq "$1"; }
    watched() { grep -Eq "$1" watch.log; }
    latest() { grep "pid=$1 " watch.log | tail -n 1 | grep -Eq "$2"; }

    cage -r 1024x768 2> cage.log &
    within 100 test -S "$XDG_RUNTIME_DIR/wayland-0" || fail "no Wayland socket"
    export WAYLAND_DISPLAY=wayland-0

    panoptes-windows watch > watch.log &
    display=$(panoptes-windows | sed -n 's/^xwayland display=//p')
    [ -n "$display" ] || fail "no Xwayland display announced"
    [ "$(panoptes-windows | grep -c '^window')" = 0 ] || fail "windows before any client"

    foot sh -c 'printf "\033]2;agent\007"; exec sleep 600' 2> foot.log &
    foot=$!
    # foot draws its title bar as a subsurface above its main surface, and
    # Cage puts the main surface, not the window geometry, at the origin:
    # the box starts above the screen.
    win="pid=$foot app_id=\"foot\" title=\"agent\" box=0,-[0-9]+,1024x768 focused=1 parent=0"
    within 100 watched "$win" || fail "the watcher never saw foot's window retitled"
    listed "$win" || fail "a fresh listing disagrees on foot's window"

    DISPLAY=$display xeyes 2> xeyes.log &
    xeyes=$!
    win="pid=$xeyes app_id=\"XEyes\" .* box=0,0,1024x768 focused=1 parent=0"
    within 100 watched "$win" || fail "the watcher never saw xeyes' window"
    listed "$win" || fail "a fresh listing disagrees on xeyes' window"
    within 20 latest "$foot" "focused=0" || fail "foot kept focus after xeyes mapped"
    listed "pid=$foot .* focused=0" || fail "a fresh listing disagrees on foot's focus"

    panoptes-windows activate foot || fail "activate foot"
    within 50 latest "$foot" "focused=1" || fail "activate did not focus foot"
    within 20 latest "$xeyes" "focused=0" || fail "xeyes kept focus after activate"
    listed "pid=$xeyes .* focused=0" || fail "a fresh listing disagrees on xeyes' focus"

    id=$(grep -m1 "pid=$xeyes " watch.log | sed 's/^window id=\([0-9]*\) .*/\1/')
    [ -n "$id" ] || fail "no id for xeyes"
    panoptes-windows close XEyes || fail "close XEyes"
    within 50 watched "^closed id=$id\$" || fail "the watcher never saw xeyes' window close"
    listed "pid=$xeyes " && fail "a fresh listing still has xeyes' window"

    touch "$out"
  ''
