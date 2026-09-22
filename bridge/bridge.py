"""Bridges the llm-scout Factorio mod to an LLM backend.

Tails the mod's append-only bus file in script-output, streams a reply from the
configured provider, and pushes it back into the game over RCON.
"""
import argparse
import json
import logging
import os
import re
import sys
import time
import tomllib
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import providers
from prompt import SYSTEM
from rcon import RconClient, RconError, lua_quote

BUS_FILE = "llm_scout_bus.jsonl"
HEARTBEAT_SECONDS = 10
MAX_RCON_BODY = 3500
FLUSH_CHARS = 60
FLUSH_SECONDS = 0.20
PING_RE = re.compile(r"\[\[ping:\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*(?:\|([^\]]*))?\]\]")
COMPACT_BACKENDS = {"ollama", "openai-compatible"}

log = logging.getLogger("llm-scout")


def load_config(path: Path) -> dict:
    with open(path, "rb") as fh:
        return tomllib.load(fh)


class Bridge:
    def __init__(self, cfg: dict, replay: bool = False):
        self.cfg = cfg
        bridge_cfg = cfg.get("bridge", {})
        self.backend = bridge_cfg.get("backend", "claude-cli")
        self.poll = bridge_cfg.get("poll_interval", 0.2)
        self.history_turns = bridge_cfg.get("history_turns", 6)
        self.tier_setting = cfg.get("snapshot", {}).get("tier", "auto")

        out_dir = bridge_cfg.get("script_output_dir", "./serverdata/script-output")
        out_path = Path(os.path.expanduser(out_dir))
        if not out_path.is_absolute():
            out_path = (Path(__file__).parent.parent / out_path).resolve()
        self.bus_path = out_path / BUS_FILE

        rc = cfg.get("rcon", {})
        self.rcon = RconClient(rc.get("host", "127.0.0.1"), rc.get("port", 27015),
                               rc.get("password", ""))
        self.history = defaultdict(list)
        self.offset = 0
        self.replay = replay
        self.last_heartbeat = 0.0

    # ---- game I/O -------------------------------------------------------

    def call_mod(self, fn: str, payload: dict):
        body = json.dumps(payload, ensure_ascii=True, separators=(",", ":"))
        cmd = f"/silent-command remote.call('llm_scout','{fn}','{lua_quote(body)}')"
        if len(cmd) > MAX_RCON_BODY:
            raise RconError(f"RCON command too long ({len(cmd)} bytes)")
        try:
            out = self.rcon.command(cmd)
            if out and out.strip():
                log.debug("rcon said: %s", out.strip()[:200])
        except RconError as exc:
            log.warning("rcon %s failed: %s", fn, exc)

    def heartbeat(self):
        now = time.time()
        if now - self.last_heartbeat < HEARTBEAT_SECONDS:
            return
        self.last_heartbeat = now
        self.call_mod("hello", {
            "backends": providers.BACKENDS,
            "selected": self.backend,
            "tier": self.tier_setting,
        })

    def send_text(self, req_id: int, text: str, final: bool = False):
        for piece in self._split_for_rcon(text):
            self.call_mod("deliver", {"id": req_id, "text": piece, "final": False})
        if final:
            self.call_mod("deliver", {"id": req_id, "text": "", "final": True})

    def send_error(self, req_id: int, message: str):
        self.call_mod("deliver", {"id": req_id, "error": message[:500], "final": True})

    @staticmethod
    def _split_for_rcon(text: str):
        """Chunk so each escaped RCON command stays under the packet budget."""
        budget = 1200
        while text:
            yield text[:budget]
            text = text[budget:]

    # ---- ping extraction ------------------------------------------------

    def drain(self, buffer: str, req_id: int, player_index: int, final: bool,
              collected: list | None = None):
        """Strip complete ping markers, collect them, return (emittable, leftover)."""
        pings = []

        def take(match):
            pings.append((float(match.group(1)), float(match.group(2)),
                          (match.group(3) or "LLM Scout").strip()))
            return ""

        buffer = PING_RE.sub(take, buffer)
        for x, y, label in pings:
            log.info("ping %s,%s %s", x, y, label)
            if collected is not None:
                collected.append({"x": x, "y": y, "text": label,
                                  "player_index": player_index, "id": req_id})

        if final:
            return buffer, ""

        # Hold back anything that might be the start of an unclosed marker.
        cut = buffer.rfind("[[")
        if cut != -1 and "]]" not in buffer[cut:]:
            return buffer[:cut], buffer[cut:]
        return buffer, ""

    # ---- request handling -----------------------------------------------

    def effective_tier(self) -> str:
        if self.tier_setting != "auto":
            return self.tier_setting
        return "compact" if self.backend in COMPACT_BACKENDS else "full"

    def build_messages(self, req: dict) -> list[dict]:
        key = req.get("player_index", 0)
        snapshot = json.dumps(req.get("snapshot", {}), separators=(",", ":"))
        user = (
            f"Current factory snapshot:\n{snapshot}\n\n"
            f"Player question: {req.get('question', '')}"
        )
        prior = self.history[key][-self.history_turns * 2:]
        return prior + [{"role": "user", "content": user}]

    def handle(self, req: dict):
        req_id = req.get("id")
        player_index = req.get("player_index", 1)
        question = (req.get("question") or "").strip()
        backend = req.get("backend") or self.backend
        if backend not in providers.BACKENDS:
            backend = self.backend
        self.backend = backend

        snap_bytes = len(json.dumps(req.get("snapshot", {})))
        log.info("q#%s [%s] %r (snapshot %.1f KB)", req_id, backend, question[:80],
                 snap_bytes / 1024)

        messages = self.build_messages(req)
        cfg = self.cfg.get(backend, {})

        buffer, emitted, pings = "", [], []
        pending, last_flush = "", time.time()
        started = time.time()

        try:
            for chunk in providers.stream(backend, cfg, SYSTEM, messages):
                buffer += chunk
                ready, buffer = self.drain(buffer, req_id, player_index, final=False,
                                           collected=pings)
                if ready:
                    pending += ready
                    emitted.append(ready)
                now = time.time()
                if pending and (len(pending) >= FLUSH_CHARS or now - last_flush >= FLUSH_SECONDS):
                    self.send_text(req_id, pending)
                    pending, last_flush = "", now

            ready, _ = self.drain(buffer, req_id, player_index, final=True,
                                  collected=pings)
            pending += ready
            emitted.append(ready)
            if pending:
                self.send_text(req_id, pending)
            self.call_mod("deliver", {"id": req_id, "text": "", "final": True})
            for ping in pings:
                self.call_mod("ping", ping)

        except providers.ProviderError as exc:
            log.error("provider failed: %s", exc)
            self.send_error(req_id, f"{backend}: {exc}")
            return
        except Exception as exc:  # noqa: BLE001 - surface anything into the game
            log.exception("unexpected failure")
            self.send_error(req_id, f"bridge error: {exc}")
            return

        answer = "".join(emitted).strip()
        key = req.get("player_index", 0)
        self.history[key].append({"role": "user", "content": question})
        self.history[key].append({"role": "assistant", "content": answer})
        self.history[key] = self.history[key][-self.history_turns * 2:]
        log.info("q#%s answered in %.1fs (%d chars)", req_id, time.time() - started, len(answer))

    # ---- main loop ------------------------------------------------------

    def run(self):
        self.bus_path.parent.mkdir(parents=True, exist_ok=True)
        if not self.bus_path.exists():
            self.bus_path.touch()
        self.offset = 0 if self.replay else self.bus_path.stat().st_size

        log.info("watching %s (from byte %d)", self.bus_path, self.offset)
        log.info("backend=%s tier=%s", self.backend, self.effective_tier())

        try:
            self.rcon.connect()
            log.info("rcon connected")
        except (RconError, OSError) as exc:
            log.warning("rcon not reachable yet (%s) - will retry", exc)

        carry = ""
        while True:
            try:
                self.heartbeat()
                size = self.bus_path.stat().st_size
                if size < self.offset:
                    log.info("bus file truncated, rewinding")
                    self.offset, carry = 0, ""
                if size > self.offset:
                    with open(self.bus_path, "r", encoding="utf-8", errors="replace") as fh:
                        fh.seek(self.offset)
                        data = fh.read()
                        self.offset = fh.tell()
                    carry += data
                    while "\n" in carry:
                        line, carry = carry.split("\n", 1)
                        line = line.strip()
                        if not line:
                            continue
                        try:
                            req = json.loads(line)
                        except json.JSONDecodeError as exc:
                            log.warning("skipping malformed bus line: %s", exc)
                            continue
                        if req.get("type") == "ask":
                            self.handle(req)
                time.sleep(self.poll)
            except KeyboardInterrupt:
                log.info("shutting down")
                return
            except FileNotFoundError:
                time.sleep(self.poll)
            except Exception:  # noqa: BLE001 - never let the watcher die
                log.exception("loop error")
                time.sleep(1.0)


def main():
    ap = argparse.ArgumentParser(description="llm-scout bridge daemon")
    ap.add_argument("--config", default=str(Path(__file__).parent.parent / "config.toml"))
    ap.add_argument("--backend", help="override the configured backend")
    ap.add_argument("--replay", action="store_true",
                    help="process the whole bus file instead of only new lines")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)-7s %(message)s",
        datefmt="%H:%M:%S",
    )

    cfg = load_config(Path(args.config))
    if args.backend:
        cfg.setdefault("bridge", {})["backend"] = args.backend
    Bridge(cfg, replay=args.replay).run()


if __name__ == "__main__":
    main()
