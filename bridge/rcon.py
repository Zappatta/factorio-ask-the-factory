"""Minimal Source RCON client, stdlib only."""
import socket
import struct
import threading

SERVERDATA_AUTH = 3
SERVERDATA_AUTH_RESPONSE = 2
SERVERDATA_EXECCOMMAND = 2
SERVERDATA_RESPONSE_VALUE = 0


class RconError(Exception):
    pass


class RconClient:
    def __init__(self, host: str, port: int, password: str, timeout: float = 10.0):
        self.host, self.port, self.password, self.timeout = host, port, password, timeout
        self._sock = None
        self._req_id = 0
        self._lock = threading.Lock()

    def connect(self):
        self.close()
        sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        sock.settimeout(self.timeout)
        self._sock = sock
        self._req_id = 0
        auth_id = self._send(SERVERDATA_AUTH, self.password)
        while True:
            resp_id, resp_type, _ = self._recv()
            if resp_type == SERVERDATA_AUTH_RESPONSE:
                if resp_id == -1:
                    self.close()
                    raise RconError("RCON authentication failed (wrong password)")
                if resp_id == auth_id:
                    return
            elif resp_type == SERVERDATA_RESPONSE_VALUE:
                continue
            else:
                raise RconError(f"unexpected packet type {resp_type} during auth")

    def close(self):
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None

    @property
    def connected(self) -> bool:
        return self._sock is not None

    def command(self, body: str) -> str:
        with self._lock:
            if self._sock is None:
                self.connect()
            try:
                sent_id = self._send(SERVERDATA_EXECCOMMAND, body)
                resp_id, _, payload = self._recv()
                if resp_id != sent_id:
                    resp_id, _, payload = self._recv()
                return payload
            except (OSError, struct.error) as exc:
                self.close()
                raise RconError(f"RCON transport failure: {exc}") from exc

    def _send(self, packet_type: int, body: str) -> int:
        self._req_id += 1
        req_id = self._req_id
        raw = body.encode("utf-8")
        payload = struct.pack("<ii", req_id, packet_type) + raw + b"\x00\x00"
        self._sock.sendall(struct.pack("<i", len(payload)) + payload)
        return req_id

    def _recv(self):
        size = struct.unpack("<i", self._read_exact(4))[0]
        if size < 10 or size > 8_388_608:
            raise RconError(f"implausible RCON packet size {size}")
        data = self._read_exact(size)
        req_id, packet_type = struct.unpack("<ii", data[:8])
        return req_id, packet_type, data[8:-2].decode("utf-8", errors="replace")

    def _read_exact(self, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = self._sock.recv(n - len(buf))
            if not chunk:
                raise RconError("RCON connection closed by server")
            buf += chunk
        return buf


def lua_quote(text: str) -> str:
    """Escape a string for embedding in a Lua single-quoted literal."""
    return text.replace("\\", "\\\\").replace("'", "\\'").replace("\n", "\\n").replace("\r", "")
