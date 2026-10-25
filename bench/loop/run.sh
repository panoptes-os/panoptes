#!/usr/bin/env bash
# run.sh NAME DELAY_MS INVOICES: oracle at DELAY_MS, panoptes-loop over the
# comma-separated INVOICES, then verify.sh on what was assigned.
set -uo pipefail
B=/var/tmp/glarb-bench P=$B/panoptes L=$B/panoptes-run/loop D=$B/demo/data
. $B/scripts/loop/jq.sh
name=$1 delay=$2 inv=$3
mkdir -p $L/$name/oracle && jq --arg n "$name" '.run_name = $n' $B/scripts/loop/oracle.json > $L/$name.json
$P/target/release/panoptes-oracle --truth $D/truth.jsonl --creds $D/creds.tsv --delay-ms "$delay" --log-dir $L/$name/oracle \
  > $L/$name.oracle.log 2>&1 & op=$!
trap 'kill $op 2>/dev/null' EXIT
for _ in $(seq 50); do curl -fsS http://127.0.0.1:8099/v1/models > /dev/null 2>&1 && break; sleep 0.1; done
$P/target/release/panoptes-loop $L/$name.json --invoices "$inv" > $L/$name/summary.out 2> $L/$name.loop.log
echo "loop exit $?"
cat $L/$name/results.jsonl | jq -c '{invoice, done, steps, error, note, wall_ms}'
(cd $B/demo && bash gen/verify.sh data $L/$name/assigned.txt)
