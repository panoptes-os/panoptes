#!/usr/bin/env bash
# all.sh TAG: 1 and 10 agents at 50 ms and 2 s oracle delay, cleaning the
# test invoices before each run. Runs land in panoptes-run/loop/TAG-*.
B=/var/tmp/glarb-bench/scripts/loop TAG=${1:?usage: all.sh TAG}
ten=$(seq -f 'inv-%04g' 1 10 | paste -sd,)
for spec in "1x50 50 inv-0001" "1x2000 2000 inv-0001" "10x50 50 $ten" "10x2000 2000 $ten"; do
  set -- $spec
  echo "### $1"; bash $B/clean.sh "$ten"
  timeout 900 bash $B/run.sh "$TAG-$1" "$2" "$3"
done
