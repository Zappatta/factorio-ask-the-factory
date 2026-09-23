# How this was built

A record of the first build session (22–23 Sep 2026): what was decided, what was tried and
abandoned, and what the failures taught. Read this if you are picking the project up and
want to know why it is shaped the way it is.

For how it works today, see [ARCHITECTURE.md](ARCHITECTURE.md). For what is next, see
[PROGRESS.md](PROGRESS.md).

## The question it started from

*Can a Factorio mod talk to an LLM?* Not directly: the Lua sandbox has no sockets, no `io`,
no `os`. But a mod can write files out, and **RCON** can push data in — so the answer was
yes, with a bridge process in the middle.

That single constraint determined nearly everything else.

## Decisions that shaped it

**A local server, not singleplayer.** RCON only exists in server mode. This was tested, not
assumed: a client accepts `--rcon-port` and silently ignores it, never opening the socket.
So the launcher runs a local server and connects your client to it. The cost is achievements
and a little friction; the alternative was no inbound channel at all.

**The server gets its own data directory.** Factorio takes an exclusive lock on write-data,
so the server cannot share one with a running client.

**Snapshot per question, not tools.** A tool round-trip is game → file → bridge → RCON →
game, about a second each. One hop carrying everything beats five hops on demand, so the
full factory state rides along with every question. It also means the model never has to
guess which query to run.

**Copy, don't compose.** The first attempts had the model place chests and inserters
entity by entity. The results looked plausible and were subtly wrong, because the snapshot
contained no belts or inserters — it was guessing. The fix was not a better prompt. It was
`clone_like`: the model points at one machine, and the *mod* walks that machine's inserter
links to find the whole functional block and copies it with Factorio's own blueprint API.
The model picks *which* and *where*; deterministic code decides *what exactly*.

That division — judgement to the model, geometry to code — is the single most useful idea
in the project.

**One binary.** The bridge started in Python. Once it was clear this should be
distributable, that became the blocker: users would need a Python install. Ported to Rust
and folded into the launcher as a thread. Users need nothing.

## Things that were tried and dropped

- **`[gps=x,y]` rich text** for clickable coordinates. Renders as a broken icon in a GUI
  label; it only works in chat. Locations became buttons instead.
- **A second Python launcher** alongside the Rust one. Two implementations of the same
  launch sequence, with a warning in the docs about drift — a warning standing in for a
  fix. Deleted.
- **Parsing save metadata** to show each save's version and mods in the picker. The
  `level-init.dat` header format is undocumented; a parser for it reported 2.0.77 saves as
  1.1.107. Dropped in favour of filesystem metadata, which is always right.
- **A ~48-tile local view.** A design review caught that the token estimate had been
  measured on an area twelve times smaller than the window proposed. Rescoped to r=28 with
  hard caps.

## What the failures taught

**Swallowed errors cost more than crashes.** Five separate bugs in one session presented
identically — the feature silently did nothing — because a `pcall` wrapper returned a
default instead of surfacing the error: `force.get_trains`, `max_energy_production`,
`LuaPlayer.open_map`, a helper missing from the file that called it, and `get_recipe()`
raising on labs and drills 254 times per snapshot. The last one was found in a single run
by an error tally. The others took hours each.

The lesson is in the code now: `meta.collector_errors` counts what gets swallowed, and mod
errors returned over RCON are logged loudly instead of at debug level.

**Verify against the running game, not against memory.** Nearly every wrong assumption was
a Factorio 1.1 habit that 2.0 had changed. The ones that matter are listed in
[ARCHITECTURE.md](ARCHITECTURE.md#factorio-20-api-notes). The habit that worked was probing
the live game over RCON before writing code against an API.

**Test with data shaped like the real thing.** Two bugs made every chat label render blank
while the elements were present with correct captions — readable over RCON, invisible on
screen. They took several rounds to find because the first tests used short strings, and
both bugs only appear once text is long enough to wrap. A third leaked markers into the
chat only when a stream chunk happened to end on a lone `[`; it survived every hand-written
reconstruction and was caught by a test sweeping chunk sizes 1 to 59.

**Ask what it does, not what it is.** The chat window's blank labels were diagnosed by
injecting labelled variants into the live game and asking which ones were visible. Reading
style properties back over RCON reported everything normal on labels showing nothing.

## The incident

Late in the session a subagent verified the snapshot collector by pushing Lua into the
running game over RCON and `load()`ing it — a way to test changes without restarting.
`/silent-command` executes in the **scenario's** context, not the mod's. It wrote into
freeplay's script state, which was then serialised into every save that followed. Several
hours of saves stopped loading: `silo-script.lua:95: attempt to index global 'storage'`.

It was recovered. The map lives in `level.dat*`; only the scenario's `control.lua` inside
the save zip was crashing, so replacing that one line with a no-op made the save load with
the factory fully intact. The cost was freeplay's flavour scripting.

**The rule: never `load()` code into the game over RCON.** Read-only `/silent-command`
probes are fine and are used throughout. Testing mod code means a restart; pay the restart.

## Where it ended

The working name throughout was **LLM Scout**; it became **Ask the Factory** afterwards.
Only the user-facing strings moved. The mod id, the mod folder, the remote interface and
the bus file still say `llm_scout`, because a save records which mods it was made with and
renaming the mod would orphan every save already on disk.

Eight commits. ~5,200 lines of Lua and Rust plus a 165-line prompt. The reading half is
solid — it finds stalled production chains, power headroom and starved recipes with real
numbers. The placing half works mechanically and is limited by the model's judgement rather
than its plumbing, which is what the next stretch of work is about.
