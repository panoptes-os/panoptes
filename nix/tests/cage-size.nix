# The forked Cage runs with no client, its headless output at the size -r
# asks for, and a screenshot comes back at that size. A malformed size is
# refused before anything starts.
{
  runCommand,
  cage,
  file,
  grim,
}:

runCommand "cage-size"
  {
    nativeBuildInputs = [
      cage
      file
      grim
    ];
  }
  ''
    export XDG_RUNTIME_DIR=$TMPDIR/xdg WLR_BACKENDS=headless WLR_RENDERER=pixman \
      WLR_LIBINPUT_NO_DEVICES=1
    mkdir -m 700 "$XDG_RUNTIME_DIR"

    cage -r 1024x768 2> cage.log &
    pid=$!
    for _ in $(seq 100); do
      [ -S "$XDG_RUNTIME_DIR/wayland-0" ] && break
      sleep 0.1
    done
    WAYLAND_DISPLAY=wayland-0 grim shot.png || { cat cage.log; exit 1; }
    kill "$pid"
    wait "$pid" || true
    file shot.png | grep -q '1024 x 768' || { file shot.png; exit 1; }

    if cage -r 1024 2> bad.log; then
      echo "cage accepted -r 1024" >&2
      exit 1
    fi
    grep -q 'expected WxH' bad.log || { cat bad.log; exit 1; }

    touch "$out"
  ''
