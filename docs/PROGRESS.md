# Progress

Where the project stands and what is next. Updated 23 Sep 2026.

## State

**Reading and diagnosis: solid.** Ask why something is stalled and it answers from real
numbers — production rates, machine status, power headroom, what each starved group is
short on, whether a block is belt-fed or bot-fed.

**Placing: experimental.** The mechanics are done — Place / Place ghosts / Undo, region
cloning, `clone_like`. The limit is the model's judgement about *what* to place, not the
plumbing.

**Distribution: one binary, no runtime dependencies.** Not yet built for other platforms.

## Uncommitted right now

The logistics-vision work (`short_on`, per-cluster feed summary, `local_view`, prompt
status-name fixes, production sort fix) plus the `TRACE THE WHOLE CHAIN` prompt section.
~816 lines across 5 files. Syntax-checked, Rust clean, collector verified live — but four
small edits in `mod/control.lua` (`last_focus` storage, threading the question through for
coordinate detection) have **not** been exercised in a real load, because Factorio caches
mod source and the workaround used to verify the collector is now forbidden.

**Do first:** restart, ask something diagnostic, confirm, commit.

## Next, in order

### 1. Replace `[[build:]]` compass directions with `pickup:` / `drop:`

The model inverts inserter directions reliably — an inserter's `direction` is the side it
takes *from*, not where it drops, and it gets this wrong even with the rule stated
explicitly in the prompt. Both inserters in one generated layout were reversed: the input
one pulling product out of the machine, the output one pushing it back in.

Stop teaching around it. Change the syntax to `pickup:north` / `drop:south` and compute the
direction in the mod. Removing the error-prone field beats explaining it.

### 2. Destination clearance for clone and place

The model picks a destination and cannot see what is already there — it has said so itself.
`detect_block` already computes the source rectangle; count entities in the destination one
and surface it on the button ("Copy — 3 entities in the way"), or offer the nearest clear
spot. A deterministic check beats any amount of vision.

Also worth pinging the detected source bbox so you can see what was selected before
committing.

### 3. Verify `local_view` earns its tokens

It costs 2.7 K sparse and up to 11 K on a dense block. Watch whether answers actually
improve. If they do not, cut the radius or drop belt runs — the per-group `short_on` and
feed summary are the cheap wins and they need no focus point at all.

### 4. `detect_block` seed accuracy

It picked a machine ~10 tiles from the one the model named, which may have copied the wrong
block. Tighten the seed search or prefer a machine whose recipe matches what was asked
about.

### 5. Recipe-chain guidelines in the prompt

`TRACE THE WHOLE CHAIN` tells it to walk `short_on` upstream. The follow-up is explicit
recipe-chain guidance: given a missing item, generate the chain (stone → stone brick →
wall) and walk it to the genuinely stuck link. Partly dependent on 3 — see whether the
chain-walking already works with the data it now has before adding more prompt.

### 6. Release CI

`.github/workflows/release.yml` builds linux x86_64, macOS arm64 + x86_64 and windows
x86_64 archives plus the mod zip on a `v*` tag and attaches them to a GitHub release. The
tag must match both `Cargo.toml` and `mod/info.json` versions. No tests run in CI yet.
Binaries are unsigned; no installers.

## Ideas not committed to

- **Local models.** They work but are noticeably weaker at reasoning over a factory. The
  compact tier exists for them; nobody has seriously evaluated how usable they are.
- **Chat output alongside the GUI.** `[gps=]` works in chat but not in GUI labels, and chat
  auto-scrolls where the GUI cannot. One line to try.
- **Auto-scroll in the chat window.** Lost because `scroll_to_bottom()` blanks the pane.
  `scroll_to_element` might work.
- **Multi-surface.** Everything assumes one surface. Space Age would need work.

## Housekeeping

- `bridge/` now contains only `logs/`, named after code that no longer exists. Move to
  `logs/` at the root.
- Corrupt saves to delete: `serverdata/saves/_autosave{1,2,3}.zip`, and `fuckme.zip` in the
  client saves folder. See the incident in [BOOTSTRAP.md](BOOTSTRAP.md#the-incident).

## Rules learned the hard way

- **Never `load()` code into the game over RCON.** It runs in the scenario's scope and
  corrupts saves. Read-only probes are fine.
- **Suspect a dead API before suspecting the data.** Check `meta.collector_errors` first.
- **`mod/*.lua` changes need a full Stop → Resume.** Prompt changes need nothing; the
  on-disk `prompt.txt` override is re-read per question.
- **The Lua syntax gate catches syntax only.** Globals resolve at call time. Run the thing.
