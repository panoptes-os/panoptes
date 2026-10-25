#!/usr/bin/env bash
# page.sh PATH N: N parallel anonymous GETs of PATH; prints path n errors p50 p95 codes
set -u; p=$1 n=$2
seq "$n" | xargs -P "$n" -I{} curl -s -o /dev/null -w '%{http_code} %{time_total}\n' "http://127.0.0.1:8080$p" |
awk -v k="$p" -v n="$n" '{c[$1]++; t[NR]=$2*1000; if($1!=200) e++} END{asort(t);
  printf "%s\t%d\t%d\t%.0f\t%.0f\t", k, n, e+0, t[int(NR*0.5)], t[int(NR*0.95)]; for(x in c) printf "%s:%s ", x, c[x]; print ""}'
