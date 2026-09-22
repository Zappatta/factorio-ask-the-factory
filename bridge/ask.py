"""Ask a question from the terminal against the live game. Dev/debug tool.

    python3 bridge/ask.py "why is my coal backed up?"
"""
import argparse
import json
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import providers
from prompt import SYSTEM
from rcon import RconClient, lua_quote


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("question")
    ap.add_argument("--backend")
    ap.add_argument("--tier", default="full")
    ap.add_argument("--config", default=str(Path(__file__).parent.parent / "config.toml"))
    ap.add_argument("--show-snapshot", action="store_true")
    args = ap.parse_args()

    cfg = tomllib.load(open(args.config, "rb"))
    backend = args.backend or cfg["bridge"]["backend"]
    rc = cfg["rcon"]

    client = RconClient(rc["host"], rc["port"], rc["password"])
    client.connect()

    req = json.dumps({"player_index": 1, "tier": args.tier, "dump": True})
    raw = client.command(
        f"/silent-command remote.call('llm_scout','probe','{lua_quote(req)}')"
    )
    lines = raw.strip().split("\n")
    snapshot = next((l for l in lines if l.startswith("{")), None)
    if snapshot is None:
        print("could not get a snapshot. probe said:\n" + raw, file=sys.stderr)
        sys.exit(1)

    print(f"[snapshot {len(snapshot)/1024:.1f} KB, backend {backend}]\n", file=sys.stderr)
    if args.show_snapshot:
        print(json.dumps(json.loads(snapshot), indent=2)[:4000], file=sys.stderr)

    messages = [{"role": "user",
                 "content": f"Current factory snapshot:\n{snapshot}\n\nPlayer question: {args.question}"}]
    for chunk in providers.stream(backend, cfg.get(backend, {}), SYSTEM, messages):
        print(chunk, end="", flush=True)
    print()


if __name__ == "__main__":
    main()
