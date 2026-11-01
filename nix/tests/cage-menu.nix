# X11 override-redirect windows are drawn where X has them: a red Tk menu
# posted at 600,500, and a blue override-redirect toplevel that maps at
# 100,100 and then moves to 300,100. Cage then exits cleanly on SIGTERM;
# under cage-asan it also reports no memory error, leak or undefined
# behavior. Cage used to leave every override-redirect window at the
# origin and ignore its moves (upstream Cage issue 546).
{
  name ? "cage-menu",
  runCommand,
  makeFontsConf,
  dejavu_fonts,
  cage,
  grim,
  tk,
}:

let
  fonts = makeFontsConf { fontDirectories = [ dejavu_fonts ]; };
in
runCommand name
  {
    nativeBuildInputs = [
      cage
      grim
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
      for f in cage.log wish.log; do
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
    # shows X Y R G B: a fresh screenshot has color R G B at X,Y.
    shows() {
      grim -t ppm shot.ppm || return 1
      header=$(head -n 3 shot.ppm | wc -c)
      px=$(od -An -tu1 -j $((header + ($2 * 1024 + $1) * 3)) -N3 shot.ppm | tr -s ' ')
      [ "$px" = " $3 $4 $5" ]
    }

    cage -r 1024x768 2> cage.log &
    cage=$!
    within 100 test -S "$XDG_RUNTIME_DIR/wayland-0" || fail "no Wayland socket"
    export WAYLAND_DISPLAY=wayland-0
    display=$(panoptes-windows | sed -n 's/^xwayland display=//p')
    [ -n "$display" ] || fail "no Xwayland display announced"

    # Tk's main window stays withdrawn: Cage stacks views in map order, and
    # Tk may map it after the others, covering them.
    cat > menu.tcl <<'EOF'
    wm withdraw .
    toplevel .t -background blue -width 50 -height 50
    wm overrideredirect .t 1
    wm geometry .t +100+100
    after 1500 { wm geometry .t +300+100 }

    menu .m -tearoff 0 -background red -activebackground red
    .m add command -label hello
    .m add command -label world
    after 1000 { tk_popup .m 600 500 }
    EOF
    DISPLAY=$display wish menu.tcl 2> wish.log &

    within 100 shows 610 505 255 0 0 || fail "the menu posted at 600,500 is not drawn there: 610,505 is$px"
    within 100 shows 325 125 0 0 255 || fail "the window moved to 300,100 is not drawn there: 325,125 is$px"

    kill -TERM "$cage"
    (sleep 30 && kill -KILL "$cage") &
    watchdog=$!
    wait "$cage" && status=0 || status=$?
    kill "$watchdog" 2>/dev/null || true
    [ "$status" = 0 ] || fail "Cage exited with status $status after SIGTERM"
    if grep -Eq 'ERROR: (Address|Leak)Sanitizer|runtime error:' cage.log; then
      fail "a sanitizer reported an error"
    fi

    touch "$out"
  ''
