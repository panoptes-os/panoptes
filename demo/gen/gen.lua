-- Deterministic demo data for the Panoptes invoice demo.
-- usage: lua gen.lua masters OUT        suppliers.json items.json orders.jsonl
--        lua gen.lua render OUT MAP      truth.jsonl inv/*.json render.list
-- MAP is a TSV of "order key<TAB>ERPNext purchase order name" from seed.sh.
-- Every value comes from one seeded LCG, so both modes regenerate the same
-- data and render only adds the ERPNext names.
local mode, out, map_path = arg[1], arg[2], arg[3]
assert(mode == "masters" or mode == "render", "usage: gen.lua masters|render OUT [MAP]")
local SEED, N_INVOICES = 20261027, 1000

local s = SEED
local function rnd(n) -- 1..n
  s = s * 6364136223846793005 + 1442695040888963407
  return ((s >> 33) % n) + 1
end
local function pick(t) return t[rnd(#t)] end

-- JSON encoding, enough for strings, numbers, booleans, arrays, objects.
local function enc(v)
  local t = type(v)
  if t == "string" then
    return '"' .. v:gsub('[%c"\\]', function(c)
      return ({ ['"'] = '\\"', ['\\'] = '\\\\', ['\n'] = '\\n' })[c]
        or string.format("\\u%04x", c:byte())
    end) .. '"'
  elseif t == "number" then
    if math.type(v) == "integer" then return tostring(v) end
    return string.format("%.2f", v)
  elseif t == "boolean" then return tostring(v)
  elseif v[1] ~= nil or next(v) == nil and getmetatable(v) == "array" then
    local o = {}
    for i, x in ipairs(v) do o[i] = enc(x) end
    return "[" .. table.concat(o, ",") .. "]"
  else
    local keys = {}
    for k in pairs(v) do keys[#keys + 1] = k end
    table.sort(keys)
    local o = {}
    for _, k in ipairs(keys) do o[#o + 1] = enc(k) .. ":" .. enc(v[k]) end
    return "{" .. table.concat(o, ",") .. "}"
  end
end
local function write(path, text)
  local f = assert(io.open(path, "w")); f:write(text); f:close()
end

-- Money in integer cents throughout; formatting per supplier.
local function cents_str(c, thou, dec)
  local neg = c < 0; c = math.abs(c)
  local whole, frac = tostring(c // 100), string.format("%02d", c % 100)
  whole = whole:reverse():gsub("(%d%d%d)", "%1" .. thou):reverse()
  if whole:sub(1, #thou) == thou then whole = whole:sub(#thou + 1) end
  return (neg and "-" or "") .. whole .. dec .. frac
end
local MONEY = {
  function(c) return "$" .. cents_str(c, ",", ".") end,
  function(c) return "USD " .. cents_str(c, ",", ".") end,
  function(c) return cents_str(c, ",", ".") .. " USD" end,
  function(c) return "US$ " .. cents_str(c, " ", ".") end,
  function(c) return cents_str(c, "", ".") end,
}
local MONTHS = { "January", "February", "March", "April", "May", "June", "July",
  "August", "September", "October", "November", "December" }
local DATES = {
  function(y, m, d) return string.format("%04d-%02d-%02d", y, m, d) end,
  function(y, m, d) return string.format("%02d/%02d/%04d", m, d, y) end,
  function(y, m, d) return string.format("%02d/%02d/%04d", d, m, y) end,
  function(y, m, d) return string.format("%d %s %d", d, MONTHS[m]:sub(1, 3), y) end,
  function(y, m, d) return string.format("%s %d, %d", MONTHS[m], d, y) end,
  function(y, m, d) return string.format("%02d.%02d.%04d", d, m, y) end,
}
local DAYS = { 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31 }
local function add_days(y, m, d, n)
  d = d + n
  while d > DAYS[m] do d = d - DAYS[m]; m = m + 1; if m > 12 then m = 1; y = y + 1 end end
  return y, m, d
end

-- 20 fictional suppliers, each with its own invoice layout and portal theme.
-- po: "field" prints the PO in a labeled field; "text" only in a sentence.
local SUPPLIERS = {
  { "Alderbrook Office Supply", "INV-%05d", "Invoice No.", 1, 1, "field", 0 },
  { "Brightwater Fasteners", "BWF/26/%04d", "Inv #", 2, 2, "field", 600 },
  { "Cobaltline Industrial", "%06d", "Invoice Number", 3, 3, "text", 625 },
  { "Driftmoor Packaging", "DP-%04d-A", "Bill No.", 4, 1, "field", 400 },
  { "Embervale Safety Goods", "EV%05d", "Document No.", 5, 4, "field", 0 },
  { "Foxhollow Electrical", "FHE-26-%04d", "Invoice ID", 6, 2, "text", 600 },
  { "Granitepeak Tools", "GPT%04d", "Invoice", 1, 5, "field", 625 },
  { "Harborstone Logistics", "HSL-INV-%04d", "Reference", 2, 1, "field", 400 },
  { "Ironleaf Components", "IL/%04d/26", "Tax Invoice No.", 3, 3, "field", 0 },
  { "Juniperfield Janitorial", "JJ-%05d", "Invoice #", 4, 4, "text", 600 },
  { "Kestrelmark Print Works", "KPW%05d", "Invoice No", 5, 2, "field", 625 },
  { "Lumenridge Lighting", "LR-%04d", "Statement No.", 6, 1, "field", 400 },
  { "Marlowgate Hydraulics", "MGH-%05d", "Invoice Ref", 1, 3, "field", 0 },
  { "Nettlecombe Lab Supply", "NLS/%05d", "Invoice Number", 2, 5, "text", 600 },
  { "Orchardine Food Service", "OFS-%04d", "Bill #", 3, 4, "field", 625 },
  { "Pinecrest Plastics", "PCP%04d-26", "Invoice", 4, 1, "field", 400 },
  { "Quarrybank Abrasives", "QB-%05d", "Inv. No.", 5, 2, "field", 0 },
  { "Redfern Cable & Wire", "RCW-%04d", "Invoice No.", 6, 3, "field", 600 },
  { "Saltmarsh Marine Hardware", "SMH/26/%05d", "Document #", 1, 4, "field", 625 },
  { "Thistledown Textiles", "TT-%04d", "Invoice ID", 2, 5, "field", 400 },
}
local TAX_TEMPLATE = { [0] = "", [400] = "US ST 4% - PDC", [600] = "US ST 6% - PDC",
  [625] = "US ST 6.25% - PDC" }
local THEMES = { -- portal look: background, text, accent, font, list style
  { "#f7f4ee", "#2b2b2b", "#7a3b1d", "Georgia, serif", "table" },
  { "#0f1720", "#e6edf3", "#3fb950", "system-ui, sans-serif", "cards" },
  { "#ffffff", "#111111", "#0057b8", "Arial, sans-serif", "table" },
  { "#fff8e7", "#3a2e1f", "#c26a00", "Verdana, sans-serif", "list" },
  { "#eef3f8", "#1c2a39", "#5b2a86", "Tahoma, sans-serif", "table" },
}
local PAGE_SIZES = { 5, 10, 15, 25 }
local FILTERS = { "search", "daterange", "status", "month", "search" }
local LIST_LABELS = { "Invoices", "Billing documents", "Statements", "Your bills",
  "Documents" }

local ITEMS = {
  "A4 Copy Paper, 500 sheets", "Nitrile Gloves, box of 100", "M8 Hex Bolt, bag of 50",
  "Stretch Wrap Film, 18 in", "Safety Glasses, clear", "LED Panel Light, 2x4 ft",
  "Cordless Drill Battery 18V", "Pallet Labels, roll of 500", "Hydraulic Hose 1/2 in, per ft",
  "Cat6 Patch Cable, 10 ft", "Floor Cleaner Concentrate, 5 L", "Toner Cartridge, black",
  "Corrugated Box 12x12x12", "Hi-Vis Vest, size L", "Sanding Discs P120, pack of 25",
  "Stainless Washer M6, bag of 100", "Shrink Tubing Kit", "Lab Pipette Tips, rack of 96",
  "Disposable Coffee Cups, sleeve of 50", "Marine Shackle 3/8 in", "Cotton Shop Towels, pack of 50",
  "Zip Ties 8 in, bag of 100", "Whiteboard Markers, set of 4", "Ear Plugs, box of 200",
  "Packing Tape, 6 rolls", "Fluorescent Tube T8, 4 ft", "Hex Key Set, metric",
  "Thermal Receipt Paper, 50 rolls", "Copper Wire 12 AWG, 100 ft", "Hand Sanitizer, 1 L",
  "Bubble Mailers, pack of 25", "Industrial Fan 24 in", "Drum Liner 55 gal, case",
  "Torque Wrench 3/8 in", "Label Printer Ribbon", "Steel Shelving Unit",
  "Microfiber Cloths, pack of 12", "PVC Pipe 2 in, 10 ft", "First Aid Kit, 50 person",
  "Extension Cord 25 ft",
}

-- Masters: items with fixed prices, suppliers with item ranges.
local items = {}
for i, name in ipairs(ITEMS) do
  items[i] = { code = string.format("DEMO-ITM-%03d", i), name = name,
    rate = (rnd(45000) + 199) } -- cents, 2.00 to 452.00
end
local suppliers = {}
for i, row in ipairs(SUPPLIERS) do
  local name, nofmt, label, datef, moneyf, po, tax = table.unpack(row)
  local th = THEMES[(i - 1) % #THEMES + 1]
  local sold = {}
  for k = 0, 7 do sold[#sold + 1] = ((i * 3 + k * 5) % #items) + 1 end
  local slug = name:lower():gsub("[^%w]+", "-"):gsub("-+$", ""):match("^[^-]+")
  suppliers[i] = {
    id = i, name = name, slug = slug, no_format = nofmt, label_no = label,
    date_format = datef, money_format = moneyf, po_mode = po, tax_bp = tax,
    tax_template = TAX_TEMPLATE[tax], items = sold, layout = i,
    portal = { bg = th[1], fg = th[2], accent = th[3], font = th[4], list = th[5],
      page_size = PAGE_SIZES[(i - 1) % #PAGE_SIZES + 1],
      filter = FILTERS[(i - 1) % #FILTERS + 1],
      list_label = LIST_LABELS[(i - 1) % #LIST_LABELS + 1],
      user_field = ({ "username", "email", "account" })[(i - 1) % 3 + 1] },
    login = { user = slug .. "-ap", password = string.format("demo-%s-%04d", slug, rnd(9999)) },
  }
end

-- Orders: one purchase order per invoice, 50 per supplier.
local orders = {}
for n = 1, N_INVOICES do
  local sup = suppliers[(n - 1) % #suppliers + 1]
  local m = 8 + rnd(3) - 1 -- Aug, Sep, Oct 2026
  local d = rnd(m == 10 and 12 or DAYS[m])
  local lines, used = {}, {}
  for _ = 1, rnd(4) do
    local it = items[sup.items[rnd(#sup.items)]]
    if not used[it.code] then
      used[it.code] = true
      lines[#lines + 1] = { item_code = it.code, item_name = it.name,
        qty = rnd(40), rate_c = it.rate }
    end
  end
  local iy, im, id = add_days(2026, m, d, rnd(12) + 2)
  orders[n] = { key = string.format("DEMO-PO-%04d", n), n = n, supplier = sup,
    y = 2026, m = m, d = d, iy = iy, im = im, id = id, lines = lines,
    seq = (n - 1) // #suppliers + 101 + sup.id * 7, paid = rnd(3) == 1 }
end

local function iso(y, m, d) return string.format("%04d-%02d-%02d", y, m, d) end

if mode == "masters" then
  os.execute("mkdir -p " .. out)
  local si = {}
  for i, sp in ipairs(suppliers) do
    si[i] = { id = sp.id, name = sp.name, slug = sp.slug, tax_template = sp.tax_template,
      portal = sp.portal, login = sp.login, layout = sp.layout }
  end
  write(out .. "/suppliers.json", enc(si) .. "\n")
  local it = {}
  for i, x in ipairs(items) do it[i] = { code = x.code, name = x.name, rate = x.rate / 100.0 } end
  write(out .. "/items.json", enc(it) .. "\n")
  local o = {}
  for _, od in ipairs(orders) do
    local ls = {}
    for i, l in ipairs(od.lines) do ls[i] = { item_code = l.item_code, qty = l.qty, rate = l.rate_c / 100.0 } end
    o[#o + 1] = enc({ key = od.key, supplier = od.supplier.name, date = iso(od.y, od.m, od.d),
      schedule_date = iso(od.iy, od.im, od.id), tax_template = od.supplier.tax_template, lines = ls })
  end
  write(out .. "/orders.jsonl", table.concat(o, "\n") .. "\n")
  print(string.format("masters: %d suppliers, %d items, %d orders", #suppliers, #items, #orders))
  return
end

-- Render: join ERPNext PO names, compute totals, write truth and typst inputs.
local po = {}
for line in assert(io.open(map_path)):lines() do
  local k, v = line:match("^(%S+)\t(%S+)$")
  if k then po[k] = v end
end
os.execute("mkdir -p " .. out .. "/inv " .. out .. "/pdf")
local truth, jobs = {}, {}
for _, od in ipairs(orders) do
  local sp = od.supplier
  local erp_po = assert(po[od.key], "no ERPNext PO for " .. od.key)
  local money, date = MONEY[sp.money_format], DATES[sp.date_format]
  local net, rows, tl = 0, {}, {}
  for i, l in ipairs(od.lines) do
    local amt = l.qty * l.rate_c
    net = net + amt
    rows[i] = { desc = l.item_name, code = l.item_code, qty = tostring(l.qty),
      rate = money(l.rate_c), amount = money(amt) }
    tl[i] = { item_code = l.item_code, qty = l.qty, rate = l.rate_c / 100.0 }
  end
  -- Tax rounds half to even at the cent, as ERPNext does: 2306.96 at 6.25%
  -- is 144.185, which ERPNext stores as 144.18.
  local tax, rem = (net * sp.tax_bp) // 10000, (net * sp.tax_bp) % 10000
  if rem > 5000 or rem == 5000 and tax % 2 == 1 then tax = tax + 1 end
  local no = string.format(sp.no_format, od.seq)
  local file = string.format("inv-%04d", od.n)
  local due_y, due_m, due_d = add_days(od.iy, od.im, od.id, 30)
  truth[#truth + 1] = enc({ invoice_no = no, file = file, supplier = sp.name, slug = sp.slug,
    date = iso(od.iy, od.im, od.id), po = erp_po, po_key = od.key, lines = tl,
    net = net / 100.0, tax_rate = sp.tax_bp / 100.0, tax = tax / 100.0,
    total = (net + tax) / 100.0, tax_template = sp.tax_template,
    status = od.paid and "Paid" or "Open",
    display = { date = date(od.iy, od.im, od.id), total = money(net + tax),
      due = date(due_y, due_m, due_d) } })
  write(out .. "/inv/" .. file .. ".json", enc({
    layout = sp.layout, supplier = sp.name, label_no = sp.label_no, no = no,
    date = date(od.iy, od.im, od.id), due = date(due_y, due_m, due_d), po = erp_po,
    po_mode = sp.po_mode, rows = rows, net = money(net),
    tax_label = sp.tax_bp == 0 and "" or string.format("Sales tax %s%%",
      (sp.tax_bp % 100 == 0) and tostring(sp.tax_bp // 100) or string.format("%.2f", sp.tax_bp / 100)),
    tax = money(tax), total = money(net + tax),
    bill_to = "Panoptes Demo Co, Accounts Payable, 100 Example Way, Springfield, IL 62701",
  }) .. "\n")
  jobs[#jobs + 1] = file
end
write(out .. "/truth.jsonl", table.concat(truth, "\n") .. "\n")
write(out .. "/render.list", table.concat(jobs, "\n") .. "\n")
print(string.format("render: %d invoices", #truth))
