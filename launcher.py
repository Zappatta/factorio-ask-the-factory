#!/usr/bin/env python3
"""Cross-platform launcher for LLM Scout.

Finds your Factorio install, lets you pick a save, starts an isolated local
server with RCON, starts the bridge daemon, and launches your client already
connected to it.

    python3 launcher.py                 pick a save interactively
    python3 launcher.py --save "world"  skip the picker
    python3 launcher.py --list          just list saves
    python3 launcher.py --no-client     start server + bridge only
    python3 launcher.py --export NAME   copy the session save back to your saves
"""
import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import tomllib
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SERVER_DIR = ROOT / "serverdata"
SESSION_SAVE = SERVER_DIR / "saves" / "session.zip"
IS_WINDOWS = sys.platform == "win32"


# ---- platform discovery -------------------------------------------------

def platform_paths():
    home = Path.home()
    if sys.platform == "darwin":
        data = home / "Library/Application Support/factorio"
        steam = home / "Library/Application Support/Steam"
        bins = [
            steam / "steamapps/common/Factorio/factorio.app/Contents/MacOS/factorio",
            Path("/Applications/factorio.app/Contents/MacOS/factorio"),
            home / "Applications/factorio.app/Contents/MacOS/factorio",
        ]
    elif IS_WINDOWS:
        data = Path(os.environ.get("APPDATA", home)) / "Factorio"
        steam = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Steam"
        bins = [
            steam / "steamapps/common/Factorio/bin/x64/factorio.exe",
            Path(r"C:\Program Files\Factorio\bin\x64\factorio.exe"),
            home / "Factorio/bin/x64/factorio.exe",
        ]
    else:
        data = home / ".factorio"
        steam = home / ".steam/steam"
        bins = [
            steam / "steamapps/common/Factorio/bin/x64/factorio",
            home / ".local/share/Steam/steamapps/common/Factorio/bin/x64/factorio",
            Path("/usr/share/factorio/bin/x64/factorio"),
            home / "factorio/bin/x64/factorio",
        ]
    return data, steam, bins


def steam_library_binaries(steam_root: Path):
    """Factorio may live in a secondary Steam library; libraryfolders.vdf lists them."""
    vdf = steam_root / "steamapps" / "libraryfolders.vdf"
    if not vdf.exists():
        return []
    found = []
    try:
        for line in vdf.read_text(errors="replace").splitlines():
            if '"path"' not in line:
                continue
            parts = line.split('"')
            if len(parts) < 4:
                continue
            base = Path(parts[3].replace("\\\\", "\\")) / "steamapps/common/Factorio"
            for rel in ("bin/x64/factorio.exe", "bin/x64/factorio",
                        "factorio.app/Contents/MacOS/factorio"):
                found.append(base / rel)
    except OSError:
        pass
    return found


def find_binary(override: str | None):
    if override:
        p = Path(override).expanduser()
        if p.is_file():
            return p
        die(f"FACTORIO_BIN set but not a file: {p}")
    _, steam, candidates = platform_paths()
    for p in candidates + steam_library_binaries(steam):
        if p.is_file():
            return p
    die("Could not find the Factorio executable.\n"
        "Set it explicitly:  FACTORIO_BIN=/path/to/factorio python3 launcher.py")


def find_data_dir(override: str | None):
    if override:
        p = Path(override).expanduser()
        if p.is_dir():
            return p
        die(f"FACTORIO_DIR set but not a directory: {p}")
    data, _, _ = platform_paths()
    if (data / "saves").is_dir() or (data / "mods").is_dir():
        return data
    die(f"Could not find your Factorio data directory (looked in {data}).\n"
        "Set it explicitly:  FACTORIO_DIR=/path/to/factorio python3 launcher.py")


def install_signal_handlers():
    """Explicit handlers: a process started in the background inherits SIGINT as
    ignored, and we want SIGTERM to shut down cleanly too."""
    def handler(signum, frame):
        raise KeyboardInterrupt
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, handler)
        except (ValueError, OSError):
            pass


def die(msg: str):
    print(f"\n{msg}\n", file=sys.stderr)
    sys.exit(1)


# ---- save listing -------------------------------------------------------

def human_size(n: int) -> str:
    return f"{n / 1024 / 1024:.1f} MB"


def human_age(ts: float) -> str:
    delta = time.time() - ts
    if delta < 90:
        return "just now"
    for limit, div, unit in ((3600, 60, "min"), (86400, 3600, "hour"), (2592000, 86400, "day")):
        if delta < limit:
            n = int(delta // div)
            return f"{n} {unit}{'s' if n != 1 else ''} ago"
    return datetime.fromtimestamp(ts).strftime("%d %b %Y")


def list_saves(data_dir: Path):
    saves_dir = data_dir / "saves"
    if not saves_dir.is_dir():
        die(f"No saves directory at {saves_dir}")
    rows = []
    for p in saves_dir.glob("*.zip"):
        st = p.stat()
        rows.append({"path": p, "name": p.stem, "mtime": st.st_mtime, "size": st.st_size})
    rows.sort(key=lambda r: r["mtime"], reverse=True)
    return rows


def print_saves(rows, limit=None):
    shown = rows if limit is None else rows[:limit]
    width = max((len(r["name"]) for r in shown), default=10)
    width = min(max(width, 12), 42)
    print(f"  {'#':>3}  {'save':<{width}}  {'modified':<14}  size")
    print(f"  {'-' * 3}  {'-' * width}  {'-' * 14}  {'-' * 8}")
    for i, r in enumerate(shown, 1):
        name = r["name"] if len(r["name"]) <= width else r["name"][:width - 1] + "…"
        auto = "  (autosave)" if r["name"].startswith("_autosave") else ""
        print(f"  {i:>3}  {name:<{width}}  {human_age(r['mtime']):<14}  "
              f"{human_size(r['size'])}{auto}")
    if limit is not None and len(rows) > limit:
        print(f"       ... and {len(rows) - limit} older (--list shows all)")


def pick_save(rows):
    print("\nWhich save?\n")
    print_saves(rows, limit=12)
    print()
    while True:
        try:
            raw = input("  number, or Enter for the newest, q to quit: ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            sys.exit(0)
        if raw.lower() in ("q", "quit", "exit"):
            sys.exit(0)
        if raw == "":
            return rows[0]
        if raw.isdigit() and 1 <= int(raw) <= min(len(rows), 12):
            return rows[int(raw) - 1]
        matches = [r for r in rows if r["name"].lower() == raw.lower()]
        if matches:
            return matches[0]
        print("  not a valid choice, try again")


# ---- server preparation -------------------------------------------------

def write_server_config():
    (SERVER_DIR / "saves").mkdir(parents=True, exist_ok=True)
    (SERVER_DIR / "script-output").mkdir(parents=True, exist_ok=True)
    (SERVER_DIR / "mods").mkdir(parents=True, exist_ok=True)
    (SERVER_DIR / "config.ini").write_text(
        "[path]\n"
        "read-data=__PATH__system-read-data__\n"
        f"write-data={SERVER_DIR}\n\n"
        "[general]\n"
        "locale=auto\n"
    )


def install_mod_into_client(client_mods: Path):
    """The connecting client needs the mod too, or multiplayer refuses to join."""
    version = json.loads((ROOT / "mod" / "info.json").read_text())["version"]
    target = client_mods / f"llm-scout_{version}"
    if target.exists():
        shutil.rmtree(target)
    shutil.copytree(ROOT / "mod", target)

    list_path = client_mods / "mod-list.json"
    data = json.loads(list_path.read_text()) if list_path.exists() else {"mods": []}
    for m in data["mods"]:
        if m["name"] == "llm-scout":
            m["enabled"] = True
            break
    else:
        data["mods"].append({"name": "llm-scout", "enabled": True})
    list_path.write_text(json.dumps(data, indent=2))
    return version


def mirror_mods(client_mods: Path, version: str):
    dst = SERVER_DIR / "mods"
    for f in dst.iterdir():
        shutil.rmtree(f) if f.is_dir() else f.unlink()

    enabled = {m["name"] for m in json.loads((client_mods / "mod-list.json").read_text())["mods"]
               if m["enabled"] and m["name"] != "base"}
    enabled.add("llm-scout")

    copied, missing = [], []
    for name in sorted(enabled):
        if name == "llm-scout":
            shutil.copytree(ROOT / "mod", dst / f"llm-scout_{version}")
            copied.append(f"llm-scout {version}")
            continue
        zips = sorted(client_mods.glob(f"{name}_*.zip"))
        if zips:
            shutil.copy(zips[-1], dst / zips[-1].name)
            copied.append(zips[-1].stem)
            continue
        folders = [p for p in client_mods.glob(f"{name}_*") if p.is_dir()]
        if folders:
            shutil.copytree(folders[-1], dst / folders[-1].name)
            copied.append(folders[-1].name)
        else:
            missing.append(name)

    (dst / "mod-list.json").write_text(json.dumps(
        {"mods": [{"name": "base", "enabled": True}] +
                 [{"name": n, "enabled": True} for n in sorted(enabled)]}, indent=2))
    settings = client_mods / "mod-settings.dat"
    if settings.exists():
        shutil.copy(settings, dst / "mod-settings.dat")
    return copied, missing


# ---- lifecycle ----------------------------------------------------------

def wait_for_server(proc, log_path: Path, timeout=120):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            tail = log_path.read_text(errors="replace").splitlines()[-25:]
            die("Server exited during startup:\n  " + "\n  ".join(tail))
        try:
            if "Hosting game at" in log_path.read_text(errors="replace"):
                return True
        except OSError:
            pass
        time.sleep(1)
    return False


def stop_server(proc, cfg):
    """Ask the server to quit over RCON so it writes a final save, then fall back."""
    if proc.poll() is not None:
        return
    try:
        sys.path.insert(0, str(ROOT / "bridge"))
        from rcon import RconClient
        rc = cfg["rcon"]
        client = RconClient(rc["host"], rc["port"], rc["password"], timeout=5)
        client.connect()
        client.command("/quit")
        client.close()
    except Exception:
        try:
            proc.send_signal(signal.CTRL_BREAK_EVENT if IS_WINDOWS else signal.SIGINT)
        except Exception:
            proc.terminate()
    try:
        proc.wait(timeout=45)
    except subprocess.TimeoutExpired:
        proc.kill()


def export_session(data_dir: Path, name: str):
    if not SESSION_SAVE.exists():
        die(f"No session save at {SESSION_SAVE}")
    dest = data_dir / "saves" / f"{name.removesuffix('.zip')}.zip"
    shutil.copy(SESSION_SAVE, dest)
    print(f"exported -> {dest}")
    print(f"It will appear in Factorio under Single player -> Load game as '{dest.stem}'.")


# ---- main ---------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="LLM Scout launcher")
    ap.add_argument("--save", help="save name, skips the picker")
    ap.add_argument("--list", action="store_true", help="list saves and exit")
    ap.add_argument("--no-client", action="store_true", help="do not launch the game client")
    ap.add_argument("--resume", action="store_true",
                    help="continue the existing session save instead of reseeding")
    ap.add_argument("--export", metavar="NAME", help="copy the session save back and exit")
    ap.add_argument("--backend", help="override the bridge backend for this run")
    args = ap.parse_args()

    try:
        sys.stdout.reconfigure(line_buffering=True)
    except (AttributeError, ValueError):
        pass
    install_signal_handlers()

    cfg = tomllib.load(open(ROOT / "config.toml", "rb"))
    data_dir = find_data_dir(os.environ.get("FACTORIO_DIR"))

    if args.export:
        export_session(data_dir, args.export)
        return

    saves = list_saves(data_dir)
    if args.list:
        print(f"\nsaves in {data_dir / 'saves'}\n")
        print_saves(saves)
        print()
        return
    if not saves and not args.resume:
        die(f"No saves found in {data_dir / 'saves'}")

    binary = find_binary(os.environ.get("FACTORIO_BIN"))
    print(f"Factorio : {binary}")
    print(f"Data dir : {data_dir}")

    chosen = None
    if args.resume and SESSION_SAVE.exists():
        print("Save     : resuming existing session")
    else:
        if args.save:
            matches = [r for r in saves if r["name"].lower() == args.save.removesuffix(".zip").lower()]
            if not matches:
                print(f"\nNo save named {args.save!r}. Available:\n")
                print_saves(saves, limit=15)
                sys.exit(1)
            chosen = matches[0]
        else:
            chosen = pick_save(saves)
        print(f"Save     : {chosen['name']}  ({human_age(chosen['mtime'])})")

    write_server_config()
    client_mods = data_dir / "mods"
    version = install_mod_into_client(client_mods)
    copied, missing = mirror_mods(client_mods, version)
    print(f"Mods     : {', '.join(copied)}")
    if missing:
        print(f"  WARNING: enabled but not found, the save may refuse to load: {', '.join(missing)}")

    if chosen is not None:
        shutil.copy(chosen["path"], SESSION_SAVE)

    rc = cfg["rcon"]
    log_path = ROOT / "server.log"
    cmd = [
        str(binary),
        "-c", str(SERVER_DIR / "config.ini"),
        "--mod-directory", str(SERVER_DIR / "mods"),
        "--start-server", str(SESSION_SAVE),
        "--server-settings", str(ROOT / "server-settings.json"),
        "--rcon-bind", f"{rc['host']}:{rc['port']}",
        "--rcon-password", rc["password"],
    ]
    creation = subprocess.CREATE_NEW_PROCESS_GROUP if IS_WINDOWS else 0
    print(f"\nStarting server on {rc['host']}:{rc['port']} ...")
    with open(log_path, "w") as log:
        server = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT,
                                  creationflags=creation)

    if not wait_for_server(server, log_path):
        stop_server(server, cfg)
        die(f"Server did not come up in time. See {log_path}")
    print("Server   : up")

    bridge_cmd = [sys.executable, str(ROOT / "bridge" / "bridge.py"),
                  "--config", str(ROOT / "config.toml")]
    if args.backend:
        bridge_cmd += ["--backend", args.backend]
    bridge = subprocess.Popen(bridge_cmd)

    client = None
    if not args.no_client:
        print("Client   : launching, connecting to localhost")
        client = subprocess.Popen(
            [str(binary), "--mp-connect", f"127.0.0.1:{cfg.get('server', {}).get('port', 34197)}"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    else:
        print("Client   : skipped. Connect manually to localhost")

    print("\n  In game: Ctrl+Shift+L, or the radar button top-left")
    print(f"  Server log: {log_path}")
    print("  Ctrl+C here to shut down cleanly (the server writes a final save)\n")

    try:
        while server.poll() is None:
            time.sleep(1)
    except KeyboardInterrupt:
        print("\nShutting down...")
    finally:
        if bridge.poll() is None:
            bridge.terminate()
        stop_server(server, cfg)
        if client and client.poll() is None:
            client.terminate()
        print("Stopped. Session save kept at serverdata/saves/session.zip")
        print(f"Export it back with:  python3 launcher.py --export \"my world\"")


if __name__ == "__main__":
    main()
