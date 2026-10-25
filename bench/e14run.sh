#!/usr/bin/env bash
# Cage's client: size the headless output, then exec the app.
# usage: e14run.sh APP
set -u
E=/var/tmp/glarb-bench/e14
app=$1
wlr-randr --output HEADLESS-1 --custom-mode 1280x720 2>> "$XDG_RUNTIME_DIR/randr.log"
echo "DISPLAY=${DISPLAY:-}" > "$XDG_RUNTIME_DIR/display"
# wayvnc injects input as an absolute virtual pointer; wlrctl's relative
# pointer does not reach clients in Cage.
[ -n "${VNC:-}" ] && wayvnc 127.0.0.1 "$VNC" 2>> "$XDG_RUNTIME_DIR/wayvnc.log" &
# Rotate the 3D view a little each frame, through Blender's own API.
rot='import bpy, math
def spin():
    for a in bpy.context.screen.areas:
        if a.type == "VIEW_3D":
            r = a.spaces[0].region_3d
            r.view_rotation.rotate(__import__("mathutils").Euler((0, 0, math.radians(2))))
            a.tag_redraw()
    return 1 / 60
bpy.app.timers.register(spin)'
gl() { # Nix GL apps on a non-NixOS host find no drivers; point them at Mesa
  local m
  m=$(cat "$E/mesa.path")
  export __EGL_VENDOR_LIBRARY_FILENAMES=$m/share/glvnd/egl_vendor.d/50_mesa.json
  export LIBGL_DRIVERS_PATH=$m/lib/dri LD_LIBRARY_PATH=$m/lib GALLIUM_DRIVER=llvmpipe
}
case $app in blender* | kicad) gl ;; esac
case $app in
kicad | kicad-nogl) exec eeschema "$E/proj/demo.kicad_sch" ;;
blender) exec blender --factory-startup ;;
blender-rot) exec blender --factory-startup --python-expr "$rot" ;;
calc) exec gnome-calculator ;;
ltspice)
  export WINEPREFIX=$E/wineprefix WINEDEBUG=-all
  exec wine "$WINEPREFIX/drive_c/Program Files/ADI/LTspice/LTspice.exe" ;;
*) echo "e14run: unknown app $app" >&2; exit 2 ;;
esac
