#!/usr/bin/env bash
# park.sh N [KILL]: N desks under panoptes-resd log into mock supplier
# portals, go idle, freeze, and park; then every agent resumes with a
# screenshot and continues its flow (open its invoice list, download a PDF).
# With KILL=1, panoptes-resd is killed with SIGKILL once some agents have
# parked and is restarted; the run then checks that no kiosk was thawed.
# Needs jq on PATH. Results: panoptes-run/park/{agents.tsv,summary.txt}.
set -uo pipefail
N=${1:?usage: park.sh N [KILL]} KILL=${2:-0}
B=/var/tmp/glarb-bench P=$B/panoptes/target/release R=$B/panoptes-run/park
CG=/sys/fs/cgroup/panoptes.slice CREDS=$B/demo/data/creds.tsv PORTAL=http://127.0.0.1:8090
SOCK=/run/panoptes/resd.sock PARK_AFTER=${PARK_AFTER:-10000}
CTL=$P/panoptesctl
mkdir -p "$R"

resd_start() {
  sudo systemd-run --quiet --collect --unit=panoptes-resd -p StandardError=append:$R/resd.log \
    $P/panoptes-resd --park-after-ms "$PARK_AFTER" --tick-ms 500
  for _ in $(seq 50); do sudo test -S $SOCK && break; sleep 0.1; done
}
status() { sudo $CTL resd $SOCK '{"op":"status"}'; }
kcg() { echo "$CG/pdesk-park-$1.service/kiosk"; }
desk() { a=${3:-}; [ -n "$a" ] || a='{}'; $CTL desk "$R/$1/desk.sock" "$2" "$a"; }
mem_avail() { awk '/MemAvailable/ {print $2 * 1024}' /proc/meminfo; }
now_ms() { date +%s%3N; }

cleanup() {
  for i in $(seq "$N"); do sudo systemctl stop "pdesk-park-$i" 2> /dev/null; done
  sudo systemctl stop panoptes-resd 2> /dev/null
}
trap cleanup EXIT

: > $R/resd.log
sudo systemctl stop panoptes-resd 2> /dev/null; sudo rm -f /run/panoptes/resd.state
resd_start
avail0=$(mem_avail)

echo "== start $N desks"
for i in $(seq "$N"); do
  sudo systemctl stop "pdesk-park-$i" 2> /dev/null
  rm -rf "${R:?}/$i"; mkdir -p "$R/$i"
  sudo systemd-run --quiet --collect --unit="pdesk-park-$i" --slice=panoptes.slice \
    -p User=user -p Group=user -p Delegate=yes \
    -p StandardOutput=file:"$R/$i/desk.log" -p StandardError=file:"$R/$i/desk.log" \
    sh -c ". $B/scripts/env.sh; exec $P/panoptes-desk --run-dir $R/$i --size 1280x800 --resd $SOCK"
done
for i in $(seq "$N"); do
  for _ in $(seq 300); do [ -S "$R/$i/desk.sock" ] && break; sleep 0.1; done
done

# login I: the portal for agent I, by the first textbox, the password
# textbox, and the first button of its sign-in form.
login() {
  i=$1
  slug=$(awk -F'\t' -v n="$i" 'NR > 1 { s[++c] = $1 } END { print s[(n - 1) % c + 1] }' $CREDS)
  user=$(awk -F'\t' -v s="$slug" '$1 == s { print $4 }' $CREDS)
  pass=$(awk -F'\t' -v s="$slug" '$1 == s { print $5 }' $CREDS)
  desk "$i" navigate "$(jq -nc --arg u "$PORTAL/$slug/login" '{url:$u}')" > /dev/null
  t=$(desk "$i" ax_tree | jq -r .result.tree)
  id() { printf '%s\n' "$t" | grep -m1 -E "$1" | sed -n 's/^ *\[\([0-9]*\)\].*/\1/p'; }
  u=$(printf '%s\n' "$t" | grep -E 'textbox' | grep -v -i 'password' | head -1 | sed -n 's/^ *\[\([0-9]*\)\].*/\1/p')
  p=$(id 'textbox "[^"]*[Pp]assword')
  b=$(id 'button ')
  desk "$i" type "$(jq -nc --argjson n "$u" --arg t "$user" '{node_id:$n,text:$t}')" > /dev/null
  desk "$i" type "$(jq -nc --argjson n "$p" --arg t "$pass" '{node_id:$n,text:$t}')" > /dev/null
  desk "$i" click "{\"node_id\":$b}" > /dev/null
  desk "$i" wait_settled '{"timeout_ms":8000}' > /dev/null
  t=$(desk "$i" ax_tree | jq -r .result.tree)
  if printf '%s\n' "$t" | grep -q -i 'password'; then echo "$i $slug login-failed"; else echo "$i $slug ok"; fi
}
export -f login desk; export R CTL CREDS PORTAL
echo "== log in"
seq "$N" | xargs -P 10 -I{} bash -c 'login {}' > $R/login.txt
grep -c ' ok$' $R/login.txt | sed 's/^/logged in: /'

# Each desk released its kiosk 100 ms after its last call: all should be
# waiting (frozen). Record the memory they hold before parking.
sleep 1
for i in $(seq "$N"); do
  printf '%s\t%s\t%s\n' "$i" "$(cat "$(kcg "$i")/memory.current")" \
    "$(awk '/^frozen/ {print $2}' "$(kcg "$i")/cgroup.events")"
done > $R/before.tsv
awk '{ s += $2; f += $3 } END { printf "waiting: %d of %d frozen, %.1f MiB per kiosk\n", f, NR, s / NR / 1048576 }' $R/before.tsv
avail1=$(mem_avail)

if [ "$KILL" = 1 ]; then
  # Kill resd once the first parks have happened, then restart it.
  for _ in $(seq 600); do
    n=$(grep -c 'parked pdesk' $R/resd.log); [ "$n" -ge $((N / 4)) ] && break; sleep 0.05
  done
  echo "== SIGKILL resd after $(grep -c 'parked pdesk' $R/resd.log) parks"
  sudo systemctl kill -s KILL panoptes-resd; sleep 0.5
  thawed=0
  for i in $(seq "$N"); do
    [ "$(awk '/^frozen/ {print $2}' "$(kcg "$i")/cgroup.events")" = 1 ] || thawed=$((thawed + 1))
  done
  echo "while dead: $thawed thawed"
  sudo systemctl reset-failed panoptes-resd 2> /dev/null; sudo rm -f $SOCK
  resd_start
  grep 'recovered' $R/resd.log | tail -1
fi

echo "== wait for parks"
t0=$(now_ms)
for _ in $(seq 240); do
  parked=$(status | jq '[.agents[] | select(.state == "parked")] | length')
  [ "$parked" -ge "$N" ] && break; sleep 0.5
done
echo "parked: $parked of $N after $(( $(now_ms) - t0 )) ms"
sleep 2
status > $R/status-parked.json
thawed=0
for i in $(seq "$N"); do
  [ "$(awk '/^frozen/ {print $2}' "$(kcg "$i")/cgroup.events")" = 1 ] || thawed=$((thawed + 1))
done
echo "frozen check after parks: $thawed thawed"
for i in $(seq "$N"); do cat "$(kcg "$i")/memory.current"; done > $R/after.txt
avail2=$(mem_avail)
pool=$(sudo cat /sys/kernel/debug/zswap/pool_total_size 2> /dev/null || echo NA)

echo "== resume: screenshot, then continue the flow"
resume() {
  i=$1
  r=$(desk "$i" screenshot '{"scale":0.5}')
  shot=$(printf '%s' "$r" | jq -c '[.ok, .timing.wake_ms, .timing.capture_ms, .timing.total_ms]')
  slug=$(awk -v n="$i" '$1 == n {print $2}' $R/login.txt)
  desk "$i" navigate "$(jq -nc --arg u "$PORTAL/$slug/invoices" '{url:$u}')" > /dev/null
  t=$(desk "$i" ax_tree | jq -r .result.tree)
  if printf '%s\n' "$t" | grep -q -i 'password'; then echo "$i $shot session-lost"; return; fi
  # The PDF download needs the session cookie from before the park.
  inv=$(jq -r --arg s "$slug" 'select(.slug == $s) | .file' $B/demo/data/truth.jsonl | head -1)
  desk "$i" navigate "$(jq -nc --arg u "$PORTAL/$slug/download/$inv.pdf" '{url:$u}')" > /dev/null 2>&1
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    d=$(desk "$i" download_list | jq '[.result.files[]? | select(.complete == true and .bytes > 1000)] | length')
    [ "$d" -ge 1 ] && break; sleep 0.3
  done
  echo "$i $shot downloads=$d"
}
export -f resume; export PORTAL B
seq "$N" | xargs -P 10 -I{} bash -c 'resume {}' > $R/resume.txt
status > $R/status-resumed.json

paste $R/before.tsv $R/after.txt > $R/agents.tsv
{
  echo "agents $N kill=$KILL park_after_ms=$PARK_AFTER"
  awk '{ b += $2; a += $4 } END { printf "kiosk memory.current per agent: %.1f MiB waiting -> %.1f MiB parked\n", b / NR / 1048576, a / NR / 1048576 }' $R/agents.tsv
  echo "MemAvailable: start $((avail0 >> 20)) MiB, all waiting $((avail1 >> 20)), all parked $((avail2 >> 20)) MiB; zswap pool $pool"
  jq -r '[.agents[].last_park | select(.) | .ms] | "park reclaim ms: min \(min) max \(max) mean \(add / length | floor)"' $R/status-parked.json
  awk '{ gsub(/[][]/, "", $2); split($2, f, ","); if (f[1] == "true") { ok++; w += f[2]; t += f[4]; if (f[2] > wm) wm = f[2]; if (f[4] > tm) tm = f[4] } }
       / downloads=[1-9]/ { d++ } /session-lost/ { s++ }
       END { printf "resume: %d ok screenshots; wake mean %.1f max %.1f ms; screenshot total mean %.1f max %.1f ms; %d continued to a download; %d lost their session\n", ok, w / ok, wm, t / ok, tm, d, s }' $R/resume.txt
} | tee $R/summary.txt
