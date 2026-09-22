# LLM Scout

Chat with an LLM about your Factorio factory, from inside the game.

A mod collects a snapshot of your base and writes it to `script-output`. A bridge daemon
picks it up, streams an answer from Claude (or a local model), and pushes the text back
into the game over RCON, token by token. The model can drop pins on your map.

```
 Factorio server (RCON)                bridge daemon
 ┌────────────────────────┐            ┌──────────────────────┐
 │ mod: llm-scout         │            │                      │
 │  chat GUI              │  bus file  │  tail → provider     │
 │  snapshot collector ───┼───────────▶│    claude-cli        │
 │                        │            │    anthropic-api     │
 │  remote.deliver()   ◀──┼────RCON────┤    ollama            │
 │  remote.ping()         │            │    openai-compatible │
 └────────────────────────┘            └──────────────────────┘
          ▲
          │ you connect as a client to localhost
```

## Why a server

RCON is the only way to push data *into* a running Factorio game, and Factorio only opens
the RCON socket in server mode. A normal client never does. So you run a local server and
connect to it as a client — same save, same mods, feels like singleplayer.

The server gets its own `serverdata/` write-data directory, because Factorio takes an
exclusive lock on it and cannot share one with your running client.

Console commands are used to deliver replies, so **achievements are disabled** on the
session save.

## Usage

### GUI launcher (Rust + egui)

```bash
cd launcher-gui && cargo build --release
./launcher-gui/target/release/llmscout-launcher
```

Pick a save from the list, hit **Launch**. It installs the mod into your client,
mirrors your enabled mods to the server, starts the server and bridge, and launches
Factorio already connected via `--mp-connect`. Double-clicking a save launches it.

While running it shows a live log; **Stop** shuts everything down over RCON so the
server writes a final save. **Export session…** copies your progress back to your
normal saves folder via a native file dialog. The model dropdown writes the chosen
backend into `config.toml`.

An 8 MB single binary. Paths are auto-detected on macOS, Windows and Linux (including
secondary Steam libraries); **Browse…** overrides either one if a guess is wrong.

```bash
./llmscout-launcher --check    # print what it detected, no window
```

Python 3 must be on PATH — the bridge daemon is Python, so the binary is not fully
self-contained. The GUI reports this instead of failing at launch.

### Headless launcher (Python)

Same job, no window, for scripting or when the GUI will not build:

```bash
python3 launcher.py                    # pick a save interactively
python3 launcher.py --save "my world"  # skip the picker
python3 launcher.py --list             # list saves
python3 launcher.py --resume           # continue the session save
python3 launcher.py --no-client        # server + bridge only
python3 launcher.py --export "name"    # copy the session save back
```

Override discovery in either launcher:

```bash
FACTORIO_BIN=/path/to/factorio FACTORIO_DIR=/path/to/data ...
```

### In game

Open the window with **Ctrl+Shift+L** or the radar button top-left. There is also a
`/llm <question>` console command.

Your original saves are never touched. The chosen save is copied to
`serverdata/saves/session.zip` and the server runs on that copy.

## Backends

Set in `config.toml`, switchable live from the dropdown in the chat window.

| Backend | Auth | Notes |
|---|---|---|
| `claude-cli` | your existing Claude Code login | Default. No API key. |
| `anthropic-api` | `ANTHROPIC_API_KEY` | Leanest token use. |
| `ollama` | none, local | Set `model` in config. |
| `openai-compatible` | optional | LM Studio, llama.cpp, vLLM, OpenRouter. |

The `claude-cli` backend runs with MCP servers, settings and tools stripped
(`--strict-mcp-config`, `--setting-sources ""`, `--allowed-tools ""`), which cuts the
per-question overhead from ~27k cache-creation tokens to ~3.7k.

## Snapshot tiers

`[snapshot] tier` in `config.toml`:

- `auto` — `full` for Claude, `compact` for local models
- `full` — ~23 KB: production rates, power, machine clusters with map positions, trains,
  census, research, alerts
- `compact` — ~4 KB: the same minus positions and census, for small context windows

## Dev tools

```bash
python3 bridge/ask.py "why is my coal backed up?"          # ask from the terminal (needs a running server)
python3 bridge/ask.py "..." --backend ollama               # try another backend
python3 bridge/ask.py "..." --show-snapshot                # dump what the model sees
python3 bridge/bridge.py --verbose                         # bridge alone, chatty
```

Two remote interfaces exist for debugging, callable over RCON:

```
/silent-command remote.call('llm_scout','probe','{"player_index":1,"tier":"full"}')
/silent-command remote.call('llm_scout','gui_test','{"player_index":1}')
```

## Layout

```
mod/              the Factorio mod
  control.lua       GUI, events, remote interface
  snapshot.lua      state collection
bridge/
  bridge.py         tail bus file → provider → RCON
  providers.py      the four backends
  prompt.py         system prompt
  rcon.py           Source RCON client, stdlib only
  ask.py            terminal dev tool
config.toml       backend, model, RCON, tier
launcher-gui/     Rust + egui GUI launcher
  src/factorio.rs   path discovery, save listing, mod mirroring
  src/session.rs    server/bridge/client lifecycle, log streaming
  src/rcon.rs       RCON client (for the clean /quit on shutdown)
  src/config.rs     reads config.toml
  src/main.rs       egui UI
launcher.py       headless equivalent of the GUI
serverdata/       server write-data (generated, not in git)
```

## API notes

Factorio 2.0 details that are easy to get wrong, all verified against a live game:

- Item statistics read as counts (`count=true`). Electric network statistics read as
  **joules per tick** and only with `count=false` — `kW = value * 60 / 1000`.
- `force.get_trains()` does not exist. Use `game.train_manager.get_trains{surface, force}`.
- `LuaEntityPrototype.max_energy_production` does not exist. Use
  `get_max_energy_production()` (works for solar; `max_power_output` returns nil there).
- `LuaFlowStatistics.output_counts` is a property, not a method, and holds lifetime totals.
- The shortest flow precision is `five_seconds`, not `one_second`.
- Generation always equals consumption unless the base is browning out. Compare consumption
  against installed **capacity** for real headroom.
- `require` only works at control.lua parse time, never from a console command.

## Chat pane rendering: two separate traps

Both make **every affected label render as nothing** while the element is still present
with the correct caption — reading them back over RCON shows the full text, only the
rendering is gone, and it never recovers. Both were found by A/B on a live game.

**1. `scroll_to_bottom()` blanks the pane.** Identical labels render correctly; add one
`scroll_to_bottom()` call and they all go blank, sometimes without even a scrollbar.
Deferring it to a later tick does not help. There is no auto-scroll in the chat window
as a result.

**2. A wrapping label needs an explicit `style.width`, not `maximal_width`.** A label
created with an empty caption and appended to afterwards — exactly what streaming does —
never re-lays-out under `maximal_width` and stays invisible. `style.width = 560` works
for both the create-with-text and create-empty-then-append cases.

Both were expensive to find because **labels short enough to fit on one line render fine
either way**. The bugs only appear once content wraps, which is every real answer and
none of the obvious first tests. If you are debugging this pane again, test with text
long enough to wrap, and trust the screen over the property read-back.

## Locations are buttons, not gps tags

`[gps=X,Y]` rich text does not render in a GUI label - it shows Factorio's broken-tag
icon. It only works in chat. Locations therefore come through the `[[ping:x,y|label]]`
marker, which produces both a map pin and a clickable button under the answer.

Clicking one uses `player.set_controller{type = defines.controllers.remote, position=...}`.
**`LuaPlayer.open_map` and `zoom_to_world` do not exist in 2.0** - the remote controller
replaced them. Item, entity, recipe, technology and fluid tags all render fine.

## Things that do not work, and why

Tested against Factorio 2.0.77, not assumed:

- **RCON on a normal client.** `--rcon-port` is accepted without error on a client
  launch and then silently ignored — the port never opens and `RemoteCommandProcessor`
  never starts. It only initialises under `--start-server`. This is why a server is
  required for the in-game window.
- **Reading a file from Lua.** There is no read API at all. `io` and `os` are `nil`,
  and `helpers.read_file` / `game.read_file` do not exist. `helpers.write_file` works,
  so the sandbox is strictly one-way out.
- **Sharing a data directory with your client.** Factorio takes an exclusive lock on
  it, so the server gets its own `serverdata/` via `write-data` in a private
  `config.ini`.
- **Save file metadata.** The `level-init.dat` header format is undocumented; a parser
  for it misreported 2.0.77 saves as 1.1.107, so the launcher shows filesystem
  metadata only.

## Note on the two launchers

`launcher-gui` and `launcher.py` implement the same steps independently and can drift.
The GUI is the primary one; the Python script exists for headless use and as a fallback
if the Rust build is unavailable. If you change the launch sequence, change both.

## egui 0.36 API notes

The 0.36 release reworked the app API from what most examples online still show:

- `eframe::App` requires `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame)`.
  There is no `update(&mut self, ctx, frame)` any more.
- `TopBottomPanel` and `SidePanel` are gone. Use `egui::Panel::top(id)`,
  `Panel::bottom(id)`, `Panel::left(id)`, `Panel::right(id)`.
- Panels and `CentralPanel` take `&mut Ui`, not `&Context`. Reach the context with
  `ui.ctx()`.
