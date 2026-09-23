# Architecture

How LLM Scout fits together, what the model sees, and the Factorio 2.0 details that are
easy to get wrong.

## Overview

```
 Factorio server (RCON)                bridge daemon
 ┌────────────────────────┐            ┌──────────────────────┐
 │ mod: llm-scout         │            │                      │
 │  chat GUI              │  bus file  │  tail → provider     │
 │  snapshot collector ───┼───────────▶│    claude-cli        │
 │                        │            │    anthropic-api     │
 │  remote.deliver()   ◀──┼────RCON────┤    ollama            │
 │  remote.ping()         │            │    openai-compatible │
 │  remote.place/clone()  │            │                      │
 └────────────────────────┘            └──────────────────────┘
          ▲
          │ you connect as a client to localhost
```

**Outbound.** The mod appends one JSON line per question to
`serverdata/script-output/llm_scout_bus.jsonl` via `helpers.write_file`. The bridge tails
it by byte offset.

**Inbound.** The bridge sends `/silent-command remote.call('llm_scout', ...)` over RCON.
Payloads are JSON escaped into a Lua single-quoted literal. RCON has a packet size budget,
so text is chunked at ~1200 chars and larger payloads carry `seq`/`total` for reassembly.

**Streaming.** Provider output is buffered and flushed every ~60 chars or 200 ms, so the
answer types itself into the chat window.

Tested against Factorio 2.0.77 on macOS.

## Why an isolated server directory

Factorio takes an exclusive lock on its write-data directory, so the server cannot share
one with your running client. The launcher writes `serverdata/config.ini` pointing
`write-data` at `serverdata/`, and passes `--mod-directory` separately. The client's enabled
mods are mirrored in, because multiplayer refuses to connect on a mod mismatch.

## What the model sees

A JSON snapshot accompanies every question. There are no tool round-trips — a round-trip
would be game → file → bridge → RCON → game, roughly a second each, so one hop with
everything beats five hops on demand.

- production rates per item and fluid, last minute and last hour, plus lifetime totals,
  ranked by `max(made, used)` so items nothing produces but everything consumes still rank
- power: generation, consumption and installed capacity by prototype, load percent
- machines grouped by recipe, with status histograms and spatial clusters (centroid + bbox)
- per group, `short_on`: which ingredient the stalled machines are actually missing, and how
  many of them lack it
- per group, `fed_by` / `outputs_to` / `inserters`: what the inserters on that group touch,
  which is what says belt-fed vs bot-fed without needing a focus point
- `local_view`: every belt run, inserter, machine and chest in a 56×56 square around one point
- research, pollution, alerts, trains with state histogram, station names, entity census
- exact entity footprints read from the prototypes

**Tiers.** `full` for Claude, `compact` (~19 KB, no local view, no clusters) for local models,
`auto` picks by backend. Set in `config.toml`.

**Sizes**, measured on a 668-machine, 1,143-inserter base:

| Section | Bytes |
|---|---|
| machines | 16.6 K |
| production | 9.7 K |
| local_view | 2.1 K over empty ground, 7–11 K on a dense block |
| everything else | ~4.5 K |
| **full total** | **33 K sparse, 42 K worst case** |

**Cost per question.** ~2.5k tokens of system prompt, ~9–11k of snapshot, plus the last 6
turns. History stores questions only — replaying prior snapshots would dwarf everything else.

### The local view

Full tier only. A square `radius` (28) tiles either side of a focus point. The radius is
deliberately small: real clusters span 15–33 tiles, and doubling it quadruples the token
cost for ground nobody asked about.

**Focus**, in priority order: coordinates in the question text (`(-412, 338)`, `[gps=…]`, or
a bare pair where one side is signed — an unsigned bare pair is rejected so "6,000 plates"
is not a map position); then `storage.last_focus[player.index]`, written by the goto button
handler; then `player.position`. There is deliberately no matching on prototype names —
players write "LDS" and "green circuits", not "low-density-structure".

**Caps** are 30 machines, 60 inserters, 20 belt runs, 20 containers, nearest to centre
first, with `shown` and `total` maps so the model can see what was cut. Belt runs rank by
distance minus their length (bonus capped at 12 tiles): a side-loaded merge lane is
legitimately a chain of one-tile runs, and without the bias those stubs filled the whole
budget while the main bus two tiles further out fell off the end.

**Rows carry diagnosis, not geometry.** Machines carry status, recipe and `short_on`.
Inserters carry the edge — `from`/`to` — plus `src_items`, what is on the belt tile they
reach into, which is the difference between "belt empty here" and "the belt is fine, the
inserter is the wrong tier". Containers carry logistic mode and contents; a chest with no
contents is just a name. Poles and pipes are excluded: nobody can act on them.

**Two id spaces**, kept apart by JSON type. A `names` array is emitted once and every row's
`name` is an integer index into it, because hyphenated prototype names tokenise badly and
would otherwise repeat on every row. Entity references (`from`, `to`) are the string row ids
`m1`, `i1`, `b1`, `c1` when the other end is listed, and a bare integer name index when it
is not. String means "this exact thing, listed above"; number means "some entity of this
prototype, outside the view".

**Belt runs are merged along the belt graph, not by geometry.** Follow
`belt_neighbours.outputs[1]` while the next belt has the same prototype, both are
`belt_shape == "straight"`, and the successor has exactly one input. Corners, undergrounds,
splitters, side-loads and tier changes all end a run — each of them is a place the contents
can change. Merging by position instead glues parallel lanes into one fictional belt.

**Cost.** The full collector went from 244 ms to 256 ms with all of this added: the local
view is area-limited, and the whole-surface inserter sweep behind `fed_by`/`outputs_to` is
~15 ms for 1,143 inserters.

## Markers

The model emits markers in its prose; the bridge strips them before display and converts
them to remote calls.

| Marker | Effect |
|---|---|
| `[[ping:x,y\|label]]` | Map pin plus a clickable button that opens the map there |
| `[[clone_like: x,y -> dx,dy \| label]]` | Point at one machine; the mod finds its whole block and copies it |
| `[[clone: from: x1,y1 x2,y2 / to: cx,cy]]` | Copy an explicit rectangle |
| `[[build: name, x, y, dir, recipe]]` | Place individual entities |

The `[[build:]]` name is historical — the user-facing term is *placing*, and the buttons
say Place.

Markers spanning stream chunks are handled by holding back everything from the last
unclosed `[[`.

### How clone_like finds a block

Seed on the nearest crafting machine, then three rounds of: find inserters adjacent to each
known entity, add their `pickup_target` and `drop_target`, and add whatever sits at their
pickup and drop positions. Take the bounding box of everything reached, add 2 tiles of
margin, capture that rectangle with the blueprint API.

Capped at 300 linked entities and a 72-tile span clamped around the seed, so following a
shared main bus cannot turn into a base-sized rectangle.

This is deliberately deterministic: the model chooses *which* and *where*, the mod decides
*what exactly*, because that is the part models get wrong.

### Placing and undo

**Place** pastes ghosts then calls `revive()` on each — free and instant. **Place ghosts**
leaves them for the robots. Both record entity references, so **Undo** removes exactly what
was created; for ghosts the robots have since built, it falls back to matching on exact name
and position.

## Backends

`src/bridge/providers.rs`, one streaming function per backend. Blocking HTTP via `ureq`,
deliberately no async runtime — the bridge owns a thread already.

`claude-cli` spawns `claude -p --output-format stream-json --include-partial-messages` and
reads `content_block_delta` events. It runs with MCP servers, settings and tools stripped
(`--strict-mcp-config`, `--setting-sources ""`, `--allowed-tools ""`), which cut
per-question overhead from ~27k cache-creation tokens to ~3.7k. Each invocation is a fresh
session, so conversation history is replayed manually by the bridge.

## Development

### What a change needs

| Changed | Needs |
|---|---|
| `assets/prompt.txt`, or a `prompt.txt` beside the binary | nothing — the on-disk copy wins over the embedded one and is re-read per question |
| Rust sources | rebuild and relaunch the launcher |
| `mod/*.lua` | full Stop → Resume; Factorio fixes mod code at load |

### Tools

```bash
llmscout-launcher --check                      # what it detected, no window
llmscout-launcher --ask "why is my coal backed up?"
llmscout-launcher --ask "..." --backend ollama --show-snapshot
```

`--ask` talks to a running session over RCON and prints to stdout, which is the quickest
way to iterate on the prompt without going through the GUI.

`bridge/logs/answers.log` records every raw model response **before** marker stripping.
First place to look when something the model emitted did not take effect.

Debug endpoints over RCON:

```
/silent-command remote.call('llm_scout','probe','{"player_index":1,"tier":"full"}')
/silent-command remote.call('llm_scout','gui_test','{"player_index":1}')
```

### Syntax gate

```bash
lua -e "assert(loadfile('mod/control.lua'))" && lua -e "assert(loadfile('mod/snapshot.lua'))"
cd launcher-gui && cargo test && cargo build --release
```

The Lua check is syntax only. Lua resolves globals at call time, so a missing function
passes the gate and dies at runtime — a real one shipped this way. Run the thing.

### Layout

```
mod/
  control.lua       GUI, events, remote interface, placement and clone execution
  snapshot.lua      state collection
launcher-gui/
  src/main.rs       egui UI, --check and --ask
  src/factorio.rs   path discovery, save listing, mod mirroring
  src/session.rs    server lifecycle; runs the bridge on its own thread
  src/rcon.rs       Source RCON client
  src/config.rs     reads config.toml
  src/bridge/
    mod.rs          bus tailer → provider → RCON, answer logs
    markers.rs      marker parsing and the streaming hold-back
    providers.rs    the four backends
  assets/prompt.txt system prompt, embedded at build time
config.toml
bridge/logs/        raw model answers (generated)
serverdata/         server write-data (generated)
```

## Factorio 2.0 API notes

All verified against a live game. Several cost a debugging session each, usually because a
`pcall` swallowed the failure and the feature silently did nothing. If something reads as
zero or does nothing, suspect a dead API before suspecting the data.

### Statistics

- Item statistics read as counts (`count=true`). Electric network statistics read as
  **joules per tick** and only with `count=false` — `kW = value * 60 / 1000`.
- `LuaFlowStatistics.output_counts` is a property, not a method, and holds lifetime totals.
- Shortest flow precision is `five_seconds`, not `one_second`.
- Generation always equals consumption unless the base is browning out. Compare consumption
  against installed **capacity** for real headroom.

### Renamed or removed in 2.0

- `force.get_trains()` → `game.train_manager.get_trains{surface, force}`
- `LuaEntityPrototype.max_energy_production` → `get_max_energy_production()`
- `LuaPlayer.open_map` / `zoom_to_world` → `set_controller{type = defines.controllers.remote,
  position = ...}`
- mod global table is `storage`, not `global`
- directions are 16-point: north=0, east=4, south=8, west=12

### Gotchas

- **An inserter's direction is the side it takes FROM, not where it drops.** The direction
  points at the source. `direction=north` picks up from the north, drops to the south.
- `LuaEntity.get_recipe()` **raises** on labs and mining drills rather than returning nil.
  Guard on `entity.type` before calling it. This was silently costing 254 swallowed pcalls
  per snapshot until the collector started counting them.
- Labs: there is no recipe. The packs the current research needs come from
  `force.current_research.research_unit_ingredients`, which is `{name, amount}` with no
  `type` field, unlike `LuaRecipe.ingredients`.
- `LuaInventory.get_contents()` returns an **array** of `{name, count, quality}` in 2.0, not
  a name → count map.
- `defines.inventory.assembling_machine_input`, `furnace_source` and `lab_input` are all 2,
  so one constant covers crafting machines, furnaces, labs and the rocket silo.
- A `LuaTransportLine` off a belt entity covers that **one tile** (`line_length == 1`), not
  the whole segment. Summing a run means walking the run.
- `game.reload_script()` and `game.reload_mods()` both run without error on a headless
  server but do **not** pick up edits to mod files on disk — the source is read once at
  load. Mod changes need a real Stop → Resume. To test collector changes against a live
  save without restarting, push the file over RCON in ~2.6 KB chunks into a `_G` table and
  `load(table.concat(...))` it: globals persist between `/silent-command` calls and `load`
  is available.
- `create_entity` succeeds where `can_place_entity` returns false.
- `build_blueprint` silently places nothing on ungenerated or uncharted ground — generate
  and chart the destination first.
- `require` only works at control.lua parse time, never from a console command.
- Console commands run outside mod scope, so `storage` is unreachable from
  `/silent-command`.
- RCON is never available on a client. `--rcon-port` is accepted and silently ignored
  outside `--start-server`.
- Lua cannot read files at runtime. `io` and `os` are `nil`; `helpers.write_file` works, so
  the sandbox is strictly one-way out.

## Chat pane rendering

Two traps, both of which make **every affected label render as nothing** while the element
remains present with the correct caption — readable over RCON, invisible on screen, never
recovers.

1. **`scroll_to_bottom()` blanks the pane.** Deferring to a later tick does not help. There
   is no auto-scroll in the chat window as a result.
2. **A wrapping label needs an explicit `style.width`, not `maximal_width`.** A label
   created empty and appended to — exactly what streaming does — never re-lays-out under
   `maximal_width`.

Both are invisible to labels short enough to fit on one line, which is why they survived
several rounds of testing. Test this pane with text long enough to wrap, and trust the
screen over the property read-back — every style reported normal on labels showing nothing.

`[gps=X,Y]` does not render in a GUI label; it only works in chat. Locations are therefore
buttons. Item, entity, recipe, technology and fluid tags render fine.

## egui 0.36 notes

The 0.36 release reworked the app API from what most examples still show:

- `eframe::App` requires `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame)`; there is
  no `update(&mut self, ctx, frame)`
- `TopBottomPanel` and `SidePanel` are gone — use `egui::Panel::top(id)`, `::bottom`,
  `::left`, `::right`
- panels and `CentralPanel` take `&mut Ui`, not `&Context`; reach the context with
  `ui.ctx()`

## Single binary

The launcher is self-contained: the bridge runs on a thread inside it, the system prompt is
embedded with `include_str!`, and there is no Python or other runtime dependency. Users
download one executable.

A `prompt.txt` placed beside the binary overrides the embedded copy, so the prompt can be
iterated on without a rebuild.
