#!/usr/bin/env bash
# E12: one shared OpenVPN client, policy routing for N agent netns.
#
#   e12-vpns: OpenVPN server, darkhttpd on 10.66.0.1 (allowed) and
#             10.66.1.1 (denied by the allowlist)
#   e12-rtr:  OpenVPN client (tun0), underlay u0, br0 10.77.0.1/16,
#             table 100 = VPN, table 200 = direct, main default = "internet"
#   e12-aN:   one agent, eth0 on br0; 1-500 route via VPN, 501-700 direct
#
# usage (root): e12.sh all       N=700 by default; tears down on exit
#               e12.sh teardown  clean up after a failed run
set -u
B=/var/tmp/glarb-bench/e12
R=/var/tmp/glarb-bench/results/e12.tsv
N=${N:-700}
RTR=e12-rtr VPNS=e12-vpns
PATH="$(cat "$B/path.txt"):/usr/bin:/bin:/usr/sbin:/sbin"
export PATH
mkdir -p "$B/runs" "$B/www"

log() { printf '%s %s\n' "$(date +%T)" "$*" >&2; }
row() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" | tee -a "$R"; }
now() { date +%s.%N; }
el() { awk -v a="$1" -v b="$(now)" 'BEGIN { printf "%.3f", b - a }'; }
ip_of() {
  if [ "$1" -le 250 ]; then echo "10.77.1.$1"
  elif [ "$1" -le 500 ]; then echo "10.77.2.$(($1 - 250))"
  else echo "10.77.3.$(($1 - 500))"; fi
}
# kB: MemAvailable Slab SUnreclaim Percpu KernelStack
mem() {
  awk '$1 ~ /^(MemAvailable|Slab|SUnreclaim|Percpu|KernelStack):$/ { v[$1] = $2 }
    END { print v["MemAvailable:"], v["Slab:"], v["SUnreclaim:"],
      v["Percpu:"], v["KernelStack:"] }' /proc/meminfo
}
slab() { awk 'NR > 2 { print $1, $3 * $4 }' /proc/slabinfo | sort; }
snap() {
  {
    ip -br link | awk '{ print $1 }' | sort; echo ---
    ip -br addr | sort; echo ---
    ip rule; echo ---
    ip route show table all | sort; echo ---
    ip -6 route show table all | sort; echo ---
    nft list ruleset; echo ---
    ls /run/netns | sort; echo ---
    sysctl net.ipv4.ip_forward net.ipv4.conf.all.rp_filter
  } 2>&1 | sed -E "s/expires [0-9]+sec //; s/counter packets [0-9]+ bytes [0-9]+/counter/"
}
cpu_t() { awk '{ print $14 + $15 }' "/proc/$1/stat"; }
host_cpu() { awk '/^cpu / { print $2+$3+$4+$5+$6+$7+$8, $5+$6 }' /proc/stat; }
# n ok err p50_s p95_s mbps over fetch lines: start time_total bytes code rc
summ() {
  local f; f=$(mktemp)
  cat "$@" > "$f"
  local a p
  a=$(awk '{ n++; if ($5 == 0 && $4 == 200) { ok++; b += $3; e = $1 + $2
      if (e > mx) mx = e } else err++
      if (mn == "" || $1 < mn) mn = $1 }
    END { w = mx - mn; printf "%d %d %d %.1f", n, ok, err,
      (w > 0 ? b * 8 / w / 1e6 : 0) }' "$f")
  p=$(awk '$5 == 0 && $4 == 200 { print $2 }' "$f" | sort -n |
    awk '{ a[NR] = $1 } END { if (!NR) { print "NA NA"; exit }
      i = int(NR * 0.5 + 0.5); j = int(NR * 0.95 + 0.5)
      if (i < 1) i = 1; if (j < 1) j = 1
      printf "%.3f %.3f", a[i], a[j] }')
  rm -f "$f"
  echo "$a $p"
}

teardown() {
  log teardown
  for p in cli vpns httpd; do
    [ -f "$B/$p.pid" ] && kill "$(cat "$B/$p.pid")" 2> /dev/null
    rm -f "$B/$p.pid"
  done
  pkill -f "$B/fetch.sh" 2> /dev/null
  local t0; t0=$(now)
  ls /run/netns | awk '/^e12-/ { print "netns del " $1 }' > "$B/del.batch"
  [ -s "$B/del.batch" ] && ip -force -batch "$B/del.batch"
  row teardown_cmd_s "$(el "$t0")" s
}

base() {
  cd "$B" || exit 1
  for n in server client; do
    [ -f $n.crt ] || openssl req -x509 -newkey ec \
      -pkeyopt ec_paramgen_curve:prime256v1 -keyout $n.key -out $n.crt \
      -nodes -days 30 -subj "/CN=e12-$n" 2> /dev/null
  done
  fp() { openssl x509 -fingerprint -sha256 -noout -in "$1" | cut -d= -f2; }
  cat > server.conf << X
dev tun0
proto udp
local 10.99.0.2
port 1194
float
tls-server
dh none
ifconfig 10.8.0.1 10.8.0.2
cert $B/server.crt
key $B/server.key
peer-fingerprint $(fp client.crt)
keepalive 2 10
disable-dco
verb 3
writepid $B/vpns.pid
X
  cat > client.conf << X
dev tun0
proto udp
remote 10.99.0.2 1194
nobind
tls-client
ifconfig 10.8.0.2 10.8.0.1
cert $B/client.crt
key $B/client.key
peer-fingerprint $(fp server.crt)
keepalive 2 10
disable-dco
verb 3
writepid $B/cli.pid
X
  [ -f www/page.bin ] || head -c 1500000 /dev/urandom > www/page.bin
  echo ok > www/small.txt

  ip netns add $RTR; ip netns add $VPNS
  ip link add u0 netns $RTR type veth peer name u1 netns $VPNS
  ip -n $RTR -batch - << X
link set lo up
addr add 10.99.0.1/30 dev u0
link set u0 up
link add br0 type bridge
addr add 10.77.0.1/16 dev br0
link set br0 up
route add default via 10.99.0.2
route add 10.66.0.0/16 via 10.99.0.2 table 200
route add unreachable default table 100 metric 4000
rule add from 10.77.1.0/24 lookup 100 pref 1000
rule add from 10.77.2.0/24 lookup 100 pref 1000
rule add from 10.77.3.0/24 lookup 200 pref 1000
X
  ip netns exec $RTR sysctl -qw net.ipv4.ip_forward=1
  ip -n $VPNS -batch - << X
link set lo up
addr add 10.99.0.2/30 dev u1
link set u1 up
link add site0 type dummy
addr add 10.66.0.1/24 dev site0
addr add 10.66.1.1/24 dev site0
link set site0 up
X
  ip netns exec $VPNS darkhttpd "$B/www" --addr 0.0.0.0 --port 80 \
    --daemon --pidfile "$B/httpd.pid" --no-listing > /dev/null
  ip netns exec $VPNS openvpn --config "$B/server.conf" --daemon \
    --log "$B/vpns.log"
  sleep 0.5
  vpn_up > /dev/null
}

# Start the client; print seconds to "Initialization Sequence Completed"
# and to the restored route.
vpn_up() {
  local t0; t0=$(now)
  ip netns exec $RTR openvpn --config "$B/client.conf" --daemon \
    --log "$B/cli.log"
  local k
  for k in $(seq 1 600); do
    grep -q 'Initialization Sequence Completed' "$B/cli.log" 2> /dev/null &&
      break
    sleep 0.05
  done
  local ti; ti=$(el "$t0")
  ip -n $RTR route replace 10.66.0.0/16 dev tun0 table 100
  echo "$ti $(el "$t0")"
}

agents() {
  local t0 i
  t0=$(now)
  for i in $(seq 1 "$N"); do echo "netns add e12-a$i"; done > "$B/ns.batch"
  ip -batch "$B/ns.batch"
  row setup_netns_s "$(el "$t0")" s
  t0=$(now)
  for i in $(seq 1 "$N"); do
    echo "link add vb$i type veth peer name eth0 netns e12-a$i"
    echo "link set vb$i master br0 up"
  done > "$B/veth.batch"
  ip -n $RTR -batch "$B/veth.batch"
  row setup_veth_s "$(el "$t0")" s
  t0=$(now)
  for i in $(seq 1 "$N"); do
    printf 'link set lo up\naddr add %s/16 dev eth0\nlink set eth0 up\nroute add default via 10.77.0.1\n' \
      "$(ip_of "$i")" | ip -n "e12-a$i" -batch -
  done
  row setup_addr_s "$(el "$t0")" s
}

gen_nft() {
  local i
  echo "table inet e12 {"
  echo "  counter viatun {}"
  echo "  counter viau0 {}"
  for i in $(seq 1 "$N"); do
    echo "  counter o$i {}"; echo "  counter d$i {}"; echo "  counter i$i {}"
    echo "  set al$i { type ipv4_addr; flags interval; elements = { 10.66.0.0/24 } }"
  done
  echo "  map cin { type ipv4_addr : counter; elements = {"
  for i in $(seq 1 "$N"); do printf '    %s : "i%s"\n' "$(ip_of "$i")" "$i"; done |
    paste -sd, -
  echo "  } }"
  echo "  map out { type ipv4_addr : verdict; elements = {"
  for i in $(seq 1 "$N"); do printf '    %s : jump a%s\n' "$(ip_of "$i")" "$i"; done |
    paste -sd, -
  echo "  } }"
  for i in $(seq 1 "$N"); do
    echo "  chain a$i { ip daddr @al$i counter name \"o$i\" accept; counter name \"d$i\" drop; }"
  done
  cat << X
  chain agents_fwd {
    type filter hook forward priority 0; policy drop;
    tcp flags syn tcp option maxseg size set rt mtu
    ct state established,related ip daddr 10.77.0.0/16 counter name ip daddr map @cin accept
    ip saddr vmap @out
  }
  chain path_count {
    type filter hook postrouting priority 0;
    ip saddr 10.77.0.0/16 oifname "tun0" counter name "viatun"
    ip saddr 10.77.0.0/16 oifname "u0" counter name "viau0"
  }
  chain agents_nat {
    type nat hook postrouting priority srcnat;
    ip saddr 10.77.0.0/16 oifname { "tun0", "u0" } masquerade
  }
}
X
}

ctr() { ip netns exec $RTR nft list counter inet e12 "$1" | awk '/bytes/ { print $NF }'; }

# fetch in agents FROM..TO for SECS; files under runs/TAG.*
fetch() {
  local tag=$1 from=$2 to=$3 secs=$4 url=${5:-http://10.66.0.1/page.bin} i
  rm -f "$B/runs/$tag".*
  for i in $(seq "$from" "$to"); do
    ip netns exec "e12-a$i" "$B/fetch.sh" "$url" "$secs" "$B/runs/$tag.$i" &
  done
}

measure() {
  local s0=$1 m0=$2 m1 i
  sleep 3
  m1=$(mem)
  set -- $m0; local a0=$1 sl0=$2 su0=$3 pc0=$4 ks0=$5
  set -- $m1
  row mem_per_agent_avail_kb "$(( (a0 - $1) / N ))" kB
  row mem_per_agent_slab_kb "$(( ($2 - sl0) / N ))" kB
  row mem_per_agent_sunreclaim_kb "$(( ($3 - su0) / N ))" kB
  row mem_per_agent_percpu_kb "$(( ($4 - pc0) / N ))" kB
  row mem_per_agent_kstack_kb "$(( ($5 - ks0) / N ))" kB
  slab | join - "$s0" | awk -v n="$N" '{ d = $2 - $3; if (d > 0)
      printf "%s %.1f\n", $1, d / n / 1024 }' | sort -k2 -rn | head -6 |
    while read -r c k; do row "slab_per_agent_$c" "$k" kB; done
}

all() {
  : > "$R"
  trap teardown EXIT
  snap > "$B/snap.before"
  lsmod | awk 'NR > 1 { print $1 }' | sort > "$B/lsmod.before"
  row n_agents "$N" count

  local t0 m0 s0 i
  t0=$(now); base; row setup_base_s "$(el "$t0")" s
  grep -o -m1 'Data Channel: Cipher .*' "$B/cli.log" |
    sed 's/^/# /' | tee -a "$R"
  sleep 2
  m0=$(mem); slab > "$B/slab.0"
  t0=$(now); agents
  ip netns exec $RTR nft -f <(gen_nft) || { log nft failed; exit 1; }
  row setup_total_s "$(el "$t0")" s
  measure "$B/slab.0" "$m0"

  # every agent reaches its allowed site
  t0=$(now)
  ok=$(seq 1 "$N" | xargs -P 50 -I{} ip netns exec e12-a{} curl -s \
    -o /dev/null --max-time 5 -w '%{http_code}\n' http://10.66.0.1/small.txt |
    grep -c '^200$')
  row reach_ok "$ok/$N" agents
  row reach_all_s "$(el "$t0")" s
  ip netns exec e12-a1 curl -s -o /dev/null --max-time 3 http://10.66.1.1/small.txt
  row deny_curl_rc "$?" "rc (28 = timed out, dropped)"
  row deny_counter_bytes "$(ctr d1)" bytes
  row neigh_rtr_br0 "$(ip -n $RTR neigh show dev br0 | wc -l)" entries
  row arp_cache_table_fulls "$(awk 'NR == 2 { print $13 }' /proc/net/stat/arp_cache)" hex
  row arp_cache_forced_gc "$(awk 'NR == 2 { print $11 }' /proc/net/stat/arp_cache)" hex
  # bridged agent-to-agent traffic skips the forward chain
  ip netns exec e12-a1 ping -c1 -W1 "$(ip_of 2)" > /dev/null
  row a2a_ping_rc_open "$?" "rc (0 = agents reach each other at L2)"
  for i in $(seq 1 "$N"); do echo "link set dev vb$i isolated on"; done |
    ip netns exec $RTR bridge -batch -
  ip netns exec e12-a1 ping -c1 -W1 "$(ip_of 2)" > /dev/null
  row a2a_ping_rc_isolated "$?" "rc (1 = blocked)"
  ip netns exec e12-a1 curl -s -o /dev/null --max-time 5 http://10.66.0.1/small.txt
  row isolated_still_reaches_site "$?" rc

  # unloaded latency, 1.5 MB page
  for i in 1 501; do
    ip netns exec "e12-a$i" env MAXN=20 "$B/fetch.sh" http://10.66.0.1/page.bin 600 \
      "$B/runs/lat.$i"
    row "lat_$([ $i = 1 ] && echo vpn || echo direct)" \
      "$(summ "$B/runs/lat.$i")" "n ok err Mbps p50_s p95_s"
  done

  # 50 concurrent agents, VPN then direct
  local c0 s0b h0 h1
  c0="$(cpu_t "$(cat "$B/cli.pid")") $(cpu_t "$(cat "$B/vpns.pid")")"
  h0=$(host_cpu); t0=$(now)
  fetch tput_vpn 1 50 20; wait
  local w; w=$(el "$t0"); h1=$(host_cpu)
  set -- $c0
  row tput_vpn "$(summ "$B"/runs/tput_vpn.*)" "n ok err Mbps p50_s p95_s"
  row openvpn_client_cpu "$(awk -v a="$1" -v b="$(cpu_t "$(cat "$B/cli.pid")")" \
    -v w="$w" -v hz="$(getconf CLK_TCK)" 'BEGIN { printf "%.0f", (b - a) / hz / w * 100 }')" \
    "% of one core"
  row openvpn_server_cpu "$(awk -v a="$2" -v b="$(cpu_t "$(cat "$B/vpns.pid")")" \
    -v w="$w" -v hz="$(getconf CLK_TCK)" 'BEGIN { printf "%.0f", (b - a) / hz / w * 100 }')" \
    "% of one core"
  row host_busy_during_vpn "$(echo "$h0 $h1" | awk '{ t = $3 - $1; i = $4 - $2
    printf "%.0f", (t - i) / t * 100 }')" "% of 32 threads"
  fetch tput_direct 501 550 20; wait
  row tput_direct "$(summ "$B"/runs/tput_direct.*)" "n ok err Mbps p50_s p95_s"

  # live flip: agents 1-100 to direct and back while 1-20 fetch
  local vt vu
  vt=$(ctr viatun); vu=$(ctr viau0)
  fetch flip 1 20 30
  sleep 10
  for i in $(seq 1 100); do echo "rule add from $(ip_of "$i") lookup 200 pref 500"; done > "$B/flip.batch"
  t0=$(now); ip -n $RTR -batch "$B/flip.batch"; row flip_100_s "$(el "$t0")" s
  row flip_route_a1 "$(ip -n $RTR route get 10.66.0.1 from 10.77.1.1 iif vb1 | head -1)" route
  sleep 10
  local vt2 vu2; vt2=$(ctr viatun); vu2=$(ctr viau0)
  sed 's/rule add/rule del/' "$B/flip.batch" > "$B/unflip.batch"
  t0=$(now); ip -n $RTR -batch "$B/unflip.batch"; row unflip_100_s "$(el "$t0")" s
  wait
  row flip_fetches "$(summ "$B"/runs/flip.*)" "n ok err Mbps p50_s p95_s"
  row flip_window_bytes_via "tun $((vt2 - vt)) u0 $((vu2 - vu))" "bytes, 10 s before flip + 10 s flipped"

  # tunnel drop with the kill switch (unreachable default in table 100)
  drop kill_switch
  # and without it: VPN agents fall through to main's default route
  ip -n $RTR route del unreachable default table 100
  drop no_kill_switch
  ip -n $RTR route add unreachable default table 100 metric 4000

  trap - EXIT
  teardown
  t0=$(now)
  set -- $m0
  for i in $(seq 1 120); do
    set -- $m0; local p0=$4
    [ "$(mem | awk '{ print $4 }')" -le $((p0 + p0 / 50)) ] && break
    sleep 0.5
  done
  row teardown_settle_s "$(el "$t0")" "s (Percpu back within 2%)"
  snap > "$B/snap.after"
  lsmod | awk 'NR > 1 { print $1 }' | sort > "$B/lsmod.after"
  if diff "$B/snap.before" "$B/snap.after" > "$B/snap.diff"; then
    row host_snapshot_diff 0 "lines (identical)"
  else
    row host_snapshot_diff "$(wc -l < "$B/snap.diff")" lines
  fi
  row modules_loaded_by_test "$(comm -13 "$B/lsmod.before" "$B/lsmod.after" | paste -sd, -)" names
}

drop() {
  local tag=$1 vu t0 r
  vu=$(ctr viau0)
  fetch "drop_$tag" 1 20 40
  sleep 10
  local tk; tk=$(now)
  kill -9 "$(cat "$B/cli.pid")"
  sleep 10
  r=$(vpn_up)
  row "drop_${tag}_restart" "$r" "s to tunnel up, s to route restored"
  wait
  # fetches that started during the outage and succeeded = leaked
  local tu; tu=$(awk -v k="$tk" -v r="$r" 'BEGIN { split(r, x, " "); printf "%.3f", k + 10 + x[2] }')
  row "drop_${tag}_outage" "$(cat "$B"/runs/drop_"$tag".* | awk -v a="$tk" -v b="$tu" \
    '$1 >= a && $1 < b { n++; if ($5 == 0 && $4 == 200) ok++; else rc[$5]++ }
     END { printf "started %d ok %d", n, ok; for (k in rc) printf " rc%s=%d", k, rc[k] }')" \
    "fetches started while down"
  row "drop_${tag}_recover" "$(cat "$B"/runs/drop_"$tag".* | awk -v b="$tu" \
    '$1 >= b && $5 == 0 && $4 == 200 { if (m == "" || $1 + $2 < m) m = $1 + $2 }
     END { if (m == "") print "NA"; else printf "%.2f", m - b }')" \
    "s from route restored to first good fetch"
  row "drop_${tag}_leak_bytes_u0" "$(( $(ctr viau0) - vu ))" "bytes from VPN agents out the direct path"
}

case "${1:-all}" in
  all) all ;;
  teardown) teardown ;;
  *) echo "usage: $0 all|teardown" >&2; exit 2 ;;
esac
