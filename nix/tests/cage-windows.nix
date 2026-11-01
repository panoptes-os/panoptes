# Cage's panoptes_window_v1 end to end, for a Wayland window (foot) and an
# X11 one (xeyes): each is listed with its pid, app_id, title and box;
# focus follows mapping and activate; a title change and a close reach a
# client that was watching before the windows mapped, and a client that
# binds afterwards sees the same state. A Tk dialog gets the size it asks
# for, centered, before and after it maps; its main window keeps the
# screen. Cage then exits cleanly on SIGTERM; built with sanitizers, neither
# it nor panoptes-windows reports a memory error, leak or undefined behavior.
{
  name ? "cage-windows",
  runCommand,
  makeFontsConf,
  dejavu_fonts,
  cage,
  foot,
  xeyes,
  tk,
}:

let
  fonts = makeFontsConf { fontDirectories = [ dejavu_fonts ]; };
in
runCommand name
  {
    nativeBuildInputs = [
      cage
      foot
      xeyes
      tk
    ];
  }
  ''
    export XDG_RUNTIME_DIR=$TMPDIR/xdg WLR_BACKENDS=headless WLR_RENDERER=pixman \
      WLR_LIBINPUT_NO_DEVICES=1 FONTCONFIG_FILE=${fonts} \
      ASAN_OPTIONS=detect_leaks=1 UBSAN_OPTIONS=print_stacktrace=1
    mkdir -m 700 "$XDG_RUNTIME_DIR"

    fail() {
      echo "FAIL: $*" >&2
      for f in watch.log windows.log cage.log foot.log xeyes.log wish.log; do
        echo "--- $f" >&2
        cat "$f" >&2 || true
      done
      exit 1
    }
    # Every call's stderr goes to a log the final check scans. UBSan prints
    # its report and carries on, so the exit status does not show it.
    panoptes-windows() { command panoptes-windows "$@" 2>> windows.log; }
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
    # has a line matching RE. latest KEY RE: the watcher's newest line
    # containing KEY matches RE.
    listed() { panoptes-windows | grep -Eq "$1"; }
    watched() { grep -Eq "$1" watch.log; }
    latest() { grep -F "$1" watch.log | tail -n 1 | grep -Eq "$2"; }

    cage -r 1024x768 2> cage.log &
    cage=$!
    within 100 test -S "$XDG_RUNTIME_DIR/wayland-0" || fail "no Wayland socket"
    export WAYLAND_DISPLAY=wayland-0

    panoptes-windows watch > watch.log &
    watcher=$!
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
    within 20 latest "pid=$foot " "focused=0" || fail "foot kept focus after xeyes mapped"
    listed "pid=$foot .* focused=0" || fail "a fresh listing disagrees on foot's focus"

    panoptes-windows activate foot || fail "activate foot"
    within 50 latest "pid=$foot " "focused=1" || fail "activate did not focus foot"
    within 20 latest "pid=$xeyes " "focused=0" || fail "xeyes kept focus after activate"
    listed "pid=$xeyes .* focused=0" || fail "a fresh listing disagrees on xeyes' focus"

    id=$(grep -m1 "pid=$xeyes " watch.log | sed 's/^window id=\([0-9]*\) .*/\1/')
    [ -n "$id" ] || fail "no id for xeyes"
    panoptes-windows close XEyes || fail "close XEyes"
    within 50 watched "^closed id=$id\$" || fail "the watcher never saw xeyes' window close"
    listed "pid=$xeyes " && fail "a fresh listing still has xeyes' window"

    # Tk, a line at a time. It creates each window at 1x1, then asks for
    # its size.
    mkfifo tk.in
    DISPLAY=$display wish < tk.in 2> wish.log &
    wish=$!
    exec 3> tk.in
    echo 'wm title . main; update; toplevel .dlg; wm title .dlg dialog; wm geometry .dlg 300x200; wm transient .dlg .' >&3
    within 100 watched "pid=$wish .* title=\"main\" box=0,0,1024x768 " || fail "the watcher never saw Tk's main window"
    main=$(grep -m1 'title="main"' watch.log | sed 's/^window id=\([0-9]*\) .*/\1/')
    win="pid=$wish .* title=\"dialog\" box=362,284,300x200 focused=1 parent=$main\$"
    within 100 watched "$win" || fail "the dialog did not map centered at 300x200"
    # Each client numbers the windows itself.
    panoptes-windows > fresh
    id=$(grep -m1 'title="main"' fresh | sed 's/^window id=\([0-9]*\) .*/\1/')
    grep -Eq "title=\"dialog\" box=362,284,300x200 focused=1 parent=$id\$" fresh ||
      fail "a fresh listing disagrees on the dialog"

    echo 'wm geometry . 200x100; wm geometry .dlg 400x300' >&3
    within 100 latest 'title="dialog"' "box=312,234,400x300 " ||
      fail "the dialog did not take 400x300, centered"
    latest 'title="main"' "box=0,0,1024x768 " || fail "the main window left the screen's size"

    # Cage shuts down on SIGTERM. The watcher ends when Cage's socket closes,
    # with status 0. Under ASan, each one's exit runs the leak check.
    exec 3>&-
    kill "$foot"
    kill -TERM "$cage"
    (sleep 30 && kill -KILL "$cage" "$watcher") &
    watchdog=$!
    wait "$cage" && status=0 || status=$?
    wait "$watcher" && watcher_status=0 || watcher_status=$?
    kill "$watchdog" 2>/dev/null || true
    [ "$status" = 0 ] || fail "Cage exited with status $status after SIGTERM"
    [ "$watcher_status" = 0 ] || fail "the watcher exited with status $watcher_status"
    if grep -Eq 'ERROR: (Address|Leak)Sanitizer|runtime error:' cage.log windows.log; then
      fail "a sanitizer reported an error"
    fi

    touch "$out"
  ''
