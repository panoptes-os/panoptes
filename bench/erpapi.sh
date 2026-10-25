#!/usr/bin/env bash
# erpapi.sh cancel NAME | delete NAME | list: test-invoice cleanup over
# ERPNext's REST API (Administrator/admin on 127.0.0.1:8080).
set -euo pipefail
U=${URL:-http://127.0.0.1:8080} T=$(mktemp) && trap 'rm -f "$T"' EXIT
curl -fsS -c "$T" -X POST "$U/api/method/login" -H 'Content-Type: application/json' \
  -d '{"usr":"Administrator","pwd":"admin"}' > /dev/null
case $1 in
  cancel) curl -fsS -b "$T" -X POST "$U/api/method/frappe.client.cancel" -H 'Content-Type: application/json' \
            -d "{\"doctype\":\"Purchase Invoice\",\"name\":\"$2\"}" > /dev/null ;;
  delete) curl -fsS -b "$T" -X DELETE "$U/api/resource/Purchase%20Invoice/$2" > /dev/null ;;
  list)   curl -fsS -b "$T" -G "$U/api/resource/Purchase%20Invoice" --data-urlencode limit_page_length=0 \
            --data-urlencode 'fields=["name","bill_no","docstatus"]' ;;
esac
