#!/usr/bin/env bash
# E3 setup: an empty rootfs whose only content is the host's /nix/store
# (bind-mounted read-only at run time), an OCI bundle template, a temporary
# dockerd in e3.slice, and the rootfs imported as image e3img for docker and
# podman. Every path lives under /var/tmp/glarb-bench/e3.
set -eu
B=/var/tmp/glarb-bench E=$B/e3 P=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
cd "$B/scripts" && . ./env.sh
mkdir -p "$E"/rootfs/{nix/store,proc,dev,tmp,sys,etc,run} "$E"/podman
: > "$E/rootfs/etc/os-release"
echo "$(nix build --no-link --print-out-paths "$P#coreutils")/bin/sleep" > "$E/sleep.path"
docker_pkg=$(nix build --no-link --print-out-paths "$P#docker")
echo "$docker_pkg" > "$E/docker.path"
tar -C "$E/rootfs" -cf "$E/rootfs.tar" .
cd "$E" && rm -f config.json && crun spec && cd - > /dev/null
jq --arg r "$E/rootfs" --arg s "$(cat "$E/sleep.path")" '
  .root = {path: $r, readonly: true}
  | .process.terminal = false
  | .process.args = [$s, "infinity"]
  | .mounts += [{destination: "/nix/store", type: "bind", source: "/nix/store",
                 options: ["rbind", "ro"]}]' "$E/config.json" > "$E/config.tmpl.json"
sudo systemd-run -q --unit=e3-dockerd --slice=e3.slice \
  "$docker_pkg/bin/dockerd" --data-root "$E/docker" --exec-root "$E/dx" \
  -H "unix://$E/docker.sock" --pidfile "$E/docker.pid" \
  --iptables=false --ip6tables=false --bridge=none --ip-forward=false \
  --exec-opt native.cgroupdriver=systemd --cgroup-parent=e3.slice
for _ in $(seq 100); do sudo test -S "$E/docker.sock" && break; sleep 0.2; done
sudo "$docker_pkg/bin/docker" -H "unix://$E/docker.sock" import "$E/rootfs.tar" e3img
sudo "$(command -v podman)" --root "$E/podman/root" --runroot "$E/podman/run" \
  --cgroup-manager systemd import "$E/rootfs.tar" e3img
echo e3setup-ok
