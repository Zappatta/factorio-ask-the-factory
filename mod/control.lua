local mod_gui = require("mod-gui")
local snapshot = require("snapshot")

local BUS_FILE = "llm_scout_bus.jsonl"
local WINDOW = "llm_scout_window"
local SCROLL = "llm_scout_scroll"
local INPUT = "llm_scout_input"
local BACKEND_DD = "llm_scout_backend"
local TOP_BUTTON = "llm_scout_top_button"
local CHUNK_CHARS = 1800
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
