#!/usr/bin/env bash
# usage: burst.sh KIND N   KIND: login | po. Prints: kind n errors p50_ms p95_ms codes
set -u
kind=$1 n=$2 B=http://127.0.0.1:8080 D=/var/tmp/glarb-bench/erpnext/scale
po=$(cat "$D/po.name")
one() {
  i=$1 j=$(mktemp)
  if [ "$kind" = login ]; then
    curl -s -o /dev/null -c "$j" -w '%{http_code} %{time_total}\n' -X POST "$B/api/method/login" -d usr=Administrator -d pwd=admin
  else
    curl -s -o /dev/null -b "$D/admin.jar" -w '%{http_code} %{time_total}\n' "$B/api/resource/Purchase%20Order/$po"
  fi
  rm -f "$j"
}
export -f one; export kind B D po
seq "$n" | xargs -P "$n" -I{} bash -c 'one {}' > "$D/last.txt"
awk -v k="$kind" -v n="$n" '{c[$1]++; t[NR]=$2*1000; if($1!=200) e++} END{
  asort(t); p50=t[int(NR*0.5)>0?int(NR*0.5):1]; p95=t[int(NR*0.95)>0?int(NR*0.95):1];
  s=""; for(x in c) s=s x ":" c[x] " "; printf "%s\t%d\t%d\t%.0f\t%.0f\t%s\n", k, n, e+0, p50, p95, s}' "$D/last.txt"
