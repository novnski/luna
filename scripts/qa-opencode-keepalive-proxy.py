#!/usr/bin/env python3
"""Keep-alive fault wrapper for `opencode serve` (S1 lost-answer repro).

Give this script to LUNA as the OpenCode executable (a harness probe's exe
argument, or OPENCODE_EXECUTABLE) with the real binary or QA profile wrapper
in LUNA_KA_REAL_OPENCODE:

  LUNA_KA_REAL_OPENCODE=<profile>/bin/opencode LUNA_KA_LOG=<dir>/proxy.jsonl \
      opencode_input_probe scripts/qa-opencode-keepalive-proxy.py <model> <prompt>

`serve`: the real server starts with the same arguments and environment
(stdin is inherited, so LUNA's `--stdio` ownership lease still works), and
its `{"url"}` readiness line is replaced by a loopback TCP proxy in front of
it. Any other invocation (`--version`, ...) execs the real binary unchanged.

The proxy passes bytes through untouched (LUNA's Authorization header
included; nothing but request lines is logged) with these faults:

- Idle reset (always on): a request that arrives on a client connection
  idle for at least LUNA_KA_IDLE_MS (default 6000) is reset (RST) without
  reaching the server. `opencode serve` 2.0.22 advertises `Keep-Alive:
  timeout=5` and closes an idle connection at 6.00 s; a request that meets
  that close is lost (evidence/opencode/e2e-paid-r2/s1/keepalive-probe). The
  proxy holds the server's idle close back from the client instead of
  racing a FIN against the request, so a client that reuses a connection
  past the server's timeout loses the request every time, not only when it
  hits the boundary.
- LUNA_KA_RESET_REPLIES=N: also reset the first N reply requests (`POST
  .../permission/<id>/reply`, `POST .../form/<id>/reply`) whatever their
  idle time: transient delivery failures.

LUNA_KA_LOG (optional) receives one JSON line per proxy event.
"""

from __future__ import annotations

import json
import os
import signal
import socket
import struct
import subprocess
import sys
import threading
import time

REAL = os.environ.get("LUNA_KA_REAL_OPENCODE", "")
IDLE_S = float(os.environ.get("LUNA_KA_IDLE_MS", "6000")) / 1000.0
RESET_REPLIES = int(os.environ.get("LUNA_KA_RESET_REPLIES", "0"))
LOG = os.environ.get("LUNA_KA_LOG")
METHODS = (b"GET", b"POST", b"PUT", b"PATCH", b"DELETE", b"HEAD", b"OPTIONS")

lock = threading.Lock()
replies_reset = 0
connections = 0


def log(event: str, **fields) -> None:
    if not LOG:
        return
    entry = {"t": round(time.time(), 3), "event": event, **fields}
    with lock, open(LOG, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry) + "\n")


def reset(sock: socket.socket) -> None:
    """Close with RST, as a server closing a connection with unread data."""
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
    except OSError:
        pass
    try:
        sock.close()
    except OSError:
        pass


def request_line(data: bytes):
    """(method, path) when `data` starts an HTTP/1.x request."""
    first = data.split(b"\r\n", 1)[0]
    parts = first.split(b" ")
    if len(parts) == 3 and parts[0] in METHODS and parts[2].startswith(b"HTTP/1."):
        return parts[0].decode(), parts[1].decode(errors="replace")
    return None


def is_reply(method: str, path: str) -> bool:
    bare = path.split("?", 1)[0]
    return method == "POST" and bare.endswith("/reply") and ("/permission/" in bare or "/form/" in bare)


class Connection:
    def __init__(self, client: socket.socket, upstream_addr: tuple[str, int], number: int):
        self.client = client
        self.upstream_addr = upstream_addr
        self.upstream = socket.create_connection(upstream_addr)
        self.number = number
        self.last = time.monotonic()
        self.requests = 0
        self.closed = False
        # The server closed an idle connection; the client has not seen it.
        self.upstream_closed = False

    def close(self, rst_client: bool = False) -> None:
        with lock:
            if self.closed:
                return
            self.closed = True
        if rst_client:
            reset(self.client)
        else:
            try:
                self.client.close()
            except OSError:
                pass
        try:
            self.upstream.close()
        except OSError:
            pass

    def pump_down(self) -> None:
        """Server -> client. The server's own keep-alive close of an idle
        connection is held back from the client, which therefore still
        believes the connection is reusable (the race, made deterministic)."""
        upstream = self.upstream
        try:
            while True:
                data = upstream.recv(65536)
                if not data:
                    break
                self.last = time.monotonic()
                self.client.sendall(data)
                self.last = time.monotonic()
        except OSError:
            pass
        idle = time.monotonic() - self.last
        if not self.closed and self.requests > 0 and idle >= IDLE_S - 0.5:
            self.upstream_closed = True
            log("upstream-idle-close", conn=self.number, idle_ms=round(idle * 1000), held=True)
            return
        self.close()

    def pump_up(self) -> None:
        """Client -> server, with the faults."""
        global replies_reset
        try:
            while True:
                data = self.client.recv(65536)
                if not data:
                    break
                started = request_line(data)
                if started:
                    method, path = started
                    idle = time.monotonic() - self.last
                    fields = {"conn": self.number, "method": method, "path": path,
                              "idle_ms": round(idle * 1000), "nth_on_conn": self.requests + 1}
                    if self.requests > 0 and idle >= IDLE_S:
                        log("reset-idle", **fields)
                        self.close(rst_client=True)
                        return
                    if is_reply(method, path):
                        with lock:
                            fault = replies_reset < RESET_REPLIES
                            if fault:
                                replies_reset += 1
                        if fault:
                            log("reset-reply", **fields, reset_number=replies_reset)
                            self.close(rst_client=True)
                            return
                    if self.upstream_closed:
                        # Not idle long enough to lose the request: carry
                        # it on a fresh server connection.
                        self.upstream = socket.create_connection(self.upstream_addr)
                        self.upstream_closed = False
                        threading.Thread(target=self.pump_down, daemon=True).start()
                    self.requests += 1
                    log("request", **fields)
                self.upstream.sendall(data)
                self.last = time.monotonic()
        except OSError:
            pass
        self.close()


def start_proxy(upstream_url: str) -> str:
    hostport = upstream_url.split("://", 1)[1].split("/", 1)[0]
    host, port = hostport.rsplit(":", 1)
    upstream_addr = (host.strip("[]"), int(port))
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(64)

    def accept_loop() -> None:
        global connections
        while True:
            try:
                client, _ = listener.accept()
            except OSError:
                return
            with lock:
                connections += 1
                number = connections
            try:
                conn = Connection(client, upstream_addr, number)
            except OSError as error:
                log("upstream-connect-failed", conn=number, error=str(error))
                reset(client)
                continue
            log("connect", conn=number)
            threading.Thread(target=conn.pump_down, daemon=True).start()
            threading.Thread(target=conn.pump_up, daemon=True).start()

    threading.Thread(target=accept_loop, daemon=True).start()
    return f"http://127.0.0.1:{listener.getsockname()[1]}"


def main() -> int:
    if not REAL:
        print("qa-opencode-keepalive-proxy: set LUNA_KA_REAL_OPENCODE", file=sys.stderr)
        return 64
    args = sys.argv[1:]
    if not args or args[0] != "serve":
        os.execv(REAL, [REAL, *args])
    child = subprocess.Popen([REAL, *args], stdout=subprocess.PIPE)

    def forward(signum, _frame):
        try:
            child.send_signal(signum)
        except OSError:
            pass

    signal.signal(signal.SIGTERM, forward)
    signal.signal(signal.SIGINT, forward)
    proxied = False
    assert child.stdout is not None
    for raw in child.stdout:
        line = raw.decode(errors="replace")
        if not proxied:
            try:
                url = json.loads(line).get("url")
            except (ValueError, AttributeError):
                url = None
            if isinstance(url, str) and url.startswith("http://"):
                proxy = start_proxy(url.rstrip("/"))
                proxied = True
                log("proxy", upstream=url, proxy=proxy, idle_ms=round(IDLE_S * 1000),
                    reset_replies=RESET_REPLIES, real=REAL)
                sys.stdout.write(json.dumps({"url": proxy}) + "\n")
                sys.stdout.flush()
                continue
        sys.stdout.write(line)
        sys.stdout.flush()
    return child.wait()


if __name__ == "__main__":
    sys.exit(main())
