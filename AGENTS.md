# Working on this repo

Operational rules for any coding agent. Short on purpose — read it all before touching
anything. Background is in [docs/](docs/); this file is what you must not get wrong.

## What this is

Ask the Factory: a Factorio 2.0 mod (`mod/`, Lua) plus a launcher (`launcher-gui/`, Rust +
egui) that runs a local Factorio server, bridges it to an LLM over RCON, and lets the
player chat with it in game. One self-contained binary, no runtime dependencies.

## Rules

**Never `load()` or inject code into the running game over RCON.** `/silent-command` runs
in the *scenario's* Lua scope, not the mod's. Writing there corrupts the scenario's script
state, which is then serialised into every save that follows — it cost several hours of a
player's progress once already. Read-only probes are fine and are used throughout. To test
mod code, restart the server and pay the cost.

**Changing `mod/*.lua` requires a full Stop → Resume of the server.** Factorio fixes mod
source at load, and neither `reload_script()` nor `reload_mods()` picks up disk changes.
There is no shortcut; do not invent one.

**Changing the prompt requires nothing.** `launcher-gui/assets/prompt.txt` is embedded at
build time, but a `prompt.txt` at the repo root overrides it and is re-read per question.
Edit both when making a permanent change.

**The internal name stays `llm_scout`.** The project is called Ask the Factory, but the mod
id in `mod/info.json`, the mod folder `llm-scout_0.1.0`, the remote interface, the GUI
element names and `llm_scout_bus.jsonl` keep the old name. A save records which mods it was
made with, so renaming the mod orphans every save already on disk. Rename user-facing
strings only.

**Verify Factorio APIs against the running game before writing code against them.** Most
wrong assumptions here were 1.1 habits that 2.0 changed. The ones already discovered are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#factorio-20-api-notes) — read that section
before touching `mod/snapshot.lua`.

**Suspect a dead API before suspecting the data.** `safe()` in `mod/snapshot.lua` returns
its default on error *or* on nil, so a removed API and an empty result look identical. Five
separate bugs presented as "the feature silently does nothing" for this reason. Check
`meta.collector_errors` first — it counts what got swallowed.

**Test with data shaped like the real thing.** Two chat-rendering bugs only appear once
text is long enough to wrap; a marker-parsing bug only appeared when a stream chunk ended
on a lone `[`. Short happy-path fixtures passed all three.

## Before claiming done

```bash
lua -e "assert(loadfile('mod/control.lua'))"
lua -e "assert(loadfile('mod/snapshot.lua'))"
cd launcher-gui && cargo test && cargo build --release
```

`cargo build --release` must stay at **zero warnings**.

The Lua check is syntax only — Lua resolves globals at call time, so a missing function
passes it and dies at runtime. That has shipped. Run the thing.

## Conventions

- Comments explain *why*, not *what*. Do not narrate the code.
- Match the surrounding style rather than importing your own.
- Function parameters stay on one line unless the line is genuinely too long.
- User-facing wording is **placing**, not "building" — "build" means building the software,
  and the in-game buttons say Place. The `[[build:]]` marker name is historical.
- Never commit unless explicitly asked.

## Layout

| Path | |
|---|---|
| `mod/control.lua` | GUI, events, remote interface, placement and clone execution |
| `mod/snapshot.lua` | state collection — what the model sees |
| `launcher-gui/src/bridge/` | bus tailer, marker parsing, the four LLM backends |
| `launcher-gui/src/session.rs` | server lifecycle; the bridge runs on a thread here |
| `launcher-gui/assets/prompt.txt` | system prompt |
| `bridge/logs/answers.log` | raw model output, pre-marker-stripping |

`bridge/logs/answers.log` is the first place to look when the model emits something that
does not take effect. It has caught more than one bug immediately.

## Scope

Ask before: changing the marker syntax, restructuring the snapshot, adding dependencies, or
touching `launcher-gui/src/bridge/` (recently ported and stable).

## Reading order

[docs/PROGRESS.md](docs/PROGRESS.md) for what is next ·
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how it works ·
[docs/BOOTSTRAP.md](docs/BOOTSTRAP.md) for why it is shaped this way
