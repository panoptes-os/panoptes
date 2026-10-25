#!/usr/bin/env bash
# Seed ERPNext with the demo's suppliers, items, and submitted purchase
# orders through its REST API. Idempotent: existing suppliers and items are
# skipped, and a purchase order is found by its order key, which lives in
# the order's "order_confirmation_no" field. Writes DATA/po_map.tsv.
# usage: seed.sh DATA [URL]
set -euo pipefail
D=${1:?usage: seed.sh DATA [URL]} U=${2:-http://127.0.0.1:8080}
J=$(mktemp) && trap 'rm -f "$J" "$J".*' EXIT
COMPANY="Panoptes Demo Co"

curl -fsS -c "$J" -X POST "$U/api/method/login" -H 'Content-Type: application/json' \
  -d '{"usr":"Administrator","pwd":"admin"}' > /dev/null
api() { curl -sS -b "$J" -H 'Content-Type: application/json' "$@"; }
exists() { [ "$(api -o /dev/null -w '%{http_code}' -G "$U/api/resource/$(jq -rn --arg x "$1" '$x|@uri')/$(jq -rn --arg x "$2" '$x|@uri')")" = 200 ]; }
post() { # DOCTYPE JSON: insert, print the new name or fail loudly
  # Parallel inserts deadlock on the naming-series row; MariaDB rolled the
  # transaction back, so a retry cannot duplicate the document.
  local out try name
  for try in 1 2 3 4 5 6; do
    out=$(api -X POST "$U/api/resource/$(jq -rn --arg x "$1" '$x|@uri')" -d "$2")
    name=$(jq -r '.data.name // empty' <<< "$out" 2> /dev/null)
    [ -n "$name" ] && { printf '%s\n' "$name"; return 0; }
    grep -q QueryDeadlockError <<< "$out" || break
    sleep "0.$((RANDOM % 9 + 1))"
  done
  echo "insert $1 failed after $try tries: $(head -c 400 <<< "$out")" >&2; return 1
}

n=0
while read -r name; do
  exists Supplier "$name" && continue
  post Supplier "$(jq -nc --arg n "$name" '{supplier_name:$n, supplier_group:"All Supplier Groups", supplier_type:"Company"}')" > /dev/null
  n=$((n + 1))
done < <(jq -r '.[].name' "$D/suppliers.json")
echo "suppliers: $n new"

n=0
while read -r it; do
  code=$(jq -r .code <<< "$it")
  exists Item "$code" && continue
  post Item "$(jq -c '{item_code:.code, item_name:.name, description:.name, item_group:"Products",
    stock_uom:"Nos", is_stock_item:0, is_purchase_item:1, standard_rate:.rate}' <<< "$it")" > /dev/null
  n=$((n + 1))
done < <(jq -c '.[]' "$D/items.json")
echo "items: $n new"

# Each tax template's rows, so orders carry their taxes explicitly.
api -G "$U/api/resource/Purchase%20Taxes%20and%20Charges%20Template" --data-urlencode limit_page_length=0 \
  | jq -r '.data[].name' | while read -r t; do
    api -G "$U/api/resource/Purchase%20Taxes%20and%20Charges%20Template/$(jq -rn --arg x "$t" '$x|@uri')" \
      | jq -c --arg t "$t" '{($t): [.data.taxes[] | {charge_type, account_head, rate, description,
          category, add_deduct_tax, cost_center}]}'
  done | jq -sc 'add // {}' > "$J.taxes"

# Orders already in ERPNext, by key.
api -G "$U/api/resource/Purchase%20Order" --data-urlencode 'fields=["name","order_confirmation_no","docstatus"]' \
  --data-urlencode 'filters=[["order_confirmation_no","like","DEMO-PO-%"],["docstatus","=",1]]' \
  --data-urlencode limit_page_length=0 \
  | jq -r '.data[] | "\(.order_confirmation_no)\t\(.name)"' | sort > "$J.have"
cut -f1 "$J.have" > "$J.keys"
echo "orders: $(wc -l < "$J.have") already submitted"

one() { # ORDER-JSON: insert and submit one purchase order, print "key<TAB>name"
  local body name
  body=$(jq -c --arg c "$COMPANY" --slurpfile tx "$J.taxes" '{supplier, transaction_date:.date,
    schedule_date, company:$c, order_confirmation_no:.key, docstatus:1,
    items: [.lines[] as $l | {item_code:$l.item_code, qty:$l.qty, rate:$l.rate}],
    taxes_and_charges: (if .tax_template == "" then null else .tax_template end),
    taxes: (if .tax_template == "" then [] else $tx[0][.tax_template] end)}' <<< "$1")
  name=$(post "Purchase Order" "$body") || return 1
  printf '%s\t%s\n' "$(jq -r .key <<< "$1")" "$name"
}
export -f one post api && export J U COMPANY
jq -c . "$D/orders.jsonl" | while read -r o; do
  grep -qxF "$(jq -r .key <<< "$o")" "$J.keys" || printf '%s\n' "$o"
done > "$J.todo"
echo "orders: $(wc -l < "$J.todo") to insert"
tr '\n' '\0' < "$J.todo" | xargs -0 -r -P "${SEED_JOBS:-1}" -I{} bash -c 'one "$1"' _ {} > "$J.new"
sort "$J.have" "$J.new" > "$D/po_map.tsv"
echo "orders: $(wc -l < "$J.new") inserted, $(wc -l < "$D/po_map.tsv") mapped"
[ "$(wc -l < "$D/po_map.tsv")" -eq "$(wc -l < "$D/orders.jsonl")" ] || { echo "map incomplete" >&2; exit 1; }
