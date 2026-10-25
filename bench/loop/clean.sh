#!/usr/bin/env bash
# clean.sh INVOICES: cancel submitted and delete draft Purchase Invoices whose
# bill_no belongs to the comma-separated invoice ids; prints what it did.
set -euo pipefail
B=/var/tmp/glarb-bench; . $B/scripts/loop/jq.sh
nos=$(jq -r --arg ids "$1" '($ids | split(",")) as $w | select(.file as $f | $w | index($f)) | .invoice_no' \
  $B/demo/data/truth.jsonl)
bash $B/scripts/erpapi.sh list | jq -r --arg nos "$nos" '($nos | split("\n")) as $w
  | .data[] | select(.bill_no as $b | $w | index($b)) | select(.docstatus < 2) | "\(.docstatus) \(.name) \(.bill_no)"' \
  | while read -r ds name bill; do
      if [ "$ds" = 1 ]; then bash $B/scripts/erpapi.sh cancel "$name"; echo "cancelled $name ($bill)"
      else bash $B/scripts/erpapi.sh delete "$name"; echo "deleted draft $name ($bill)"; fi
    done
