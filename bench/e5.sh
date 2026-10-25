#!/usr/bin/env bash
# E5: latency of one acting agent while K other agents burn CPU, under
# different cgroup policies. All agents are siblings in glarb.slice.
# usage: e5.sh K
set -u
B=/var/tmp/glarb-bench
cd "$B/scripts" && . ./env.sh
k=${1:-64} port=9600 id=e5fg
sl=/sys/fs/cgroup/glarb.slice
page='data:text/html,<body style="font:32px sans-serif"><button id=b onclick="this.textContent=++n">0</button><script>n=0</script>'
stress=$(command -v stress-ng)
./start.sh cage "$id" 1280 720 "$port" "$page" || exit 1
./waitload.sh "$port" 30; sleep 5
fg=$sl/glarb-$id.service
client() { # WEIGHT -> runs the client in its own scope beside the agents
  sudo systemd-run -q --scope --slice=glarb.slice -p CPUWeight="$1" \
    --uid=user --gid=user ./e5client.sh "$port" 100
}
bg_start() { for i in $(seq "$k"); do
  sudo systemd-run -q --unit="e5bg-$i" --slice=glarb.slice -p WorkingDirectory=/tmp "$stress" --cpu 1 --cpu-method matrixprod --quiet
done; sleep 3; }
bg_each() { for i in $(seq "$k"); do echo "$2" | sudo tee "$sl/e5bg-$i.service/$1" > /dev/null; done; }
row() { printf '%s\tk=%s\t%s\n' "$1" "$k" "$2" | tee -a "$B/results/e5.tsv"; }
row idle "$(client 100)"
bg_start
row "contended default weights" "$(client 100)"
echo 10000 | sudo tee "$fg/cpu.weight" > /dev/null
row "fg cpu.weight=10000" "$(client 10000)"
echo 100 | sudo tee "$fg/cpu.weight" > /dev/null
bg_each cpu.idle 1
row "bg cpu.idle=1" "$(client 100)"
bg_each cpu.idle 0
bg_each cpu.max "50000 100000"
row "bg cpu.max=0.5cpu" "$(client 100)"
bg_each cpu.max "max 100000"
bg_each cgroup.freeze 1
row "bg frozen" "$(client 100)"
# Thaw first: systemd never froze these units, so a stop would wait out
# TimeoutStopSec on each one before it sent SIGKILL.
bg_each cgroup.freeze 0
for i in $(seq "$k"); do sudo systemctl stop "e5bg-$i"; done
sudo systemctl stop "glarb-$id"
