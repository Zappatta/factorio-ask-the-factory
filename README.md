# LLM Scout

Ask an LLM about your Factorio base from inside the game. What you're producing, where it
is, what's stalled, what to fix next.

It reads your actual save — production rates, power, machine status, research, trains — and
answers with real numbers, with clickable buttons that jump your map to the place it's
talking about.

```
> why is my uranium backed up?

Uranium chain is stalled, not starved - the choke point is downstream of mining.
- 22 drills all waiting_for_space_in_destination
- 18 centrifuges all full_output
- uranium-238: 66/hr made vs 1,476/hr used
No uranium train stop in your station list, so product is piling up locally.

  [ uranium mine (-112, 392) ]   [ centrifuges (-87, 400) ]
```

## Why it runs a local server

Factorio's Lua sandbox is one-way: a mod can write files out, but nothing can send data
back in. The only channel into a running game is **RCON**, and Factorio only opens that
port in server mode — a normal client accepts the flags and ignores them.

So the launcher starts a local server and connects your client to it. Same save, same mods,
feels like singleplayer. Your original saves are never touched; it works on a copy.

One consequence: replies arrive as console commands, so **achievements are off** on the
session save.

## Getting started

```bash
cd launcher-gui && cargo build --release
./launcher-gui/target/release/llmscout-launcher
```

Pick a save, hit **Launch**. It sets up the server, starts everything, and opens Factorio
already connected. In game press **Ctrl+Shift+L**.

**Stop** shuts it down cleanly and writes a final save. **Export session…** copies your
progress back to your normal saves folder.

Needs Python 3 on PATH. There's a headless `python3 launcher.py` if you'd rather not use
the GUI.

## Models

Switchable from a dropdown while you play. Defaults to `claude-cli`, which uses your
existing Claude Code login — no API key needed.

| | |
|---|---|
| `claude-cli` | your Claude Code login |
| `anthropic-api` | `ANTHROPIC_API_KEY` |
| `ollama` | local models |
| `openai-compatible` | LM Studio, llama.cpp, vLLM, OpenRouter |

Local models work but are noticeably weaker at reasoning over a factory. They get a
smaller snapshot automatically so they fit in a modest context window.

## Placing things — experimental

The model can also propose changes to your factory. **This part is not reliable yet.**
Treat anything it suggests as a draft and look at it before pressing anything.

Nothing is applied without a click, and everything is reversible:

- **Place** — creates the entities immediately and free
- **Place ghosts** — ghosts your construction robots build from your own materials
- **Undo** — removes exactly what was created, nothing else

Asking it to *copy something you already built* works far better than asking it to design
something new — it uses Factorio's own blueprint machinery, so belts, inserters and recipes
come across exactly. Asking it to lay out chests and inserters from scratch usually
produces something subtly wrong, because it can't see belts or inserters in the first place.

Use **Place ghosts** while you're evaluating it. A bad suggestion then just sits there
unbuilt instead of quietly rearranging your base.

## More

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — how it works, what the model sees, the
markers it can emit, dev tools, and a pile of Factorio 2.0 API notes that cost real time to
work out.
