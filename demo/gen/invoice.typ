// One invoice. Layout 1..20 picks the supplier's header, table, columns,
// font, and labels; gen.lua pre-formats every date and amount.
// typst compile --root DATA --input data=/inv/inv-0001.json invoice.typ out.pdf
#let d = json(sys.inputs.data)
#let L = d.layout
#let fonts = ("Libertinus Serif", "New Computer Modern", "DejaVu Sans Mono")
#let accents = (rgb("#7a3b1d"), rgb("#1f6feb"), rgb("#2e7d32"), rgb("#6a1b9a"), rgb("#c62828"))
#let accent = accents.at(calc.rem(L, accents.len()))
#let hstyle = calc.rem(L, 4)
#let tstyle = calc.rem(calc.quo(L, 4), 4)
#let cstyle = calc.rem(L + 1, 4)

#set page(
  paper: if calc.rem(L, 5) == 0 { "a4" } else { "us-letter" },
  margin: (x: 18mm, y: 16mm),
  footer: align(center, text(size: 7pt, fill: luma(120))[
    #d.supplier · Remit to account #(1000 + L * 37) · Questions: billing\@example.invalid
  ]),
)
#set text(font: fonts.at(calc.rem(L, 3)), size: if calc.rem(L, 3) == 2 { 8.5pt } else { 10pt })

#let date_label = ("Date", "Invoice Date", "Issued", "Bill Date").at(calc.rem(L, 4))
#let po_label = ("PO Number", "Purchase Order", "Your PO", "Order Ref").at(calc.rem(L + 2, 4))
#let address = [#(100 + L * 37) Commerce Parkway, Suite #(L * 3) \ Riverton, OH 4#(3000 + L * 11)]

#let meta = {
  let rows = ((d.label_no, d.no), (date_label, d.date), ("Due date", d.due))
  if d.po_mode == "field" { rows.push((po_label, d.po)) }
  grid(columns: 2, column-gutter: 10pt, row-gutter: 5pt,
    ..rows.map(r => (text(weight: "bold")[#r.at(0)], [#r.at(1)])).flatten())
}

#if hstyle == 0 {
  grid(columns: (1fr, auto), column-gutter: 12pt,
    [#text(size: 16pt, weight: "bold", fill: accent)[#d.supplier] \ #address],
    [#text(size: 20pt, tracking: 2pt)[INVOICE] #v(4pt) #meta])
} else if hstyle == 1 {
  align(center)[#text(size: 18pt, weight: "bold")[#d.supplier] \ #address \ #text(fill: accent, size: 12pt)[Tax Invoice]]
  v(8pt)
  meta
} else if hstyle == 2 {
  block(fill: accent, width: 100%, inset: 10pt,
    [#text(fill: white, size: 16pt, weight: "bold")[#d.supplier] #h(1fr) #text(fill: white, size: 14pt)[INVOICE]])
  address
  v(6pt)
  meta
} else {
  grid(columns: (1fr, 1fr), column-gutter: 12pt,
    meta,
    align(right)[#text(size: 14pt, weight: "bold")[#d.supplier] \ #address \ #text(fill: accent)[Invoice]])
}

#v(12pt)
#block(stroke: if hstyle == 3 { 0.5pt + accent } else { none }, inset: if hstyle == 3 { 6pt } else { 0pt })[
  *#("Bill to", "Billed to", "Customer", "Sold to").at(calc.rem(L, 4)):* #d.bill_to
]
#v(10pt)

#let cols = (
  ("Item", "Description", "Qty", "Rate", "Amount"),
  ("Description", "Qty", "Unit Price", "Line Total"),
  ("Qty", "Description", "Price", "Total"),
  ("Code", "Description", "Amount", "Qty", "Unit"),
).at(cstyle)
#let cells = d.rows.map(r => (
  (r.code, r.desc, r.qty, r.rate, r.amount),
  (r.desc, r.qty, r.rate, r.amount),
  (r.qty, r.desc, r.rate, r.amount),
  (r.code, r.desc, r.amount, r.qty, r.rate),
).at(cstyle)).flatten()

#table(
  columns: cols.map(c => if c == "Description" { 1fr } else { auto }),
  stroke: if tstyle == 0 { 0.5pt } else if tstyle == 1 { (x, y) => (bottom: 0.5pt) } else { none },
  fill: if tstyle == 2 { (x, y) => if calc.odd(y) { luma(235) } } else { none },
  inset: 6pt,
  table.header(..cols.map(c => text(weight: "bold", fill: accent)[#c])),
  ..cells.map(c => [#c]),
)

#v(8pt)
#align(if calc.rem(L, 3) == 1 { left } else { right },
  grid(columns: 2, column-gutter: 14pt, row-gutter: 5pt,
    [Subtotal], [#d.net],
    ..(if d.tax_label != "" { ([#d.tax_label], [#d.tax]) } else { () }),
    text(weight: "bold")[#("Total due", "Amount due", "Invoice total", "Balance").at(calc.rem(L, 4))],
    text(weight: "bold")[#d.total]))

#v(16pt)
#text(size: 9pt)[
  Payment terms: Net 30. Please include the invoice number with your remittance.
  #if d.po_mode == "text" [
    This invoice covers the goods shipped against your order #d.po, as
    confirmed by our sales desk.
  ]
]
