#!/usr/bin/env bash
# E1: memory of one agent desktop stack, by variant and page, run one at a
# time so no other instance shares its pages. Appends to results/e1.tsv.
# usage: e1.sh REPS "VARIANTS" "PAGES"
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh
reps=${1:-3} variants=${2:-"cage cage-vnc xfce headless"}
pages=${3:-"blank gh wd wiki"}
declare -A url=(
  [blank]=about:blank
  [gh]=https://job-boards.greenhouse.io/anthropic
  [wd]=https://nvidia.wd5.myworkdayjobs.com/NVIDIAExternalCareerSite
  [wiki]=https://en.wikipedia.org/wiki/Linux_kernel
)
out=$B/results/e1.tsv
k=0
for rep in $(seq "$reps"); do
  for v in $variants; do
    for p in $pages; do
      k=$((k + 1)) id=e1n$k port=9300
      ./start.sh "$v" "$id" 1280 720 "$port" "${url[$p]}" || continue
      ./waitload.sh "$port" 60 || echo "e1: $v $p load timeout" >&2
      sleep 20
      m=$(sudo ./mem.sh "/sys/fs/cgroup/glarb.slice/glarb-$id.service")
      printf '%s\t%s\t%s\t%s\n' "$rep" "$v" "$p" "$m" | tee -a "$out"
      sudo ./mem.sh "/sys/fs/cgroup/glarb.slice/glarb-$id.service" breakdown \
        > "$B/logs/e1-$id-$v-$p.breakdown"
      sudo systemctl stop "glarb-$id"
      sleep 3
    done
  done
done
