# Panoptes demo data and supplier portals

Scratch build, 2026-10-27. Runs on the test server under
`/var/tmp/glarb-bench/demo`; every port binds to 127.0.0.1.

```text
demo/
├─ gen/
│  ├─ gen.lua       deterministic data from seed 20261027: masters, orders, truth
│  ├─ invoice.typ   one template, 20 layouts chosen by supplier
│  ├─ seed.sh       idempotent ERPNext seed over REST; writes data/po_map.tsv
│  ├─ verify.sh     scores ERPNext purchase invoices against truth.jsonl
│  └─ smoke.sh      signs in to two portals, downloads PDFs, tests the challenge
├─ portals/         cargo workspace: axum server for the 20 supplier portals
├─ data-snapshot/   truth, creds, PO map, masters, four sample PDFs
└─ web-tasks.jsonl  20 tasks on public sites for `panoptes-loop --tasks`; no ERPNext
```

Regenerate, reseed, and run (on the server, in `/var/tmp/glarb-bench/demo`):

```sh
lua gen/gen.lua masters data
bash gen/seed.sh data                       # SEED_JOBS=1; parallel inserts deadlock
lua gen/gen.lua render data data/po_map.tsv
cp gen/invoice.typ data/
xargs -P 16 -I{} typst compile --root data --input data=/inv/{}.json \
  data/invoice.typ data/pdf/{}.pdf < data/render.list
jq -r '["slug","url","user_field","user","password"], (.[] | [.slug,
  "http://127.0.0.1:8090/\(.slug)/login", .portal.user_field, .login.user,
  .login.password]) | @tsv' data/suppliers.json > data/creds.tsv
(cd portals && cargo build --release)
setsid nohup portals/target/release/portals data > portals.log 2>&1 < /dev/null &
bash gen/smoke.sh data
bash gen/verify.sh data [assigned-ids.txt]
```

Portals: `http://127.0.0.1:8090/<slug>/login`, then `/<slug>/invoices`
(query `page`, plus the portal's filter: `q`, `from`/`to`, `status`, or
`month`), `/<slug>/invoices/<inv-NNNN>` (card portals only link here), and
`/<slug>/download/<inv-NNNN>.pdf`. The staged challenge:
`/admin/captcha?portal=<slug>&on=1|0`, or `CAPTCHA_PORTALS=slug,slug` at
start. It is labeled as a demo and its code is plain SVG text, so it only
exercises the handoff; it is not a real CAPTCHA.
