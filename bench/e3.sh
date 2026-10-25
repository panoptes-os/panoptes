#!/usr/bin/env bash
# E3: per-instance memory and start latency of agent "container" runtimes.
# Each instance runs the same coreutils sleep from the shared /nix/store, so
# the runtime's own cost is what varies. Everything lands in e3.slice, whose
# memory.current includes kernel memory charged to the instances.
# usage: e3.sh RUNTIME N   RUNTIME: systemd systemd-root nspawn bwrap crun runc docker podman
set -u
B=/var/tmp/glarb-bench E=$B/e3
cd "$B/scripts" && . ./env.sh
rt=$1 n=${2:-50}
S=$(cat "$E/sleep.path") D=$(cat "$E/docker.path")/bin/docker
sl=/sys/fs/cgroup/e3.slice
# sudo resets PATH to secure_path, which has no /nix/store tools.
PODMAN=$(command -v podman) BWRAP=$(command -v bwrap) RT=$(command -v "$rt" 2> /dev/null)
# Rootful podman reads its signature policy from $HOME; the host has none
# in /etc/containers, so it gets one under $E/home.
pod() { sudo env HOME="$E/home" "$PODMAN" --root "$E/podman/root" --runroot "$E/podman/run" --cgroup-manager systemd "$@"; }
start_one() {
  case $rt in
  systemd) sudo systemd-run -q --unit="e3-$1" --slice=e3.slice -p DynamicUser=yes \
      -p PrivateNetwork=yes -p PrivateTmp=yes -p ProtectSystem=strict \
      -p ProtectHome=yes -p PrivateIPC=yes -p PrivateUsers=yes "$S" infinity ;;
  systemd-root) sudo systemd-run -q --unit="e3-$1" --slice=e3.slice -p DynamicUser=yes \
      -p RootDirectory="$E/rootfs" -p BindReadOnlyPaths=/nix/store \
      -p MountAPIVFS=yes -p PrivateNetwork=yes -p PrivateTmp=yes \
      -p PrivateIPC=yes -p PrivateUsers=yes "$S" infinity ;;
  nspawn) sudo systemd-run -q --unit="e3-$1" --slice=e3.slice systemd-nspawn -q \
      --keep-unit --register=no -M "e3-$1" -D "$E/rootfs" --read-only --bind-ro=/nix/store \
      --private-network --as-pid2 "$S" infinity ;;
  bwrap) sudo systemd-run -q --unit="e3-$1" --slice=e3.slice -p User=user \
      "$BWRAP" --ro-bind "$E/rootfs" / --ro-bind /nix/store /nix/store \
      --proc /proc --dev /dev --unshare-all --die-with-parent "$S" infinity ;;
  crun | runc)
    mkdir -p "$E/b/$1"
    jq --arg c "e3.slice:$rt:e3-$1" '.linux.cgroupsPath = $c' "$E/config.tmpl.json" > "$E/b/$1/config.json"
    sudo "$RT" --root "$E/$rt-state" --systemd-cgroup run -d --bundle "$E/b/$1" "e3-$1" > /dev/null ;;
  docker) sudo "$D" -H "unix://$E/docker.sock" run -d --network none \
      --cgroup-parent e3.slice -v /nix/store:/nix/store:ro --name "e3-$1" e3img "$S" infinity > /dev/null ;;
  podman) pod run -d --network none --cgroup-parent e3.slice \
      -v /nix/store:/nix/store:ro --name "e3-$1" localhost/e3img "$S" infinity > /dev/null ;;
  esac
}
stop_all() {
  case $rt in
  crun | runc) for i in $(seq "$n"); do sudo "$RT" --root "$E/$rt-state" delete -f "e3-$i" 2> /dev/null; done ;;
  docker) sudo "$D" -H "unix://$E/docker.sock" rm -f $(seq -f 'e3-%g' "$n") > /dev/null 2>&1 ;;
  podman) pod rm -f -t 0 $(seq -f 'e3-%g' "$n") > /dev/null 2>&1 ;;
  *) for i in $(seq "$n"); do sudo systemctl stop "e3-$i" 2> /dev/null; done
     sudo systemctl reset-failed 'e3-*' 2> /dev/null ;;
  esac
}
snap() { # memory.current of e3.slice and summed PSS of its processes, KiB
  local cur pss
  cur=$(( $(cat "$sl/memory.current") >> 10 ))
  pss=$(sudo find "$sl" -name cgroup.procs -exec cat {} + |
    while read -r p; do sudo awk '/^Pss:/ {print $2}' "/proc/$p/smaps_rollup" 2> /dev/null; done |
    awk '{s += $1} END {print s + 0}')
  echo "$cur $pss"
}
# A failed unit from an earlier run keeps its name taken.
sudo systemctl reset-failed 'e3-*' 2> /dev/null
if [ "$rt" = podman ]; then
  mkdir -p "$E/home/.config/containers"
  echo '{"default": [{"type": "insecureAcceptAnything"}]}' > "$E/home/.config/containers/policy.json"
  pod image exists localhost/e3img || pod import "$E/rootfs.tar" localhost/e3img > /dev/null
fi
read -r c0 p0 < <(snap)
times=()
for i in $(seq "$n"); do
  t0=$EPOCHREALTIME
  start_one "$i"
  times+=("$(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}')")
done
sleep 5
read -r c1 p1 < <(snap)
live=$(sudo find "$sl" -name cgroup.procs -exec cat {} + | wc -l)
stop_all
stats=$(printf '%s\n' "${times[@]}" | sort -n | awk '{a[NR] = $1} END {
  printf "start_ms_p50=%d p90=%d max=%d", a[int(NR * 0.5 + 0.5)], a[int(NR * 0.9 + 0.5)], a[NR]}')
printf '%s\tn=%s\tprocs_in_slice=%s\tper_instance_current_kib=%d per_instance_pss_kib=%d\t%s\tslice_before_kib=%s\n' \
  "$rt" "$n" "$live" $(( (c1 - c0) / n )) $(( (p1 - p0) / n )) "$stats" "$c0" | tee -a "$B/results/e3.tsv"
