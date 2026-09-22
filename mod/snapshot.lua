local M = {}

local CLUSTER_CELL = 48
local MAX_RECIPE_GROUPS = 45
local MAX_CLUSTERS_PER_GROUP = 6
local MAX_ALERT_EXAMPLES = 4

local CENSUS_TYPES = {
  "transport-belt", "underground-belt", "splitter", "inserter", "assembling-machine",
  "furnace", "mining-drill", "electric-pole", "pipe", "pipe-to-ground", "pump",
  "lab", "roboport", "logistic-robot", "construction-robot", "container",
  "logistic-container", "solar-panel", "accumulator", "boiler", "generator",
  "reactor", "heat-pipe", "radar", "wall", "gun-turret", "laser-turret",
  "train-stop", "locomotive", "cargo-wagon", "fluid-wagon", "beacon", "rocket-silo",
}

local function safe(fn, default)
  local ok, res = pcall(fn)
  if ok and res ~= nil then return res end
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

local function item_prototypes()
  if prototypes and prototypes.item then return prototypes.item end
  return game.item_prototypes
end

local function fluid_prototypes()
  if prototypes and prototypes.fluid then return prototypes.fluid end
  return game.fluid_prototypes
end

local function item_stats(force, surface)
  return safe(function() return force.get_item_production_statistics(surface) end)
      or safe(function() return force.item_production_statistics end)
end

local function fluid_stats(force, surface)
  return safe(function() return force.get_fluid_production_statistics(surface) end)
      or safe(function() return force.fluid_production_statistics end)
end

local function precision(name)
  local p = defines.flow_precision_index
  return p and p[name] or nil
end

-- Handles both the 2.0 `category` form and the 1.1 `input` boolean form.
local function flow(stats, name, category, prec, as_count)
  if not stats or not prec then return 0 end
  if as_count == nil then as_count = true end
  local v = safe(function()
    return stats.get_flow_count{name = name, category = category,
                                precision_index = prec, count = as_count}
  end)
  if v then return v end
  return safe(function()
    return stats.get_flow_count{name = name, input = (category == "input"),
                                precision_index = prec, count = as_count}
  end, 0)
end

local function stat_names(stats)
  local names = {}
  if not stats then return names end
  for _, prop in pairs({"output_counts", "input_counts"}) do
    local t = safe(function() return stats[prop] end)
    if t then for name in pairs(t) do names[name] = true end end
  end
  if next(names) then return names end
  for _, getter in pairs({"get_output_counts", "get_input_counts"}) do
    local t = safe(function() return stats[getter](stats) end)
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
        lifetime_made = round(safe(function() return stats.get_output_count(name) end, 0)),
      }
    end
  end
  table.sort(out, function(a, b)
    if a.made_last_hour == b.made_last_hour then return a.name < b.name end
    return a.made_last_hour > b.made_last_hour
  end)
  if limit and #out > limit then
    local trimmed = {}
    for i = 1, limit do trimmed[i] = out[i] end
    return trimmed
  end
  return out
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

local function collect_machines(force, surface, full)
  local ents = safe(function()
    return surface.find_entities_filtered{
      type = {"assembling-machine", "furnace", "rocket-silo", "mining-drill", "lab"},
      force = force,
    }
  end, {})

  local status_names = status_lookup()
  local groups, order = {}, {}
  local totals = {}

  for _, e in pairs(ents) do
    local recipe = safe(function()
      local r = e.get_recipe()
      return r and r.name or nil
    end)
    if not recipe and e.type == "mining-drill" then
      local ore = safe(function() return e.mining_target and e.mining_target.name or nil end)
      recipe = ore and ("mining " .. ore) or nil
    end
    local key = recipe or ("(" .. e.name .. ")")
    local g = groups[key]
    if not g then
      g = {produces = key, is_recipe = recipe ~= nil, count = 0,
           machines = {}, status = {}, positions = {}}
      groups[key] = g
      order[#order + 1] = g
    end
    g.count = g.count + 1
    g.machines[e.name] = (g.machines[e.name] or 0) + 1

    local sname = status_names[e.status] or "unknown"
    g.status[sname] = (g.status[sname] or 0) + 1
    totals[sname] = (totals[sname] or 0) + 1

    if full then g.positions[#g.positions + 1] = e.position end
  end

  table.sort(order, function(a, b)
    if a.count == b.count then return a.produces < b.produces end
    return a.count > b.count
  end)

  local out = {}
  for i = 1, math.min(#order, MAX_RECIPE_GROUPS) do
    local g = order[i]
    local entry = {produces = g.produces, machine_count = g.count,
                   machines = g.machines, status = g.status}
    if full and #g.positions > 0 then
      local clusters = cluster_positions(g.positions)
      local keep = {}
      for j = 1, math.min(#clusters, MAX_CLUSTERS_PER_GROUP) do keep[j] = clusters[j] end
      entry.locations = keep
    end
    out[#out + 1] = entry
  end

  return {groups = out, total_machines = #ents, status_totals = totals,
          groups_shown = #out, groups_total = #order}
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
          local pos = safe(function() return a.target.position end)
                   or safe(function() return a.position end)
          examples[label][#examples[label] + 1] = {
            entity = safe(function() return a.target.name end),
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

function M.collect(player, tier)
  local full = tier ~= "compact"
  local force, surface = player.force, player.surface
  local started = safe(function() return game.tick end, 0)

  local snap = {
    meta = {
      tick = game.tick,
      playtime_hours = fmt(game.tick / 60 / 3600, 2),
      surface = surface.name,
      force = force.name,
      tier = tier,
      mods = {},
      evolution_pct = fmt(safe(function() return force.get_evolution_factor(surface) end, 0) * 100, 1),
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
    power = collect_power(force, surface),
    research = collect_research(force),
    alerts = collect_alerts(player),
    pollution = round(safe(function() return surface.get_total_pollution() end, 0)),
    machines = collect_machines(force, surface, full),
  }

  for name, version in pairs(safe(function() return script.active_mods end, {})) do
    snap.meta.mods[name] = version
  end

  if full then
    snap.footprints = {
      sizes = collect_footprints(force, surface, snap.machines.groups),
      note = "tile width x height. Entities sit on their CENTRE, so two 3x3 machines "
          .. "side by side are 3 apart, and a 1x1 inserter tucks into the tile "
          .. "immediately beside a 3x3 machine, 2 tiles from its centre.",
    }
    snap.census = collect_census(force, surface)
    snap.logistics = collect_logistics(force, surface)
  end

  snap.meta.gather_ticks = safe(function() return game.tick end, 0) - started
  return snap
end

return M
