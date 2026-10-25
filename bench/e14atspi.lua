-- Walk an app's AT-SPI tree over gdbus and print its size and roles.
-- usage: lua e14atspi.lua SESSION_BUS_ADDRESS [MAX_NODES]
local session = assert(arg[1], "usage: e14atspi.lua SESSION_BUS_ADDRESS [MAX]")
local max = tonumber(arg[2] or "3000")

local function sh(cmd)
  local f = io.popen(cmd .. " 2>/dev/null")
  local s = f:read("a")
  f:close()
  return s
end

local function q(s) return "'" .. s:gsub("'", "'\\''") .. "'" end

local a11y = sh("gdbus call --address " .. q(session) ..
  " --dest org.a11y.Bus --object-path /org/a11y/bus" ..
  " --method org.a11y.Bus.GetAddress"):match("%('([^']+)'")
if not a11y then print("atspi=none reason=no-a11y-bus") return end

local function call(dest, path, method)
  return sh("timeout 5 gdbus call --address " .. q(a11y) .. " --dest " ..
    q(dest) .. " --object-path " .. q(path) ..
    " --method org.a11y.atspi.Accessible." .. method)
end

local function children(dest, path)
  local out = {}
  for d, p in call(dest, path, "GetChildren"):gmatch("%('([^']+)', ([^%)]+)%)") do
    p = p:gsub("^objectpath ", ""):gsub("'", "")
    out[#out + 1] = {d, p}
  end
  return out
end

local apps = children("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root")
local nodes, depth, roles, names = 0, 0, {}, {}
local function walk(d, p, lvl)
  if nodes >= max then return end
  nodes = nodes + 1
  if lvl > depth then depth = lvl end
  local role = call(d, p, "GetRoleName"):match("%('([^']*)'") or "?"
  roles[role] = (roles[role] or 0) + 1
  for _, c in ipairs(children(d, p)) do walk(c[1], c[2], lvl + 1) end
end
for _, a in ipairs(apps) do
  local n = sh("timeout 5 gdbus call --address " .. q(a11y) .. " --dest " ..
    q(a[1]) .. " --object-path " .. q(a[2]) ..
    " --method org.freedesktop.DBus.Properties.Get org.a11y.atspi.Accessible Name")
  names[#names + 1] = (n:match("<'([^']*)'>") or "?")
  walk(a[1], a[2], 0)
end
local rl = {}
for r, c in pairs(roles) do rl[#rl + 1] = {r, c} end
table.sort(rl, function(x, y) return x[2] > y[2] end)
local top = {}
for i = 1, math.min(8, #rl) do top[i] = rl[i][1]:gsub(" ", "_") .. ":" .. rl[i][2] end
print(string.format("atspi_apps=%d names=%s nodes=%d%s depth=%d roles=%s",
  #apps, table.concat(names, ","):gsub(" ", "_"), nodes,
  nodes >= max and "+" or "", depth, table.concat(top, ",")))
