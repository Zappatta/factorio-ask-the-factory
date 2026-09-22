"""Streaming LLM backends. Each provider yields text chunks as they arrive."""
import json
import os
import subprocess
import urllib.request

BACKENDS = ["claude-cli", "anthropic-api", "ollama", "openai-compatible"]


class ProviderError(Exception):
    pass


def _flatten(system: str, messages: list[dict]) -> str:
    """Collapse a message list into one prompt, for backends that take a single string."""
    parts = []
    for m in messages[:-1]:
        parts.append(f"[{m['role']}]\n{m['content']}")
    parts.append(messages[-1]["content"])
    return "\n\n".join(parts)


def claude_cli(cfg, system, messages):
    model = cfg.get("model", "claude-sonnet-5")
    timeout = cfg.get("timeout", 180)
    cmd = [
        "claude", "-p",
        "--output-format", "stream-json",
        "--include-partial-messages",
        "--verbose",
        "--model", model,
        "--system-prompt", system,
        "--exclude-dynamic-system-prompt-sections",
        "--setting-sources", "",
        "--strict-mcp-config",
        "--mcp-config", '{"mcpServers":{}}',
        "--allowed-tools", "",
    ]
    proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, bufsize=1,
    )
    try:
        proc.stdin.write(_flatten(system, messages))
        proc.stdin.close()
    except BrokenPipeError:
        pass

    saw_text = False
    for line in proc.stdout:
        line = line.strip()
        if not line:
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue
        if evt.get("type") == "stream_event":
            inner = evt.get("event", {})
            if inner.get("type") == "content_block_delta":
                delta = inner.get("delta", {})
                if delta.get("type") == "text_delta":
                    saw_text = True
                    yield delta.get("text", "")
        elif evt.get("type") == "result" and evt.get("is_error"):
            raise ProviderError(evt.get("result") or "claude CLI reported an error")

    proc.wait(timeout=timeout)
    if proc.returncode != 0 and not saw_text:
        err = (proc.stderr.read() or "").strip()[:400]
        raise ProviderError(f"claude CLI exited {proc.returncode}: {err}")


def anthropic_api(cfg, system, messages):
    try:
        import anthropic
    except ImportError as exc:
        raise ProviderError("anthropic SDK not installed (pip install anthropic)") from exc

    key = cfg.get("api_key") or os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        raise ProviderError("no API key: set ANTHROPIC_API_KEY or config [anthropic-api].api_key")

    client = anthropic.Anthropic(api_key=key)
    with client.messages.stream(
        model=cfg.get("model", "claude-sonnet-5"),
        max_tokens=cfg.get("max_tokens", 2000),
        system=system,
        messages=messages,
    ) as stream:
        for text in stream.text_stream:
            yield text


def ollama(cfg, system, messages):
    host = cfg.get("host", "http://localhost:11434").rstrip("/")
    body = {
        "model": cfg.get("model", "llama3.1:8b"),
        "messages": [{"role": "system", "content": system}] + messages,
        "stream": True,
        "options": {"num_ctx": cfg.get("num_ctx", 16384)},
    }
    req = urllib.request.Request(
        f"{host}/api/chat",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=300) as resp:
            for raw in resp:
                raw = raw.strip()
                if not raw:
                    continue
                evt = json.loads(raw)
                if evt.get("error"):
                    raise ProviderError(evt["error"])
                chunk = evt.get("message", {}).get("content", "")
                if chunk:
                    yield chunk
                if evt.get("done"):
                    return
    except urllib.error.URLError as exc:
        raise ProviderError(f"ollama unreachable at {host}: {exc}") from exc


def openai_compatible(cfg, system, messages):
    base = cfg.get("base_url", "http://localhost:1234/v1").rstrip("/")
    body = {
        "model": cfg.get("model", "local-model"),
        "messages": [{"role": "system", "content": system}] + messages,
        "stream": True,
    }
    headers = {"Content-Type": "application/json"}
    if cfg.get("api_key"):
        headers["Authorization"] = f"Bearer {cfg['api_key']}"
    req = urllib.request.Request(
        f"{base}/chat/completions", data=json.dumps(body).encode(), headers=headers
    )
    try:
        with urllib.request.urlopen(req, timeout=300) as resp:
            for raw in resp:
                line = raw.decode("utf-8", errors="replace").strip()
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    return
                evt = json.loads(payload)
                delta = evt.get("choices", [{}])[0].get("delta", {})
                chunk = delta.get("content") or ""
                if chunk:
                    yield chunk
    except urllib.error.URLError as exc:
        raise ProviderError(f"endpoint unreachable at {base}: {exc}") from exc


DISPATCH = {
    "claude-cli": claude_cli,
    "anthropic-api": anthropic_api,
    "ollama": ollama,
    "openai-compatible": openai_compatible,
}


def stream(backend: str, cfg: dict, system: str, messages: list[dict]):
    fn = DISPATCH.get(backend)
    if fn is None:
        raise ProviderError(f"unknown backend '{backend}'")
    return fn(cfg, system, messages)
