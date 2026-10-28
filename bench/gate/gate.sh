#!/usr/bin/env bash
# gate.sh ARM PROVIDER INVOICES [COST_CAP]: clean.sh the invoices, then one
# panoptes-loop per invoice in turn (one agent at a time), each verified by
# the loop over ERPNext's REST API. Output in panoptes-run/gate/ARM/INVOICE/.
# Stops once the account ledger reaches BUDGET_USD (default 4.95, under the
# $5 free credit). BASE picks the config (default base.json).
set -uo pipefail
B=/var/tmp/glarb-bench G=$B/panoptes-run/gate S=$B/scripts/gate
. $B/scripts/loop/jq.sh
arm=$1 prov=$2 inv=$3 cap=${4:-0.5}
mkdir -p $G/$arm
bash $B/scripts/loop/clean.sh "$inv"
for i in ${inv//,/ }; do
  [ -f $G/STOP ] && { echo "stop file"; break; }
  jq --arg p "$prov" --arg n "$arm/$i" --argjson c "$cap" \
    '.provider = $p | .run_name = $n | .invoice_cost_cap_usd = $c' $S/${BASE:-base.json} > $G/$arm/$i.json
  $B/panoptes-gate/target/release/panoptes-loop $G/$arm/$i.json --invoices "$i" \
    > $G/$arm/$i.summary 2> $G/$arm/$i.log
  echo "$i loop exit $? spent $(cat $G/spent.usd 2>/dev/null || echo 0)"
  jq -c '{invoice, done, verified, attached, failed, steps, cost_usd, error, wall_ms}' $G/$arm/$i/results.jsonl
  if awk -v s="$(cat $G/spent.usd 2>/dev/null || echo 0)" -v b="${BUDGET_USD:-4.95}" 'BEGIN{exit !(s >= b)}'; then echo "budget reached"; break; fi
done
