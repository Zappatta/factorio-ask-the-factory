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

- production rates per item and fluid, last minute and last hour, plus lifetime totals
- power: generation, consumption and installed capacity by prototype, load percent
- machines grouped by recipe, with status histograms and spatial clusters (centroid + bbox)
- research, pollution, alerts, trains with state histogram, station names, entity census
- exact entity footprints read from the prototypes

**Tiers.** `full` (~23 KB) for Claude, `compact` (~4 KB) for local models, `auto` picks by
backend. Set in `config.toml`.

**Cost per question.** ~2k tokens of system prompt, ~6k of snapshot, plus the last 6 turns.
History stores questions only — replaying prior snapshots would dwarf everything else.

**Not in the snapshot:** belts, inserters, chests, pipes. This is the single biggest
limitation, and the reason composing layouts works badly. Adding a bounded local view is
the planned next step.

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
