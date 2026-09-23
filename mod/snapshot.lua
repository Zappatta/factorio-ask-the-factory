local M = {}

local CLUSTER_CELL = 48
local MAX_RECIPE_GROUPS = 45
local MAX_CLUSTERS_PER_GROUP = 6
local MAX_ALERT_EXAMPLES = 4

-- Fraction of the group budget handed out purely by machine count. The rest goes to the
-- groups with the worst non-working fraction, so a starved four-machine block is not
-- pushed off the list by forty healthy smelters.
local GROUPS_BY_SIZE = 0.7
local MAX_FEED_ENTRIES = 4

-- Local view. Radius is deliberately small: a dense block is 15-33 tiles across, and
-- r=48 would quadruple the token cost for ground nobody asked about.
local LOCAL_RADIUS = 28
local MAX_LOCAL_MACHINES = 30
local MAX_LOCAL_INSERTERS = 60
local MAX_LOCAL_BELT_RUNS = 20
local MAX_LOCAL_CONTAINERS = 20
local MAX_ITEMS_LISTED = 3

local MACHINE_TYPES = {"assembling-machine", "furnace", "rocket-silo", "mining-drill", "lab"}
local CONTAINER_TYPES = {"container", "logistic-container"}

-- Statuses where the machine is doing its job or is merely backed up. Anything else
-- counts as troubled for the purposes of ranking recipe groups.
local OK_STATUS = {
  working = true, normal = true, full_output = true,
  waiting_for_space_in_destination = true, waiting_to_launch_rocket = true,
  launching_rocket = true, preparing_rocket_for_launch = true,
}

-- The three statuses that mean "an ingredient is missing", which is the only case where
-- diffing the recipe against the input inventory tells us anything.
local SHORTAGE_STATUS = {
  item_ingredient_shortage = true,
  fluid_ingredient_shortage = true,
  missing_science_packs = true,
}

local CENSUS_TYPES = {
  "transport-belt", "underground-belt", "splitter", "inserter", "assembling-machine",
  "furnace", "mining-drill", "electric-pole", "pipe", "pipe-to-ground", "pump",
  "lab", "roboport", "logistic-robot", "construction-robot", "container",
  "logistic-container", "solar-panel", "accumulator", "boiler", "generator",
  "reactor", "heat-pipe", "radar", "wall", "gun-turret", "laser-turret",
  "train-stop", "locomotive", "cargo-wagon", "fluid-wagon", "beacon", "rocket-silo",
}

-- Error tally. pcall cannot tell "nothing there" from "the API broke", so failures are
-- counted by section and surfaced in meta rather than swallowed.
local errors = {}
local section = "?"

local function note_error(label)
  errors[label or section] = (errors[label or section] or 0) + 1
end

-- For deliberate API probing, where a failure is the expected way to learn the game
-- version. Never counted.
local function try(fn, default)
  local ok, res = pcall(fn)
  if ok and res ~= nil then return res end
  return default
end

local function safe(fn, default, label)
  local ok, res = pcall(fn)
  if not ok then
    note_error(label)
    return default
  end
  if res ~= nil then return res end
  return default
end

-- Integers only: Factorio's JSON encoder prints full float precision, so 4683.8
-- would serialise as 4683.80000000000018. Anything needing decimals uses fmt().
local function round(n)
  if type(n) ~= "number" then return 0 end
  return math.floor(n + 0.5)
end

local function fmt(n, places)
  if type(n) ~= "number" then return nil end
  return string.format("%." .. (places or 2) .. "f", n)
end

local function bump(t, key, by)
  if not key then return end
  t[key] = (t[key] or 0) + (by or 1)
end

local function top_entries(map, limit)
  local pairs_list = {}
  for k, v in pairs(map or {}) do pairs_list[#pairs_list + 1] = {k, v} end
  if #pairs_list == 0 then return nil end
  table.sort(pairs_list, function(a, b)
    if a[2] == b[2] then return a[1] < b[1] end
    return a[2] > b[2]
  end)
  local out = {}
  for i = 1, math.min(#pairs_list, limit) do out[pairs_list[i][1]] = pairs_list[i][2] end
  return out
end

-- "iron-plate 400, copper-plate 12". One string rather than a map: item names are
-- hyphenated and repeat per row, and a count with no name is useless anyway.
local function items_line(map, limit)
  local list = {}
  for k, v in pairs(map or {}) do list[#list + 1] = {k, v} end
  if #list == 0 then return "empty" end
  table.sort(list, function(a, b)
    if a[2] == b[2] then return a[1] < b[1] end
    return a[2] > b[2]
  end)
  local parts = {}
  for i = 1, math.min(#list, limit or MAX_ITEMS_LISTED) do
    parts[#parts + 1] = list[i][1] .. " " .. round(list[i][2])
  end
  if #list > (limit or MAX_ITEMS_LISTED) then parts[#parts + 1] = "+" .. (#list - (limit or MAX_ITEMS_LISTED)) .. " more" end
  return table.concat(parts, ", ")
end

-- 2.0 returns an array of {name, count, quality}; 1.1 returned name -> count.
local function contents_map(raw)
  local out = {}
  for k, v in pairs(raw or {}) do
    if type(v) == "table" and v.name then
      out[v.name] = (out[v.name] or 0) + (v.count or 0)
    else
      out[k] = v
    end
  end
  return out
end

local function item_prototypes()
  if prototypes and prototypes.item then return prototypes.item end
  return game.item_prototypes
end

local function fluid_prototypes()
  if prototypes and prototypes.fluid then return prototypes.fluid end
  return game.fluid_prototypes
end

local function item_stats(force, surface)
  return try(function() return force.get_item_production_statistics(surface) end)
      or try(function() return force.item_production_statistics end)
end

local function fluid_stats(force, surface)
  return try(function() return force.get_fluid_production_statistics(surface) end)
      or try(function() return force.fluid_production_statistics end)
end

local function precision(name)
  local p = defines.flow_precision_index
  return p and p[name] or nil
end

-- Handles both the 2.0 `category` form and the 1.1 `input` boolean form.
local function flow(stats, name, category, prec, as_count)
  if not stats or not prec then return 0 end
  if as_count == nil then as_count = true end
  local v = try(function()
    return stats.get_flow_count{name = name, category = category,
                                precision_index = prec, count = as_count}
  end)
  if v then return v end
  return try(function()
    return stats.get_flow_count{name = name, input = (category == "input"),
                                precision_index = prec, count = as_count}
  end, 0)
end

local function stat_names(stats)
  local names = {}
  if not stats then return names end
  for _, prop in pairs({"output_counts", "input_counts"}) do
    local t = try(function() return stats[prop] end)
    if t then for name in pairs(t) do names[name] = true end end
  end
  if next(names) then return names end
  for _, getter in pairs({"get_output_counts", "get_input_counts"}) do
    local t = try(function() return stats[getter](stats) end)
    if t then for name in pairs(t) do names[name] = true end end
  end
  return names
end

local function collect_flows(stats, protos, limit)
  local out = {}
  if not stats then return out end
  local minute, hour = precision("one_minute"), precision("one_hour")
  for name in pairs(protos) do
    local made_h = flow(stats, name, "output", hour)
    local used_h = flow(stats, name, "input", hour)
    if made_h > 0 or used_h > 0 then
      local made_m = flow(stats, name, "output", minute)
      local used_m = flow(stats, name, "input", minute)
      out[#out + 1] = {
        name = name,
        made_last_min = round(made_m),
        used_last_min = round(used_m),
        net_last_min = round(made_m - used_m),
        made_last_hour = round(made_h),
        used_last_hour = round(used_h),
        lifetime_made = round(safe(function() return stats.get_output_count(name) end, 0, "production")),
        _rank = math.max(made_h, used_h),
      }
    end
  end
  -- Rank on throughput in either direction. Sorting on production alone buried items
  -- that are only consumed - logistic robots, ammo, fuel - at the bottom of the list,
  -- exactly where the cap drops them.
  table.sort(out, function(a, b)
    if a._rank == b._rank then return a.name < b.name end
    return a._rank > b._rank
  end)
  local kept = {}
  for i = 1, math.min(#out, limit or #out) do
    out[i]._rank = nil
    kept[i] = out[i]
  end
  return kept
end

-- Grid-buckets positions then merges 8-connected buckets into blobs.
local function cluster_positions(positions)
  local buckets = {}
  for _, p in pairs(positions) do
    local kx, ky = math.floor(p.x / CLUSTER_CELL), math.floor(p.y / CLUSTER_CELL)
    local key = kx .. ":" .. ky
    local b = buckets[key]
    if not b then
      b = {kx = kx, ky = ky, n = 0, sx = 0, sy = 0,
           minx = p.x, maxx = p.x, miny = p.y, maxy = p.y}
      buckets[key] = b
    end
    b.n, b.sx, b.sy = b.n + 1, b.sx + p.x, b.sy + p.y
    if p.x < b.minx then b.minx = p.x end
    if p.x > b.maxx then b.maxx = p.x end
    if p.y < b.miny then b.miny = p.y end
    if p.y > b.maxy then b.maxy = p.y end
  end

  local seen, clusters = {}, {}
  for key, start in pairs(buckets) do
    if not seen[key] then
      seen[key] = true
      local stack = {start}
      local c = {n = 0, sx = 0, sy = 0,
                 minx = start.minx, maxx = start.maxx, miny = start.miny, maxy = start.maxy}
      while #stack > 0 do
        local cur = table.remove(stack)
        c.n, c.sx, c.sy = c.n + cur.n, c.sx + cur.sx, c.sy + cur.sy
        if cur.minx < c.minx then c.minx = cur.minx end
        if cur.maxx > c.maxx then c.maxx = cur.maxx end
        if cur.miny < c.miny then c.miny = cur.miny end
        if cur.maxy > c.maxy then c.maxy = cur.maxy end
        for dx = -1, 1 do
          for dy = -1, 1 do
            local nk = (cur.kx + dx) .. ":" .. (cur.ky + dy)
            if buckets[nk] and not seen[nk] then
              seen[nk] = true
              stack[#stack + 1] = buckets[nk]
            end
          end
        end
      end
      clusters[#clusters + 1] = {
        count = c.n,
        x = round(c.sx / c.n),
        y = round(c.sy / c.n),
        bbox = {round(c.minx), round(c.miny), round(c.maxx), round(c.maxy)},
      }
    end
  end
  table.sort(clusters, function(a, b) return a.count > b.count end)
  return clusters
end

local function status_lookup()
  local names = {}
  for k, v in pairs(defines.entity_status) do names[v] = k end
  return names
end

-- get_recipe() raises on labs and mining drills rather than returning nil, and the
-- error tally was the only reason anyone noticed.
local function machine_recipe(e)
  if e.type == "lab" or e.type == "mining-drill" then return nil end
  return safe(function()
    local r = e.get_recipe()
    return r and r.name or nil
  end, nil, "machines")
end

-- What a stalled machine is actually waiting for. Only called for the three shortage
-- statuses, so the cost is bounded by how much is already broken.
local function missing_ingredients(e, research_ings)
  local want = {}
  if e.type == "lab" then
    for _, ing in pairs(research_ings or {}) do
      want[#want + 1] = {type = "item", name = ing.name, amount = ing.amount or 1}
    end
  else
    local recipe = safe(function() return e.get_recipe() end, nil, "shortage")
    if not recipe then return nil end
    for _, ing in pairs(safe(function() return recipe.ingredients end, {}, "shortage")) do
      want[#want + 1] = ing
    end
  end
  if #want == 0 then return nil end

  local inv = safe(function()
    if e.type == "lab" then return e.get_inventory(defines.inventory.lab_input) end
    if e.type == "furnace" then return e.get_inventory(defines.inventory.furnace_source) end
    return e.get_inventory(defines.inventory.assembling_machine_input)
  end, nil, "shortage")
  local have = inv and contents_map(safe(function() return inv.get_contents() end, {}, "shortage")) or {}

  local fluids = {}
  local boxes = safe(function() return #e.fluidbox end, 0, "shortage")
  for i = 1, boxes do
    local fb = safe(function() return e.fluidbox[i] end, nil, "shortage")
    if fb and fb.name then fluids[fb.name] = (fluids[fb.name] or 0) + (fb.amount or 0) end
  end

  local short = {}
  for _, ing in pairs(want) do
    local need = ing.amount or 1
    if ing.type == "fluid" then
      if (fluids[ing.name] or 0) < need then short[#short + 1] = ing.name end
    else
      if (have[ing.name] or 0) < need then short[#short + 1] = ing.name end
    end
  end
  if #short == 0 then return nil end
  table.sort(short)
  return short
end

local function collect_machines(force, surface, full)
  local ents = safe(function()
    return surface.find_entities_filtered{type = MACHINE_TYPES, force = force}
  end, {}, "machines")

  local research_ings = safe(function()
    local cr = force.current_research
    return cr and cr.research_unit_ingredients or nil
  end, nil, "machines")

  local status_names = status_lookup()
  local groups, order = {}, {}
  local totals = {}
  local by_unit = {}

  for _, e in pairs(ents) do
    local recipe = machine_recipe(e)
    if not recipe and e.type == "mining-drill" then
      local ore = safe(function() return e.mining_target and e.mining_target.name or nil end,
                       nil, "machines")
      recipe = ore and ("mining " .. ore) or nil
    end
    local key = recipe or ("(" .. e.name .. ")")
    local g = groups[key]
    if not g then
      g = {produces = key, is_recipe = recipe ~= nil, count = 0, troubled = 0,
           machines = {}, status = {}, positions = {},
           short_on = {}, fed_by = {}, outputs_to = {}, inserters = {}}
      groups[key] = g
      order[#order + 1] = g
    end
    g.count = g.count + 1
    bump(g.machines, e.name)

    local sname = status_names[e.status] or "unknown"
    bump(g.status, sname)
    bump(totals, sname)
    if not OK_STATUS[sname] then g.troubled = g.troubled + 1 end

    if SHORTAGE_STATUS[sname] then
      for _, name in pairs(missing_ingredients(e, research_ings) or {}) do
        bump(g.short_on, name)
      end
    end

    local un = e.unit_number
    if un then by_unit[un] = g end

    if full then g.positions[#g.positions + 1] = e.position end
  end

  return {order = order, by_unit = by_unit, total = #ents, status_totals = totals}
end

-- One sweep of every inserter on the surface, bucketed back to the recipe group of the
-- machine each end touches. This is what answers "is this block belt-fed or bot-fed"
-- for the whole base without needing a focus point.
local function collect_feeds(force, surface, by_unit)
  local ok, ins = pcall(function()
    return surface.find_entities_filtered{type = "inserter", force = force}
  end)
  if not ok then
    note_error("feeds")
    return 0
  end

  for _, e in pairs(ins) do
    local fine, err = pcall(function()
      local pick = e.pickup_target
      local drop = e.drop_target
      local pick_name = pick and pick.name or "(ground)"
      local drop_name = drop and drop.name or "(ground)"

      local into = drop and drop.unit_number and by_unit[drop.unit_number]
      if into then
        bump(into.fed_by, pick_name)
        bump(into.inserters, e.name)
      end

      local outof = pick and pick.unit_number and by_unit[pick.unit_number]
      if outof then
        bump(outof.outputs_to, drop_name)
        bump(outof.inserters, e.name)
      end
    end)
    if not fine then note_error("feeds") end
  end
  return #ins
end

-- Machine count alone dropped small struggling groups off the end of the list, which are
-- exactly the ones worth reporting. Hand most of the budget out by size, the rest by how
-- much of the group is stuck.
local function select_groups(order, limit)
  table.sort(order, function(a, b)
    if a.count == b.count then return a.produces < b.produces end
    return a.count > b.count
  end)
  if #order <= limit then return order end

  local by_size = math.floor(limit * GROUPS_BY_SIZE)
  local kept, rest = {}, {}
  for i, g in ipairs(order) do
    if i <= by_size then kept[#kept + 1] = g else rest[#rest + 1] = g end
  end
  table.sort(rest, function(a, b)
    local fa, fb = a.troubled / a.count, b.troubled / b.count
    if fa == fb then return a.count > b.count end
    return fa > fb
  end)
  for i = 1, math.min(#rest, limit - #kept) do kept[#kept + 1] = rest[i] end
  table.sort(kept, function(a, b)
    if a.count == b.count then return a.produces < b.produces end
    return a.count > b.count
  end)
  return kept
end

local function machines_section(collected, full)
  local kept = select_groups(collected.order, MAX_RECIPE_GROUPS)
  local out = {}
  for _, g in ipairs(kept) do
    local entry = {produces = g.produces, machine_count = g.count,
                   machines = g.machines, status = g.status}
    if next(g.short_on) then entry.short_on = g.short_on end
    if full then
      if next(g.fed_by) then entry.fed_by = top_entries(g.fed_by, MAX_FEED_ENTRIES) end
      if next(g.outputs_to) then entry.outputs_to = top_entries(g.outputs_to, MAX_FEED_ENTRIES) end
      if next(g.inserters) then entry.inserters = top_entries(g.inserters, MAX_FEED_ENTRIES) end
    end
    if full and #g.positions > 0 then
      local clusters = cluster_positions(g.positions)
      local keep = {}
      for j = 1, math.min(#clusters, MAX_CLUSTERS_PER_GROUP) do keep[j] = clusters[j] end
      entry.locations = keep
    end
    out[#out + 1] = entry
  end
  return {
    groups = out,
    total_machines = collected.total,
    status_totals = collected.status_totals,
    groups_shown = #out,
    groups_total = #collected.order,
    note = "short_on counts machines of that group missing each ingredient right now. "
        .. "fed_by / outputs_to are what the inserters on this group pick up from and "
        .. "drop into, so they say whether the block is belt-fed or bot-fed.",
  }
end
local function collect_power(force, surface)
  local poles = safe(function()
    return surface.find_entities_filtered{type = "electric-pole", force = force, limit = 5000}
  end, {})
  if #poles == 0 then return {note = "no electric poles found"} end

  local counts, sample = {}, {}
  for _, p in pairs(poles) do
    local id = safe(function() return p.electric_network_id end)
    if id then
      counts[id] = (counts[id] or 0) + 1
      sample[id] = sample[id] or p
    end
  end

  local best, best_n = nil, 0
  for id, n in pairs(counts) do
    if n > best_n then best, best_n = id, n end
  end
  if not best then return {note = "no electric network found"} end

  local stats = safe(function() return sample[best].electric_network_statistics end)
  local minute = precision("one_minute")
  local generation, consumption = {}, {}
  local gen_total, con_total = 0, 0

  -- Electric flow is J/tick and only readable with count=false; kW = J/tick * 60 / 1000.
  if stats then
    for name in pairs(stat_names(stats)) do
      local out_kw = round(flow(stats, name, "output", minute, false) * 60 / 1000)
      local in_kw = round(flow(stats, name, "input", minute, false) * 60 / 1000)
      if out_kw > 0 then
        generation[name] = out_kw
        gen_total = gen_total + out_kw
      end
      if in_kw > 0 then
        consumption[name] = in_kw
        con_total = con_total + in_kw
      end
    end
  end

  -- Generation tracks demand exactly unless the base is browning out, so installed
  -- capacity is the number that actually answers "do I have headroom".
  local capacity, cap_total = {}, 0
  for name in pairs(generation) do
    local max_out = safe(function()
      return prototypes.entity[name].get_max_energy_production()
    end, 0)
    if max_out and max_out > 0 then
      local n = safe(function()
        return surface.count_entities_filtered{name = name, force = force}
      end, 0)
      local kw = round(n * max_out * 60 / 1000)
      if kw > 0 then
        capacity[name] = kw
        cap_total = cap_total + kw
      end
    end
  end

  local accs = safe(function()
    return surface.find_entities_filtered{type = "accumulator", force = force, limit = 2000}
  end, {})
  local charge, cap = 0, 0
  for _, a in pairs(accs) do
    charge = charge + safe(function() return a.energy end, 0)
    cap = cap + safe(function()
      return a.prototype.electric_energy_source_prototype.buffer_capacity
    end, 0)
  end

  return {
    networks = table_size(counts),
    largest_network_poles = best_n,
    generation_kw = generation,
    consumption_kw = consumption,
    capacity_kw = capacity,
    total_generation_kw = round(gen_total),
    total_consumption_kw = round(con_total),
    total_capacity_kw = round(cap_total),
    headroom_kw = round(cap_total - con_total),
    load_pct = cap_total > 0 and round(con_total / cap_total * 100) or nil,
    accumulators = #accs,
    accumulator_charge_pct = cap > 0 and round(charge / cap * 100) or nil,
    units = "kW over the last minute; 1000 kW = 1 MW. generation always equals "
         .. "consumption unless browning out - compare consumption against capacity "
         .. "for real headroom. Solar capacity is the daytime peak.",
  }
end

local function collect_research(force)
  local current = safe(function() return force.current_research end)
  local done = 0
  for _, t in pairs(safe(function() return force.technologies end, {})) do
    if t.researched then done = done + 1 end
  end
  local queue = {}
  for _, t in pairs(safe(function() return force.research_queue end, {})) do
    queue[#queue + 1] = t.name
  end
  return {
    current = current and current.name or nil,
    current_level = current and safe(function() return current.level end) or nil,
    progress_pct = current and round(safe(function() return force.research_progress end, 0) * 100) or nil,
    researched_count = done,
    queue = queue,
  }
end

local function collect_alerts(player)
  local raw = safe(function() return player.get_alerts{} end, {})
  local names = {}
  for k, v in pairs(defines.alert_type) do names[v] = k end

  local counts, examples = {}, {}
  for _, by_type in pairs(raw) do
    for atype, list in pairs(by_type) do
      local label = names[atype] or tostring(atype)
      if #list == 0 then goto continue end
      counts[label] = (counts[label] or 0) + #list
      examples[label] = examples[label] or {}
      for _, a in pairs(list) do
        if #examples[label] < MAX_ALERT_EXAMPLES then
          local pos = try(function() return a.target.position end)
                   or try(function() return a.position end)
          examples[label][#examples[label] + 1] = {
            entity = try(function() return a.target.name end),
            x = pos and round(pos.x) or nil,
            y = pos and round(pos.y) or nil,
          }
        end
      end
      ::continue::
    end
  end
  return {counts = counts, examples = examples}
end

local PLACEABLE_HINTS = {
  "inserter", "fast-inserter", "long-handed-inserter", "bulk-inserter",
  "transport-belt", "fast-transport-belt", "express-transport-belt",
  "underground-belt", "splitter", "pipe", "pipe-to-ground", "medium-electric-pole",
  "small-electric-pole", "big-electric-pole", "substation", "steel-chest",
  "iron-chest", "wooden-chest", "passive-provider-chest", "active-provider-chest",
  "requester-chest", "buffer-chest", "storage-chest",
}

-- Footprints, so placement arithmetic is exact rather than remembered. Entities are
-- positioned on their centre, so two 3x3 machines must be 3 tiles apart.
local function collect_footprints(force, surface, groups)
  local names = {}
  for _, hint in pairs(PLACEABLE_HINTS) do names[hint] = true end
  for _, group in pairs(groups or {}) do
    for machine in pairs(group.machines or {}) do names[machine] = true end
  end

  local out = {}
  for name in pairs(names) do
    local proto = safe(function() return prototypes.entity[name] end)
    if proto then
      local w = safe(function() return proto.tile_width end, 1)
      local h = safe(function() return proto.tile_height end, 1)
      out[name] = w .. "x" .. h
    end
  end
  return out
end

local function collect_census(force, surface)
  local census = {}
  for _, t in pairs(CENSUS_TYPES) do
    local n = safe(function()
      return surface.count_entities_filtered{type = t, force = force}
    end, 0)
    if n > 0 then census[t] = n end
  end
  return census
end

local function collect_logistics(force, surface)
  local trains = safe(function()
    return game.train_manager.get_trains{surface = surface, force = force}
  end, {})

  local state_names = {}
  for k, v in pairs(defines.train_state) do state_names[v] = k end

  local states, at_station = {}, {}
  for _, tr in pairs(trains) do
    local sname = state_names[safe(function() return tr.state end)] or "unknown"
    states[sname] = (states[sname] or 0) + 1
    local stop = safe(function() return tr.station and tr.station.backer_name or nil end)
    if stop then at_station[stop] = (at_station[stop] or 0) + 1 end
  end

  local stops = safe(function()
    return surface.find_entities_filtered{type = "train-stop", force = force, limit = 500}
  end, {})
  local stop_names = {}
  for _, st in pairs(stops) do
    local n = safe(function() return st.backer_name end, "?")
    stop_names[n] = (stop_names[n] or 0) + 1
  end

  return {
    train_count = #trains,
    train_states = states,
    trains_waiting_at = at_station,
    station_count = #stops,
    stations = stop_names,
    note = "train_states worth acting on: destination_full (the drop-off is backed up), "
        .. "no_path (broken or blocked rail), no_schedule (idle train).",
  }
end

local function inventory_contents(player)
  local inv = safe(function() return player.get_main_inventory() end)
  if not inv then return {} end
  local raw = safe(function() return inv.get_contents() end, {})
  local out = {}
  -- 2.0 returns an array of {name, count, quality}; 1.1 returns name -> count.
  for k, v in pairs(raw) do
    if type(v) == "table" and v.name then
      out[v.name] = (out[v.name] or 0) + v.count
    else
      out[k] = v
    end
  end
  return out
end

-- ---------------------------------------------------------------------------
-- Local view: the belts, inserters and chests around one point.
-- ---------------------------------------------------------------------------

local DIR_NAMES = {[0] = "north", [4] = "east", [8] = "south", [12] = "west"}

local function dir_name(d)
  return DIR_NAMES[d] or tostring(d)
end

local function dist2(p, c)
  local dx, dy = p.x - c.x, p.y - c.y
  return dx * dx + dy * dy
end

local function nearest_first(list, centre, limit)
  local ranked = {}
  for _, e in pairs(list) do ranked[#ranked + 1] = e end
  table.sort(ranked, function(a, b) return dist2(a.position, centre) < dist2(b.position, centre) end)
  local kept = {}
  for i = 1, math.min(#ranked, limit) do kept[i] = ranked[i] end
  return kept, #ranked
end

-- Walks the belt graph, not the geometry. Merging by position glues parallel lanes into
-- one fictional belt; following belt_neighbours cannot. Corners, undergrounds, splitters,
-- side-loads and tier changes all end a run, because each of them is a place where the
-- contents can change.
local function belt_runs(surface, force, area)
  local belts = safe(function()
    return surface.find_entities_filtered{area = area, type = "transport-belt", force = force}
  end, {}, "local_view")

  local in_area = {}
  for _, b in pairs(belts) do
    if b.unit_number then in_area[b.unit_number] = b end
  end

  local function neighbours(b)
    local n = safe(function() return b.belt_neighbours end, nil, "local_view")
    return (n and n.inputs or {}), (n and n.outputs or {})
  end

  local function continues(a, b)
    if not a or not b then return false end
    if b.type ~= "transport-belt" or a.name ~= b.name then return false end
    if a.belt_shape ~= "straight" or b.belt_shape ~= "straight" then return false end
    local _, a_out = neighbours(a)
    local b_in = neighbours(b)
    return #a_out == 1 and #b_in == 1
  end

  local runs, seen = {}, {}

  local function build_run(head)
    local chain, cur = {head}, head
    seen[head.unit_number] = true
    while true do
      local _, outs = neighbours(cur)
      local nxt = outs[1]
      if not (nxt and nxt.unit_number and in_area[nxt.unit_number]) then break end
      if seen[nxt.unit_number] then break end
      if not continues(cur, nxt) then break end
      seen[nxt.unit_number] = true
      chain[#chain + 1] = nxt
      cur = nxt
    end
    runs[#runs + 1] = chain
  end

  for _, b in pairs(belts) do
    if b.unit_number and not seen[b.unit_number] then
      local ins = neighbours(b)
      local prev = ins[1]
      local is_head = not (#ins == 1 and prev and prev.unit_number
                           and in_area[prev.unit_number] and continues(prev, b))
      if is_head then build_run(b) end
    end
  end
  -- Anything left is a closed loop; start it wherever we find it.
  for _, b in pairs(belts) do
    if b.unit_number and not seen[b.unit_number] then build_run(b) end
  end

  return runs, neighbours
end

local function collect_local_view(force, surface, centre, radius)
  local area = {{centre.x - radius, centre.y - radius}, {centre.x + radius, centre.y + radius}}

  local names, name_idx = {}, {}
  local function nid(name)
    if not name then return nil end
    local i = name_idx[name]
    if not i then
      i = #names + 1
      names[i] = name
      name_idx[name] = i
    end
    return i
  end

  -- unit_number -> row id. An entity that has a row is referenced by that row; anything
  -- else falls back to its prototype name index, which JSON keeps distinguishable
  -- because a row id is a string and a name index is a number.
  local row_of = {}
  local function ref(e)
    if not e then return nil end
    local un = e.unit_number
    if un and row_of[un] then return row_of[un] end
    return nid(e.name)
  end

  local research_ings = safe(function()
    local cr = force.current_research
    return cr and cr.research_unit_ingredients or nil
  end, nil, "local_view")
  local status_names = status_lookup()

  -- machines
  local raw_machines = safe(function()
    return surface.find_entities_filtered{area = area, type = MACHINE_TYPES, force = force}
  end, {}, "local_view")
  local machines, machines_total = nearest_first(raw_machines, centre, MAX_LOCAL_MACHINES)
  local machine_rows = {}
  for i, e in ipairs(machines) do
    local id = "m" .. i
    if e.unit_number then row_of[e.unit_number] = id end
    local sname = status_names[e.status] or "unknown"
    local row = {id = id, name = nid(e.name), x = round(e.position.x), y = round(e.position.y),
                 status = sname}
    local recipe = machine_recipe(e)
    if not recipe and e.type == "mining-drill" then
      local ore = safe(function() return e.mining_target and e.mining_target.name or nil end,
                       nil, "local_view")
      recipe = ore and ("mining " .. ore) or nil
    end
    row.recipe = recipe
    if SHORTAGE_STATUS[sname] then
      row.short_on = missing_ingredients(e, research_ings)
    end
    machine_rows[i] = row
  end

  -- containers
  local raw_chests = safe(function()
    return surface.find_entities_filtered{area = area, type = CONTAINER_TYPES, force = force}
  end, {}, "local_view")
  local chests, chests_total = nearest_first(raw_chests, centre, MAX_LOCAL_CONTAINERS)
  local chest_rows = {}
  for i, e in ipairs(chests) do
    local id = "c" .. i
    if e.unit_number then row_of[e.unit_number] = id end
    local inv = safe(function() return e.get_inventory(defines.inventory.chest) end, nil, "local_view")
    local held = inv and contents_map(safe(function() return inv.get_contents() end, {}, "local_view")) or {}
    chest_rows[i] = {
      id = id, name = nid(e.name),
      x = round(e.position.x), y = round(e.position.y),
      mode = safe(function() return e.prototype.logistic_mode end, nil, "local_view"),
      items = items_line(held),
    }
  end

  -- belt runs
  local runs, neighbours = belt_runs(surface, force, area)
  -- Nearest to centre, but a long run beats a stub at the same distance. Side-loaded
  -- merge lanes are legitimately a chain of one-tile runs, and without this they fill
  -- the whole budget while the main bus two tiles further out falls off the end. The
  -- length bonus is capped so a long run far away cannot outrank the block in question.
  local function run_score(chain)
    local mid = chain[math.ceil(#chain / 2)].position
    return math.sqrt(dist2(mid, centre)) - math.min(#chain, 12)
  end
  table.sort(runs, function(a, b) return run_score(a) < run_score(b) end)
  local runs_total = #runs
  local kept_runs = {}
  for i = 1, math.min(runs_total, MAX_LOCAL_BELT_RUNS) do kept_runs[i] = runs[i] end
  for i, chain in ipairs(kept_runs) do
    local id = "b" .. i
    for _, b in pairs(chain) do
      if b.unit_number then row_of[b.unit_number] = id end
    end
  end
  local belt_rows = {}
  for i, chain in ipairs(kept_runs) do
    local head, tail = chain[1], chain[#chain]
    local held = {}
    for _, b in pairs(chain) do
      local lines = safe(function() return b.get_max_transport_line_index() end, 0, "local_view")
      for li = 1, lines do
        for name, n in pairs(contents_map(safe(function()
          return b.get_transport_line(li).get_contents()
        end, {}, "local_view"))) do
          held[name] = (held[name] or 0) + n
        end
      end
    end
    local h_in = neighbours(head)
    local _, t_out = neighbours(tail)
    belt_rows[i] = {
      id = "b" .. i, name = nid(head.name), tiles = #chain,
      head = {round(head.position.x), round(head.position.y)},
      tail = {round(tail.position.x), round(tail.position.y)},
      items = items_line(held),
      from = ref(h_in[1]),
      to = ref(t_out[1]),
    }
  end

  -- inserters, last so that every other row already has an id to point at
  local raw_ins = safe(function()
    return surface.find_entities_filtered{area = area, type = "inserter", force = force}
  end, {}, "local_view")
  local ins, ins_total = nearest_first(raw_ins, centre, MAX_LOCAL_INSERTERS)
  local inserter_rows = {}
  for i, e in ipairs(ins) do
    local pick = safe(function() return e.pickup_target end, nil, "local_view")
    local drop = safe(function() return e.drop_target end, nil, "local_view")
    local row = {
      id = "i" .. i, name = nid(e.name),
      x = round(e.position.x), y = round(e.position.y),
      takes_from = dir_name(e.direction),
      from = ref(pick), to = ref(drop),
    }
    -- "the belt tile under this inserter is empty" is the whole diagnosis; a bare
    -- prototype name is not. When the source belt is a run listed above, that run's
    -- own items line already says what is on it, so only the starved case is worth
    -- repeating - sixty copies of the same string is most of what this block costs.
    if pick and pick.type == "transport-belt" then
      local held, total = {}, 0
      local lines = safe(function() return pick.get_max_transport_line_index() end, 0, "local_view")
      for li = 1, lines do
        for name, n in pairs(contents_map(safe(function()
          return pick.get_transport_line(li).get_contents()
        end, {}, "local_view"))) do
          held[name] = (held[name] or 0) + n
          total = total + n
        end
      end
      if total < 2 or type(row.from) ~= "string" then
        row.src_items = items_line(held, 2)
      end
    end
    inserter_rows[i] = row
  end

  if #machine_rows + #inserter_rows + #belt_rows + #chest_rows == 0 then
    return {
      centre = {x = round(centre.x), y = round(centre.y)},
      radius = radius,
      note = "nothing is built within " .. radius .. " tiles of this point.",
    }
  end

  return {
    centre = {x = round(centre.x), y = round(centre.y)},
    radius = radius,
    names = names,
    machines = machine_rows,
    inserters = inserter_rows,
    belt_runs = belt_rows,
    containers = chest_rows,
    shown = {machines = #machine_rows, inserters = #inserter_rows,
             belt_runs = #belt_rows, containers = #chest_rows},
    total = {machines = machines_total, inserters = ins_total,
             belt_runs = runs_total, containers = chests_total},
    note = "A square 'radius' tiles either side of centre. name is an index into names[]. "
        .. "from/to are a row id string when the other end is listed here, otherwise an "
        .. "index into names[]; a missing from/to means that end is bare ground. Rows are "
        .. "nearest to centre first, longer belt runs preferred over stubs; where shown < "
        .. "total the rest were cut. Inserter takes_from is the side it PICKS UP from. "
        .. "src_items is what sits on the belt tile the inserter reaches into, given only "
        .. "when that tile is starved or its run is not listed - otherwise read the run's "
        .. "own items. Belt runs are merged along the belt graph and end at any corner, "
        .. "underground, splitter, side-load or tier change, so a short run is normal. "
        .. "Poles and pipes are deliberately not listed.",
  }
end

-- Focus, in priority order: coordinates the player typed, then the last map button they
-- clicked, then where they are standing. No matching on prototype names - players write
-- "LDS" and "green circuits", not "low-density-structure".
local function parse_coords(text)
  if type(text) ~= "string" then return nil end
  local x, y = text:match("%[gps=(%-?%d+)[%.%d]*,%s*(%-?%d+)")
  if not x then x, y = text:match("%((%-?%d+)%s*,%s*(%-?%d+)%s*%)") end
  if not x then x, y = text:match("%[(%-?%d+)%s*,%s*(%-?%d+)%s*%]") end
  if not x then
    -- A bare pair is only trusted when one side carries a sign, so "6,000 plates" and
    -- "18 assemblers, 3 stalled" do not become map coordinates.
    for a, b in text:gmatch("(%-?%d+)%s*,%s*(%-?%d+)") do
      if a:sub(1, 1) == "-" or b:sub(1, 1) == "-" then
        x, y = a, b
        break
      end
    end
  end
  if not x then return nil end
  return {x = tonumber(x), y = tonumber(y)}
end

local function focus_point(player, question)
  local typed = parse_coords(question)
  if typed then return typed, "coordinates in the question" end

  local last = storage and storage.last_focus and storage.last_focus[player.index]
  if last and last.x then
    return {x = last.x, y = last.y}, "the last map button the player clicked"
  end

  return {x = player.position.x, y = player.position.y}, "the player's position"
end

function M.collect(player, tier, question)
  local full = tier ~= "compact"
  local force, surface = player.force, player.surface

  errors = {}
  section = "machines"
  local collected = collect_machines(force, surface, full)
  section = "feeds"
  -- Compact tier exists for small local models, so it gets short_on (cheap and high value)
  -- but not the inserter sweep behind fed_by / outputs_to.
  local inserters_seen = full and collect_feeds(force, surface, collected.by_unit) or nil

  section = "production"
  local snap = {
    meta = {
      tick = game.tick,
      playtime_hours = fmt(game.tick / 60 / 3600, 2),
      surface = surface.name,
      force = force.name,
      tier = tier,
      mods = {},
      evolution_pct = fmt(try(function() return force.get_evolution_factor(surface) end, 0) * 100, 1),
    },
    player = {
      name = player.name,
      x = round(player.position.x),
      y = round(player.position.y),
      inventory = full and inventory_contents(player) or nil,
    },
    production = {
      items = collect_flows(item_stats(force, surface), item_prototypes(), full and 120 or 25),
      fluids = collect_flows(fluid_stats(force, surface), fluid_prototypes(), full and 40 or 10),
      units = "counts over the stated window for this surface",
    },
  }

  section = "power"
  snap.power = collect_power(force, surface)
  section = "research"
  snap.research = collect_research(force)
  section = "alerts"
  snap.alerts = collect_alerts(player)
  section = "pollution"
  snap.pollution = round(safe(function() return surface.get_total_pollution() end, 0))
  section = "machines"
  snap.machines = machines_section(collected, full)
  snap.machines.inserters_scanned = inserters_seen
  if not full then snap.machines.note = nil end

  for name, version in pairs(try(function() return script.active_mods end, {})) do
    snap.meta.mods[name] = version
  end

  if full then
    section = "footprints"
    snap.footprints = {
      sizes = collect_footprints(force, surface, snap.machines.groups),
      note = "tile width x height. Entities sit on their CENTRE, so two 3x3 machines "
          .. "side by side are 3 apart, and a 1x1 inserter tucks into the tile "
          .. "immediately beside a 3x3 machine, 2 tiles from its centre.",
    }
    section = "census"
    snap.census = collect_census(force, surface)
    section = "logistics"
    snap.logistics = collect_logistics(force, surface)

    section = "local_view"
    local centre, why = focus_point(player, question)
    local ok, view = pcall(collect_local_view, force, surface, centre, LOCAL_RADIUS)
    if ok then
      view.focused_on = why
      snap.local_view = view
    else
      note_error("local_view")
      snap.local_view = {error = tostring(view), centre = {x = round(centre.x), y = round(centre.y)}}
    end
  end

  section = "?"
  -- gather_ticks cannot work: game.tick does not advance inside a single call and
  -- os.clock is nil in the sandbox. The error tally is the useful signal instead.
  if next(errors) then snap.meta.collector_errors = errors end
  return snap
end

return M
