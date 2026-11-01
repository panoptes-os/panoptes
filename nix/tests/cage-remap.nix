# An xdg toplevel maps, unmaps by committing a NULL buffer, and maps again,
# as xdg-shell allows; Cage keeps running and exits cleanly on SIGTERM.
# Under cage-asan it also reports no memory error, leak or undefined
# behavior. Cage used to keep a freed scene tree after the unmap and move
# it on the second map (upstream Cage issue 545); only the -asan check sees
# that, because the freed node is read inside wlroots.
{
  name ? "cage-remap",
  runCommand,
  stdenv,
  pkg-config,
  wayland,
  wayland-scanner,
  wayland-protocols,
  cage,
}:

let
  remap = stdenv.mkDerivation {
    name = "remap";
    src = ./cage-remap.c;
    dontUnpack = true;
    nativeBuildInputs = [
      pkg-config
      wayland-scanner
    ];
    buildInputs = [ wayland ];
    buildPhase = ''
      xml=${wayland-protocols}/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml
      wayland-scanner client-header $xml xdg-shell-client-protocol.h
      wayland-scanner private-code $xml xdg-shell-protocol.c
      $CC -Wall -Wextra -Wno-unused-parameter -Werror -I. -o remap $src xdg-shell-protocol.c \
        $(pkg-config --cflags --libs wayland-client)
    '';
    installPhase = "install -Dm755 remap $out/bin/remap";
  };
in
runCommand name
  {
    nativeBuildInputs = [
      cage
      remap
    ];
  }
  ''
    export XDG_RUNTIME_DIR=$TMPDIR/xdg WLR_BACKENDS=headless WLR_RENDERER=pixman \
      WLR_LIBINPUT_NO_DEVICES=1 \
      ASAN_OPTIONS=detect_leaks=1 UBSAN_OPTIONS=print_stacktrace=1
    mkdir -m 700 "$XDG_RUNTIME_DIR"

    fail() {
      echo "FAIL: $*" >&2
      for f in remap.log cage.log; do
        echo "--- $f" >&2
        cat "$f" >&2 || true
      done
      exit 1
    }

    cage -r 1024x768 2> cage.log &
    cage=$!
    for _ in $(seq 100); do
      [ -S "$XDG_RUNTIME_DIR/wayland-0" ] && break
      sleep 0.1
    done
    export WAYLAND_DISPLAY=wayland-0

    timeout 30 remap > remap.log 2>&1 || fail "remap did not map again"
    grep -q '^mapped again$' remap.log || fail "remap did not map again"
    kill -0 "$cage" 2>/dev/null || fail "Cage died while remap mapped again"

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
