#!/bin/sh
# Scripted end-to-end check of a running panoptes-desk; no model involved.
#
# usage: e2e.sh SOCK OUT_DIR
#
# 1. log into one mock supplier portal, open its invoice list, download a PDF
# 2. log into ERPNext, open a new Purchase Invoice, dump its ax_tree
# 3. one MCP session on SOCK: initialize, tools/list, tools/call screenshot
#
# Start from a fresh desk: both logins expect their sign-in forms. Needs jq
# and socat on PATH. Every tool reply's timing goes to
# OUT_DIR/timing.txt.
set -eu

SOCK=$1
OUT=$2
CTL=${PANOPTESCTL:-panoptesctl}
CREDS=${CREDS:-/var/tmp/glarb-bench/demo/data/creds.tsv}
SLUG=${SLUG:-alderbrook}
PORTAL=${PORTAL:-http://127.0.0.1:8090}
ERP=${ERP:-http://127.0.0.1:8080}
mkdir -p "$OUT"
: > "$OUT/timing.txt"

say() { printf '== %s\n' "$*" >&2; }
die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# call TOOL [ARGS_JSON]: print the reply, log its timing, fail on ok=false.
call() {
	a=${2-}
	[ -n "$a" ] || a='{}'
	r=$("$CTL" desk "$SOCK" "$1" "$a") || die "$1 $(printf %s "$r" | jq -c '{error,timing}')"
	printf '%-13s %s\n' "$1" "$(printf %s "$r" | jq -c .timing)" >> "$OUT/timing.txt"
	printf '%s\n' "$r"
}

tree() { call ax_tree "{\"max_bytes\":${1:-24000}}" | jq -r .result.tree; }

# node TREE PATTERN: the id of the first ax_tree line matching PATTERN.
node() {
	id=$(printf '%s\n' "$1" | grep -m1 -E "$2" | sed -n 's/^ *\[\([0-9]*\)\].*/\1/p')
	[ -n "$id" ] || die "no node matches /$2/"
	printf '%s\n' "$id"
}

type_into() { call type "$(jq -nc --argjson n "$1" --arg t "$2" '{node_id:$n,text:$t}')" >/dev/null; }
click() { call click "{\"node_id\":$1}" >/dev/null; }
go() { call navigate "$(jq -nc --arg u "$1" '{url:$u}')" | jq -c .result >&2; }
settle() { call wait_settled "{\"timeout_ms\":${1:-8000}}" | jq -c .result >&2; }

# 1. Portal: log in, list invoices, download one PDF.
say "portal $SLUG: log in"
user=$(awk -F'\t' -v s="$SLUG" '$1 == s { print $4 }' "$CREDS")
pass=$(awk -F'\t' -v s="$SLUG" '$1 == s { print $5 }' "$CREDS")
[ -n "$user" ] || die "no creds for $SLUG"
go "$PORTAL/$SLUG/login"
t=$(tree)
type_into "$(node "$t" 'textbox "Username"')" "$user"
type_into "$(node "$t" 'textbox "Password"')" "$pass"
click "$(node "$t" 'button "Log in"')"
settle

say "portal $SLUG: invoice list"
t=$(tree)
printf '%s\n' "$t" > "$OUT/portal-invoices.txt"
printf '%s\n' "$t" | grep -q 'heading "Invoices"' || die "not on the invoice list"
printf '%s\n' "$t" | grep -c 'link "Download PDF"' | sed 's/^/download links: /' >&2

say "portal $SLUG: download the first PDF"
click "$(node "$t" 'link "Download PDF"')"
i=0
while :; do
	d=$(call download_list | jq -c '.result')
	f=$(printf '%s\n' "$d" | jq -r 'first(.events[] | select(.state == "completed")
		| .suggestedFilename) as $n | "\(.dir)/\($n)"' 2>/dev/null || true)
	[ -z "$f" ] || break
	i=$((i + 1))
	[ $i -lt 50 ] || die "download did not complete: $d"
	sleep 0.2
done
printf '%s\n' "$d" | jq -c '{files, events: [.events[] | {state, suggestedFilename, totalBytes}]}' >&2
head -c 5 "$f" | grep -q '%PDF-' || die "$f is not a PDF"
cp "$f" "$OUT/"
call screenshot '{"scale":0.5}' | jq -r .image.data | base64 -d > "$OUT/portal.jpg"

# 2. ERPNext: log in, open a new Purchase Invoice, dump its tree.
say "erpnext: log in"
go "$ERP/login"
settle
t=$(tree)
printf '%s\n' "$t" > "$OUT/erp-login.txt"
# ERPNext's admin account is "Administrator"; "admin" is its password here.
type_into "$(node "$t" 'textbox "Email"')" "${ERP_USER:-Administrator}"
type_into "$(node "$t" 'textbox "Password"')" "${ERP_PASS:-admin}"
call key '{"key":"Enter"}' >/dev/null
settle 15000
tree 2000 | grep -q 'Invalid credentials' && die "erpnext rejected the login"

say "erpnext: new Purchase Invoice"
go "$ERP/app/purchase-invoice/new"
settle 15000
t=$(tree 60000)
printf '%s\n' "$t" > "$OUT/erp-purchase-invoice.txt"
printf '%s\n' "$t" | grep -q 'Purchase Invoice' || die "no Purchase Invoice in the tree"
printf '%s\n' "$t" | grep -q 'Supplier' || die "no Supplier field in the tree"
printf 'erp tree: %s lines, %s bytes\n' "$(printf '%s\n' "$t" | wc -l)" "$(printf '%s\n' "$t" | wc -c)" >&2
call screenshot '{"scale":0.5}' | jq -r .image.data | base64 -d > "$OUT/erp.jpg"

# 3. MCP on the same socket.
say "mcp on $SOCK"
"$(dirname "$0")/mcp-session.sh" | socat -t 10 - "UNIX-CONNECT:$SOCK" > "$OUT/mcp-socket.jsonl"
"$(dirname "$0")/mcp-check.sh" "$OUT/mcp-socket.jsonl"

say "ok; timings in $OUT/timing.txt"
