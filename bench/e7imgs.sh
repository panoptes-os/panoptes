#!/usr/bin/env bash
# E7 inputs, step 2: COUNT distinct 1280x720 JPEGs built from the viewport
# shots of e7shots.sh. Each is padded to the full 720p output and stamped
# with its index, so every (run, agent, step) can send an image no other
# request has sent. That keeps cache hits to what a real harness would get.
# usage: e7imgs.sh COUNT
set -eu
B=/var/tmp/glarb-bench P=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
n=${1:-200} src=$B/results/shots out=$B/results/e7imgs
mkdir -p "$out" && rm -f "$out"/*.jpg
mapfile -t base < <(ls "$src"/*.jpg | sort -V)
magick=$(nix build --no-link --print-out-paths "$P#imagemagick")/bin/magick
for k in $(seq 0 $((n - 1))); do
  "$magick" "${base[$((k % ${#base[@]}))]}" -background white -gravity north \
    -extent 1280x720 -gravity southeast -fill black -pointsize 20 \
    -annotate +10+10 "img $k" -quality 80 "$(printf '%s/img-%03d.jpg' "$out" "$k")"
done
echo "e7imgs: $(ls "$out" | wc -l) images, $(md5sum "$out"/*.jpg | cut -d' ' -f1 | sort -u | wc -l) unique"
