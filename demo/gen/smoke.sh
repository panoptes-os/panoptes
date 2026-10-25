#!/usr/bin/env bash
# Smoke test for the mock portals: sign in to two portals with curl, list
# invoices, download one PDF each, and exercise the staged challenge.
# usage: smoke.sh DATA [PORTALS_URL]
set -euo pipefail
D=${1:?usage: smoke.sh DATA [URL]} P=${2:-http://127.0.0.1:8090}
T=$(mktemp -d) && trap 'rm -rf "$T"' EXIT

signin() { # SLUG: sign in with the generated credentials, keep the cookie jar
  local f u pw
  read -r f u pw < <(awk -F'\t' -v s="$1" '$1 == s {print $3, $4, $5}' "$D/creds.tsv")
  curl -fsS -c "$T/$1.jar" -o /dev/null -w "%{redirect_url}" -X POST "$P/$1/login" \
    --data-urlencode "$f=$u" --data-urlencode "password=$pw"
}

for slug in alderbrook brightwater; do
  echo "$slug: login redirects to $(signin "$slug")"
  html=$(curl -fsS -b "$T/$slug.jar" "$P/$slug/invoices")
  echo "$slug: $(grep -o '[0-9]\+ documents' <<< "$html"), $(grep -o 'Page 1 of [0-9]*' <<< "$html")"
  file=$(grep -o 'inv-[0-9]\{4\}' <<< "$html" | head -1)
  curl -fsS -b "$T/$slug.jar" -o "$T/$file.pdf" "$P/$slug/download/$file.pdf"
  echo "$slug: downloaded $file.pdf, $(head -c 5 "$T/$file.pdf"), $(wc -c < "$T/$file.pdf") bytes"
  no=$(jq -r --arg f "$file" 'select(.file == $f) | .invoice_no' "$D/truth.jsonl")
  echo "$slug: truth says $file is $no; PDF text has it: $(nix shell nixpkgs#poppler-utils -c pdftotext "$T/$file.pdf" - | grep -cF "$no")"
done

echo "bad password: $(curl -s -o /dev/null -w '%{http_code}' -X POST "$P/alderbrook/login" -d username=alderbrook-ap -d password=wrong)"
echo "no session: $(curl -s -o /dev/null -w '%{http_code} %{redirect_url}' "$P/alderbrook/invoices")"

curl -fsS "$P/admin/captcha?portal=cobaltline&on=1"
echo "cobaltline with challenge: login redirects to $(signin cobaltline)"
echo "invoices before solving: $(curl -s -b "$T/cobaltline.jar" -o /dev/null -w '%{http_code} %{redirect_url}' "$P/cobaltline/invoices")"
curl -fsS -b "$T/cobaltline.jar" "$P/cobaltline/challenge" | grep -o 'DEMO CHALLENGE[^.]*\.'
echo "wrong answer: $(curl -s -b "$T/cobaltline.jar" -o /dev/null -w '%{redirect_url}' -X POST "$P/cobaltline/challenge" -d answer=XXXXX)"
curl -fsS "$P/admin/captcha?portal=cobaltline&on=0"
curl -fsS "$P/admin/captcha?portal=cobaltline&on=1" > /dev/null
signin cobaltline > /dev/null
code=$(curl -fsS -b "$T/cobaltline.jar" "$P/cobaltline/challenge" | grep -o '>[A-Z0-9]</text>' | tr -d '<>/text' | tr -d '\n')
echo "right answer ($code): $(curl -s -b "$T/cobaltline.jar" -o /dev/null -w '%{redirect_url}' -X POST "$P/cobaltline/challenge" -d "answer=$code")"
echo "invoices after solving: $(curl -s -b "$T/cobaltline.jar" -o /dev/null -w '%{http_code}' "$P/cobaltline/invoices")"
curl -fsS "$P/admin/captcha?portal=cobaltline&on=0"
