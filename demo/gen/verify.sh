#!/usr/bin/env bash
# Score ERPNext purchase invoices against the demo's ground truth.
# An invoice counts as found when a purchase invoice from its supplier carries
# its number in "bill_no" (Supplier Invoice No). Each field is then checked:
# date, purchase order, lines (item, qty, rate), tax, total, and submitted.
# "all" is those together; "attached" (a File record attached to the
# invoice) is reported apart and is not part of "all".
# usage: verify.sh DATA [FILES]   FILES lists the assigned inv-NNNN ids;
#                                 default is every invoice in truth.jsonl
set -euo pipefail
D=${1:?usage: verify.sh DATA [FILES]} U=${URL:-http://127.0.0.1:8080}
T=$(mktemp -d) && trap 'rm -rf "$T"' EXIT
curl -fsS -c "$T/jar" -X POST "$U/api/method/login" -H 'Content-Type: application/json' \
  -d '{"usr":"Administrator","pwd":"admin"}' > /dev/null
api() { curl -fsS -b "$T/jar" "$@"; }

if [ -n "${2:-}" ]; then
  jq -c --rawfile f "$2" '($f | split("\n") | map(select(. != ""))) as $ids
    | select(.file as $x | $ids | index($x))' "$D/truth.jsonl" > "$T/truth"
else
  cp "$D/truth.jsonl" "$T/truth"
fi

api -G "$U/api/resource/Purchase%20Invoice" --data-urlencode limit_page_length=0 \
  --data-urlencode 'fields=["name","supplier","bill_no","docstatus"]' \
  --data-urlencode 'filters=[["docstatus","<",2]]' > "$T/list"
# Full documents only for invoices that match a ground-truth row.
jq -r --slurpfile t "$T/truth" '($t | map("\(.supplier)\u0001\(.invoice_no)") ) as $want
  | .data[] | select(("\(.supplier)\u0001\(.bill_no)") as $k | $want | index($k)) | .name' "$T/list" \
  | while read -r n; do
      a=$(api -G "$U/api/resource/File" --data-urlencode limit_page_length=0 \
        --data-urlencode "filters=$(jq -cn --arg n "$n" '[["attached_to_doctype","=","Purchase Invoice"],["attached_to_name","=",$n]]')" \
        | jq '.data | length')
      api "$U/api/resource/Purchase%20Invoice/$(jq -rn --arg x "$n" '$x|@uri')" | jq -c --argjson a "$a" '.data + {attached: $a}'
    done > "$T/docs"

jq -rn --slurpfile t "$T/truth" --slurpfile d "$T/docs" '
  def near(a; b): ((a // -1) - (b // -2) | fabs) < 0.006;
  # Arithmetic first: jq 1.7 keeps ERPNext literals such as 12.0 verbatim
  # in string interpolation, so "12.0" would never equal "12".
  def lines(x): x | map("\(.item_code)|\(.qty * 1000 | round)|\(.rate * 100 | round)") | sort;
  ($d | map({("\(.supplier)\u0001\(.bill_no)"): .}) | add // {}) as $by
  | [$t[] | . as $g | $by["\(.supplier)\u0001\(.invoice_no)"] as $e | {
      found: ($e != null),
      date: ($e.bill_date == $g.date),
      po: (($e.items // []) | length > 0 and all(.purchase_order == $g.po)),
      lines: ($e != null and lines($e.items) == lines($g.lines)),
      tax: ($e != null and near($e.total_taxes_and_charges; $g.tax)),
      total: ($e != null and near($e.grand_total; $g.total)),
      submitted: ($e.docstatus == 1),
      attached: (($e.attached // 0) > 0)
    } | . + {all: (.found and .date and .po and .lines and .tax and .total and .submitted)}
      | . + {all_attached: (.all and .attached)}]
  | length as $n
  | "expected \($n), found \(map(select(.found)) | length)",
    (["found","date","po","lines","tax","total","submitted","all","attached","all_attached"][] as $f
      | (map(select(.[$f])) | length) as $c
      | "\($f)\t\($c)/\($n)\t\(if $n > 0 then ($c * 1000 / $n | round / 10) else 0 end)%")'
