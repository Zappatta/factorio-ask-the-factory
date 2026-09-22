local mod_gui = require("mod-gui")
local snapshot = require("snapshot")

local BUS_FILE = "llm_scout_bus.jsonl"
local WINDOW = "llm_scout_window"
local SCROLL = "llm_scout_scroll"
local INPUT = "llm_scout_input"
local BACKEND_DD = "llm_scout_backend"
local TOP_BUTTON = "llm_scout_top_button"
local CHUNK_CHARS = 1800
local MAX_BUILD_ENTITIES = 300
local WINDOW_WIDTH = 700
local CHAT_WIDTH = 624
local BRIDGE_STALE_TICKS = 60 * 30

local function to_json(t)
  if helpers and helpers.table_to_json then return helpers.table_to_json(t) end
  return game.table_to_json(t)
end

local function from_json(s)
  if helpers and helpers.json_to_table then return helpers.json_to_table(s) end
  return game.json_to_table(s)
end

local function write_bus(line)
  if helpers and helpers.write_file then
    helpers.write_file(BUS_FILE, line, true, 0)
  else
    game.write_file(BUS_FILE, line, true, 0)
  end
end

local function init_storage()
  storage.req_id = storage.req_id or 0
  storage.pending = storage.pending or {}
  storage.backends = storage.backends or {"claude-cli"}
  storage.backend_index = storage.backend_index or 1
  storage.tier = storage.tier or "auto"
  storage.bridge_tick = storage.bridge_tick or -1
  storage.history = storage.history or {}
  storage.offers = storage.offers or {}
  storage.builds = storage.builds or {}
  storage.offer_parts = storage.offer_parts or {}
end

local function bridge_alive()
  return storage.bridge_tick >= 0 and (game.tick - storage.bridge_tick) < BRIDGE_STALE_TICKS
end

local function get_window(player)
  return player.gui.screen[WINDOW]
end

local function get_scroll(player)
  local w = get_window(player)
  if not w then return nil end
  local found = nil
  local function walk(el)
    if found then return end
    if el.name == SCROLL then found = el return end
    for _, c in pairs(el.children) do walk(c) end
  end
  walk(w)
  return found
end

local function add_line(player, text, color)
  local scroll = get_scroll(player)
  if not scroll then return nil end
  -- An explicit width is required, not maximal_width: a wrapping label whose caption
  -- is appended to after creation never re-lays-out under maximal_width and renders
  -- as nothing. See the scroll-pane notes in the README.
  local lbl = scroll.add{type = "label", caption = text}
  lbl.style.single_line = false
  lbl.style.width = CHAT_WIDTH
  if color then lbl.style.font_color = color end
  -- No scroll_to_bottom here: on this pane it blanks every label it contains.
  -- See the scroll-pane note in the README.
  return lbl
end

local function ensure_top_button(player)
  local flow = mod_gui.get_button_flow(player)
  if flow[TOP_BUTTON] then return end
  local ok = pcall(function()
    flow.add{type = "sprite-button", name = TOP_BUTTON, sprite = "item/radar",
             tooltip = {"llm-scout.title"}, style = mod_gui.button_style}
  end)
  if not ok then
    flow.add{type = "button", name = TOP_BUTTON, caption = "LLM", style = mod_gui.button_style}
  end
end

local function close_window(player)
  local w = get_window(player)
  if w then w.destroy() end
end

local function open_window(player)
  close_window(player)

  local frame = player.gui.screen.add{type = "frame", name = WINDOW, direction = "vertical"}
  frame.auto_center = true
  frame.style.width = WINDOW_WIDTH

  local titlebar = frame.add{type = "flow", direction = "horizontal"}
  titlebar.drag_target = frame
  local title = titlebar.add{type = "label", caption = {"llm-scout.title"}, style = "frame_title"}
  title.drag_target = frame
  local filler = titlebar.add{type = "empty-widget", style = "draggable_space_header"}
  filler.style.height = 24
  filler.style.horizontally_stretchable = true
  filler.drag_target = frame
  local closed = pcall(function()
    titlebar.add{type = "sprite-button", name = "llm_scout_close", sprite = "utility/close",
                 style = "frame_action_button"}
  end)
  if not closed then
    titlebar.add{type = "button", name = "llm_scout_close", caption = "X"}
  end

  local inner = frame.add{type = "frame", style = "inside_shallow_frame_with_padding",
                          direction = "vertical"}

  local controls = inner.add{type = "flow", direction = "horizontal"}
  controls.style.vertical_align = "center"
  controls.style.bottom_margin = 6
  controls.add{type = "label", caption = "Model:"}
  controls.add{type = "drop-down", name = BACKEND_DD, items = storage.backends,
               selected_index = math.min(storage.backend_index, #storage.backends)}
  local gap = controls.add{type = "empty-widget"}
  gap.style.horizontally_stretchable = true
  controls.add{type = "button", name = "llm_scout_clear", caption = {"llm-scout.clear"}}

  local scroll = inner.add{type = "scroll-pane", name = SCROLL, direction = "vertical"}
  scroll.style.height = 440
  scroll.style.horizontally_stretchable = true
  scroll.vertical_scroll_policy = "auto"
  pcall(function()
    scroll.style.vertical_spacing = 6
    scroll.style.padding = 4
  end)

  local row = inner.add{type = "flow", direction = "horizontal"}
  row.style.top_margin = 6
  row.style.vertical_align = "center"
  local tf = row.add{type = "textfield", name = INPUT}
  tf.style.horizontally_stretchable = true
  tf.style.right_margin = 6
  row.add{type = "button", name = "llm_scout_send", caption = {"llm-scout.ask"},
          style = "green_button"}

  for _, entry in pairs(storage.history[player.index] or {}) do
    add_line(player, entry.text, entry.color)
  end

  if not bridge_alive() then
    add_line(player, "[color=orange]Bridge daemon has not checked in - check the launcher log.[/color]")
  end

  tf.focus()
end

local function toggle_window(player)
  if get_window(player) then close_window(player) else open_window(player) end
end

local function remember(player, text, color)
  local h = storage.history[player.index] or {}
  h[#h + 1] = {text = text, color = color}
  while #h > 60 do table.remove(h, 1) end
  storage.history[player.index] = h
end

local function submit(player, question)
  if not question or question == "" then return end

  storage.req_id = storage.req_id + 1
  local id = storage.req_id
  storage.pending[id] = nil

  local scroll = get_scroll(player)
  if scroll and #scroll.children > 0 then
    pcall(function() scroll.add{type = "line"} end)
  end

  local prompt = "[color=120,190,255]" .. question .. "[/color]"
  add_line(player, prompt)
  remember(player, prompt)

  local ok, snap = pcall(snapshot.collect, player, storage.tier)
  if not ok then snap = {error = "snapshot failed: " .. tostring(snap)} end

  storage.pending[id] = {player_index = player.index, chars = 0, started = game.tick}

  write_bus(to_json{
    type = "ask",
    id = id,
    tick = game.tick,
    player = player.name,
    player_index = player.index,
    backend = storage.backends[storage.backend_index],
    tier = storage.tier,
    question = question,
    snapshot = snap,
  } .. "\n")

  local status = add_line(player, "[color=150,150,150]thinking...[/color]")
  if status then storage.pending[id].status_ref = status end

  if not bridge_alive() then
    add_line(player, "[color=orange]No bridge daemon detected - this may go unanswered.[/color]")
  end
end

local function append_chunk(id, text)
  local p = storage.pending[id]
  if not p then return end
  local player = game.get_player(p.player_index)
  if not player then return end
  local scroll = get_scroll(player)
  if not scroll then return end

  if p.status_ref and p.status_ref.valid then
    p.status_ref.destroy()
    p.status_ref = nil
  end

  local lbl = p.label_ref and p.label_ref.valid and p.label_ref or nil
  if not lbl or p.chars > CHUNK_CHARS then
    lbl = add_line(player, "")
    p.label_ref = lbl
    p.chars = 0
  end
  if not lbl then return end
  lbl.caption = lbl.caption .. text
  p.chars = p.chars + #text
end

local function direction_value(name)
  if type(name) == "number" then return name end
  if type(name) ~= "string" then return nil end
  return defines.direction[string.lower(name)]
end

local function capture_area(player, area)
  local inv = game.create_inventory(1)
  inv[1].set_stack{name = "blueprint"}
  local captured = inv[1].create_blueprint{
    surface = player.surface, force = player.force,
    area = area, include_entities = true,
  }
  local count = captured and table_size(captured) or 0
  return inv, count
end

local function clone_count(player, offer)
  local inv, count = capture_area(player, offer.area)
  inv.destroy()
  return count
end

-- build_blueprint silently places nothing on ungenerated or uncharted ground.
local function ensure_ground(player, position)
  pcall(function()
    player.surface.request_to_generate_chunks(position, 3)
    player.surface.force_generate_chunk_requests()
    player.force.chart(player.surface, {
      {position.x - 48, position.y - 48}, {position.x + 48, position.y + 48}})
  end)
end

local function clone_paste(player, offer, as_ghost)
  ensure_ground(player, offer.dest)
  local inv = capture_area(player, offer.area)
  local ghosts = inv[1].build_blueprint{
    surface = player.surface, force = player.force,
    position = offer.dest, force_build = true,
  }
  inv.destroy()

  local placed, failed = {}, 0
  for _, ghost in pairs(ghosts or {}) do
    if ghost.valid then
      local name, pos = ghost.ghost_name, ghost.position
      if as_ghost then
        placed[#placed + 1] = {ref = ghost, name = name, x = pos.x, y = pos.y}
      else
        local ok, _, entity = pcall(function() return ghost.revive() end)
        if ok and entity and entity.valid then
          placed[#placed + 1] = {ref = entity, name = name, x = pos.x, y = pos.y}
        else
          failed = failed + 1
        end
      end
    end
  end
  return placed, failed
end

local function set_offer_state(row, live)
  if not (row and row.valid) then return end
  local place, ghosts = row["llm_scout_place"], row["llm_scout_ghosts"]
  if place and place.valid then
    place.enabled = not live
    place.tooltip = live and "Undo first to place again" or "Creates the entities immediately, for free."
  end
  if ghosts and ghosts.valid then
    ghosts.enabled = not live
    ghosts.tooltip = live and "Undo first to place again"
      or "Places blueprint ghosts. Your construction robots build them from your own materials."
  end
end

local function render_offer(player, id)
  local offer = storage.offers[id]
  if not offer then return end
  local scroll = get_scroll(player)
  if not scroll then return end

  local row = scroll.add{type = "flow", name = "llm_scout_build_" .. id, direction = "horizontal"}
  pcall(function() row.style.horizontal_spacing = 4 end)

  local n = offer.kind == "clone" and offer.count or #offer.entities
  local verb = offer.kind == "clone" and "Copy" or "Place"

  local place = row.add{
    type = "button", name = "llm_scout_place",
    caption = string.format("%s  (%d)", verb, n),
    tooltip = "Creates the entities immediately, for free.",
    style = "green_button",
  }
  place.tags = {llm_scout_build = true, offer = id, ghost = false}

  local ghosts = row.add{
    type = "button", name = "llm_scout_ghosts",
    caption = string.format("%s as ghosts  (%d)", verb, n),
    tooltip = "Places blueprint ghosts. Your construction robots build them from your own materials.",
  }
  ghosts.tags = {llm_scout_build = true, offer = id, ghost = true}

  for _, b in pairs({place, ghosts}) do
    pcall(function() b.style.height = 28 b.style.font = "default-small" end)
  end
end

local function execute_build(player, id, row, as_ghost)
  local offer = storage.offers[id]
  if not offer then return end
  if storage.builds[id] then return end

  if offer.kind == "clone" then
    local placed, failed = clone_paste(player, offer, as_ghost)
    storage.builds[id] = {placed = placed, label = offer.label, ghost = as_ghost}
    local summary = string.format("Copied %d %s to (%d, %d)", #placed,
      as_ghost and "ghosts" or "entities", offer.dest.x, offer.dest.y)
    if failed > 0 then summary = summary .. string.format(", %d failed", failed) end
    if as_ghost and #placed > 0 then summary = summary .. ". Your robots will build them." end
    add_line(player, "[color=150,220,150]" .. summary .. "[/color]")
    if row and row.valid then
      set_offer_state(row, true)
      if not row["llm_scout_undo"] then
        local undo = row.add{type = "button", name = "llm_scout_undo",
          caption = string.format("Undo  (%d)", #placed),
          tooltip = "Removes exactly what was just placed, then re-enables the buttons.",
          style = "red_button"}
        pcall(function() undo.style.height = 28 undo.style.font = "default-small" end)
        undo.tags = {llm_scout_undo = true, offer = id}
      end
    end
    return
  end

  local created, failed, blocked = {}, 0, 0
  for _, spec in pairs(offer.entities) do
    if #created >= MAX_BUILD_ENTITIES then break end
    local dir = direction_value(spec.direction)

    local clear = true
    pcall(function()
      clear = player.surface.can_place_entity{
        name = spec.name, position = {x = spec.x, y = spec.y},
        direction = dir, force = player.force}
    end)
    if not clear then blocked = blocked + 1 end

    local args
    if as_ghost then
      args = {name = "entity-ghost", inner_name = spec.name,
              position = {x = spec.x, y = spec.y}, force = player.force,
              raise_built = true, recipe = spec.recipe}
    else
      args = {name = spec.name, position = {x = spec.x, y = spec.y},
              force = player.force, raise_built = true}
    end
    if dir then args.direction = dir end

    local ok, entity = pcall(function() return player.surface.create_entity(args) end)
    if ok and entity and entity.valid then
      if spec.recipe and not as_ghost then
        pcall(function() entity.set_recipe(spec.recipe) end)
      end
      created[#created + 1] = {ref = entity, name = spec.name, x = spec.x, y = spec.y}
    else
      failed = failed + 1
    end
  end

  storage.builds[id] = {placed = created, label = offer.label, ghost = as_ghost}

  local what = as_ghost and "ghosts" or "entities"
  local summary = string.format("Placed %d of %d %s", #created, #offer.entities, what)
  if failed > 0 then summary = summary .. string.format(", %d failed", failed) end
  if blocked > 0 then summary = summary .. string.format(", %d on occupied ground", blocked) end
  if as_ghost and #created > 0 then summary = summary .. ". Your robots will build them." end
  add_line(player, "[color=150,220,150]" .. summary .. "[/color]")

  if row and row.valid then
    set_offer_state(row, true)
    if not row["llm_scout_undo"] then
      local undo = row.add{
        type = "button", name = "llm_scout_undo",
        caption = string.format("Undo  (%d)", #created),
        tooltip = "Removes exactly what was just placed, then re-enables the buttons.",
        style = "red_button",
      }
      pcall(function() undo.style.height = 28 undo.style.font = "default-small" end)
      undo.tags = {llm_scout_undo = true, offer = id}
    end
  end
end

local function undo_build(player, id, row)
  local record = storage.builds[id]
  if not record then return end

  local removed, built_since, gone = 0, 0, 0
  for _, item in pairs(record.placed) do
    if item.ref and item.ref.valid then
      if pcall(function() item.ref.destroy{raise_destroy = true} end) then
        removed = removed + 1
      else
        gone = gone + 1
      end
    else
      -- A ghost the robots already turned real: match back on exact name and
      -- position so nothing else can be caught by accident.
      local found = nil
      pcall(function()
        local hits = player.surface.find_entities_filtered{
          name = item.name, position = {x = item.x, y = item.y}, force = player.force, limit = 1}
        found = hits and hits[1]
      end)
      if found and found.valid and pcall(function() found.destroy{raise_destroy = true} end) then
        built_since = built_since + 1
      else
        gone = gone + 1
      end
    end
  end
  storage.builds[id] = nil

  local summary = string.format("Removed %d", removed + built_since)
  if built_since > 0 then
    summary = summary .. string.format(" (%d your robots had already built)", built_since)
  end
  if gone > 0 then summary = summary .. string.format(", %d were already gone", gone) end
  add_line(player, "[color=220,180,150]" .. summary .. "[/color]")

  if row and row.valid then
    local undo = row["llm_scout_undo"]
    if undo and undo.valid then undo.destroy() end
    set_offer_state(row, false)
  end
end

local PENDING_TIMEOUT_TICKS = 60 * 180

script.on_nth_tick(30, function()
  if not storage.pending then return end
  for id, p in pairs(storage.pending) do
    local elapsed = math.floor((game.tick - (p.started or game.tick)) / 60)
    if p.status_ref and p.status_ref.valid then
      if game.tick - (p.started or game.tick) > PENDING_TIMEOUT_TICKS then
        p.status_ref.caption = "[color=255,120,120]no reply after " .. elapsed ..
                               "s - check the bridge in the launcher log[/color]"
        storage.pending[id] = nil
      else
        p.status_ref.caption = "[color=150,150,150]thinking... " .. elapsed .. "s[/color]"
      end
    elseif game.tick - (p.started or game.tick) > PENDING_TIMEOUT_TICKS then
      storage.pending[id] = nil
    end
  end
end)

script.on_init(function()
  init_storage()
  for _, p in pairs(game.players) do ensure_top_button(p) end
end)

script.on_configuration_changed(function()
  init_storage()
  for _, p in pairs(game.players) do ensure_top_button(p) end
end)

script.on_event(defines.events.on_player_created, function(e)
  init_storage()
  ensure_top_button(game.get_player(e.player_index))
end)

script.on_event(defines.events.on_player_joined_game, function(e)
  init_storage()
  ensure_top_button(game.get_player(e.player_index))
end)

script.on_event("llm-scout-toggle", function(e)
  toggle_window(game.get_player(e.player_index))
end)

script.on_event(defines.events.on_gui_click, function(e)
  local el = e.element
  if not (el and el.valid) then return end
  local player = game.get_player(e.player_index)

  local tags = el.tags
  if tags and tags.llm_scout_build then
    execute_build(player, tags.offer, el.parent, tags.ghost == true)
    return
  end
  if tags and tags.llm_scout_undo then
    undo_build(player, tags.offer, el.parent)
    return
  end
  if tags and tags.llm_scout_goto then
    -- 2.0 removed LuaPlayer.open_map and zoom_to_world; the remote controller
    -- is the replacement. Report failures rather than swallowing them.
    local ok, err = pcall(function()
      player.set_controller{
        type = defines.controllers.remote,
        position = {x = tags.x, y = tags.y},
        surface = player.surface,
      }
    end)
    if not ok then
      player.print("[LLM Scout] could not open the map there: " .. tostring(err))
    end
    return
  end

  if el.name == TOP_BUTTON then
    toggle_window(player)
  elseif el.name == "llm_scout_close" then
    close_window(player)
  elseif el.name == "llm_scout_clear" then
    storage.history[player.index] = {}
    local scroll = get_scroll(player)
    if scroll then scroll.clear() end
  elseif el.name == "llm_scout_send" then
    local w = get_window(player)
    if not w then return end
    local tf = nil
    local function find(x)
      if tf then return end
      if x.name == INPUT then tf = x return end
      for _, c in pairs(x.children) do find(c) end
    end
    find(w)
    if tf then
      local q = tf.text
      tf.text = ""
      submit(player, q)
    end
  end
end)

script.on_event(defines.events.on_gui_confirmed, function(e)
  local el = e.element
  if not (el and el.valid) or el.name ~= INPUT then return end
  local player = game.get_player(e.player_index)
  local q = el.text
  el.text = ""
  submit(player, q)
end)

script.on_event(defines.events.on_gui_selection_state_changed, function(e)
  local el = e.element
  if not (el and el.valid) or el.name ~= BACKEND_DD then return end
  storage.backend_index = el.selected_index
end)

commands.add_command("llm", "Ask the LLM about your factory", function(cmd)
  local player = game.get_player(cmd.player_index)
  if player then submit(player, cmd.parameter) end
end)

remote.add_interface("llm_scout", {
  -- Bridge heartbeat plus the list of backends to show in the dropdown.
  hello = function(js)
    init_storage()
    local d = from_json(js)
    storage.bridge_tick = game.tick
    if d and d.backends and #d.backends > 0 then
      storage.backends = d.backends
      if d.selected then
        for i, b in pairs(d.backends) do
          if b == d.selected then storage.backend_index = i end
        end
      end
      if storage.backend_index > #storage.backends then storage.backend_index = 1 end
    end
    if d and d.tier then storage.tier = d.tier end
  end,

  deliver = function(js)
    local d = from_json(js)
    if not d then return end
    storage.bridge_tick = game.tick
    if d.text and d.text ~= "" then append_chunk(d.id, d.text) end
    if d.error then append_chunk(d.id, "\n[color=red]" .. d.error .. "[/color]") end
    if d.final then
      local p = storage.pending[d.id]
      if p then
        local player = game.get_player(p.player_index)
        if player and p.label_ref and p.label_ref.valid then
          remember(player, p.label_ref.caption)
        end
      end
      storage.pending[d.id] = nil
    end
  end,

  -- Diagnostics: runs the collector and reports shape over the RCON connection.
  probe = function(js)
    local d = from_json(js) or {}
    local player = game.get_player(d.player_index or 1)
    if not player then rcon.print("ERROR: no player") return end
    local ok, r = pcall(snapshot.collect, player, d.tier or "full")
    if not ok then rcon.print("ERROR: " .. tostring(r)) return end
    local encoded, j = pcall(to_json, r)
    if not encoded then rcon.print("ERROR encoding: " .. tostring(j)) return end
    rcon.print("OK bytes=" .. #j)
    rcon.print("items=" .. #r.production.items .. " fluids=" .. #r.production.fluids)
    rcon.print("machine_groups=" .. tostring(r.machines.groups_total) ..
               " machines=" .. tostring(r.machines.total_machines))
    rcon.print("status_totals=" .. to_json(r.machines.status_totals))
    rcon.print("power_gen_kw=" .. tostring(r.power.total_generation_kw) ..
               " con_kw=" .. tostring(r.power.total_consumption_kw) ..
               " acc=" .. tostring(r.power.accumulator_charge_pct))
    rcon.print("gen_breakdown=" .. to_json(r.power.generation_kw or {}))
    rcon.print("research=" .. tostring(r.research.current) ..
               " pct=" .. tostring(r.research.progress_pct) ..
               " done=" .. tostring(r.research.researched_count))
    rcon.print("pollution=" .. tostring(r.pollution) ..
               " playtime=" .. tostring(r.meta.playtime_hours) ..
               " evo=" .. tostring(r.meta.evolution_pct))
    rcon.print("alerts=" .. to_json(r.alerts.counts))
    if r.production.items[1] then rcon.print("top_item=" .. to_json(r.production.items[1])) end
    if r.machines.groups[1] then rcon.print("top_group=" .. to_json(r.machines.groups[1])) end
    if d.dump then rcon.print(j) end
  end,

  -- Builds the window, writes into it and tears it down, to smoke-test GUI code.
  gui_test = function(js)
    local d = from_json(js) or {}
    local player = game.get_player(d.player_index or 1)
    if not player then rcon.print("ERROR: no player") return end
    local ok, err = pcall(function()
      ensure_top_button(player)
      open_window(player)
      add_line(player, "[color=100,180,255]> smoke test[/color]")
      append_chunk(0, "")
      local w = get_window(player)
      assert(w and w.valid, "window missing after open")
      local scroll = get_scroll(player)
      assert(scroll and scroll.valid, "scroll pane not found")
      scroll.add{type = "label", caption = "[item=iron-plate] rich text renders"}
      local flow = mod_gui.get_button_flow(player)
      assert(flow[TOP_BUTTON], "top button missing")
      rcon.print("window=ok scroll_children=" .. #scroll.children ..
                 " top_button=" .. tostring(flow[TOP_BUTTON].type))
    end)
    if not ok then rcon.print("GUI ERROR: " .. tostring(err)) return end
    if not d.keep then close_window(player) rcon.print("closed cleanly") end
  end,

  -- Lets the bridge or a test harness submit a question without touching the GUI.
  ask = function(js)
    local d = from_json(js) or {}
    local player = game.get_player(d.player_index or 1)
    if player then submit(player, d.question) end
  end,

  -- Build specs arrive chunked because an RCON command has a size budget.
  build_offer = function(js)
    local d = from_json(js)
    if not d or not d.id then return end
    local parts = storage.offer_parts[d.id] or {}
    parts[d.seq] = d.part
    storage.offer_parts[d.id] = parts

    for i = 1, d.total do
      if parts[i] == nil then return end
    end

    local joined = table.concat(parts, "", 1, d.total)
    storage.offer_parts[d.id] = nil
    local spec = from_json(joined)
    if not spec or not spec.entities then return end

    storage.offers[d.id] = {
      label = spec.label or "build",
      entities = spec.entities,
      player_index = d.player_index or 1,
    }
    local player = game.get_player(d.player_index or 1)
    if player then render_offer(player, d.id) end
  end,

  clone_offer = function(js)
    local d = from_json(js)
    if not d or not d.id then return end
    local player = game.get_player(d.player_index or 1)
    if not player then return end

    local x1, y1 = math.min(d.x1, d.x2), math.min(d.y1, d.y2)
    local x2, y2 = math.max(d.x1, d.x2), math.max(d.y1, d.y2)
    local offer = {
      kind = "clone",
      label = d.label or "copy",
      area = {{x1, y1}, {x2, y2}},
      dest = {x = d.dx, y = d.dy},
      player_index = d.player_index or 1,
    }
    offer.count = clone_count(player, offer)
    if offer.count == 0 then
      add_line(player, "[color=255,120,120]Nothing to copy in that area.[/color]")
      return
    end
    storage.offers[d.id] = offer
    render_offer(player, d.id)
  end,

  ping = function(js)
    local d = from_json(js)
    if not d then return end
    local player = game.get_player(d.player_index or 1)
    if not player then return end
    pcall(function()
      player.force.add_chart_tag(player.surface, {
        position = {x = d.x, y = d.y},
        text = d.text or "LLM Scout",
      })
    end)

    -- [gps=] rich text does not render inside GUI labels, so a location becomes
    -- a button that opens the map instead.
    local scroll = get_scroll(player)
    if not scroll then return end

    -- Group a request's locations into one row rather than a stack of wide buttons.
    local row_name = "llm_scout_places_" .. tostring(d.id or 0)
    local row = scroll[row_name]
    if not row then
      row = scroll.add{type = "flow", name = row_name, direction = "horizontal"}
      pcall(function() row.style.horizontal_spacing = 4 end)
    end
    local btn = row.add{
      type = "button",
      caption = string.format("%s  (%d, %d)", d.text or "location", d.x, d.y),
      tooltip = "Show this on the map",
    }
    pcall(function()
      btn.style.height = 26
      btn.style.font = "default-small"
      btn.style.padding = {0, 8, 0, 8}
    end)
    btn.tags = {llm_scout_goto = true, x = d.x, y = d.y}
  end,
})
