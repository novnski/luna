"""Headed RM-01 readback. Run with agent-python; secrets stay in memory."""

import argparse
import hashlib
import http.client
import json
import socket
import ssl
import subprocess
import time
import uuid
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import serialization
from websockets.exceptions import ConnectionClosed, InvalidStatus
from websockets.sync.client import connect

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "--bind", required=True, help="Concrete LAN IPv4 address and free port"
)
parser.add_argument("--ipc-port", type=int, default=59777)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
OUT = args.output.resolve()
OUT.mkdir(parents=True, exist_ok=True)
host, port = args.bind.rsplit(":", 1)
BIND = (host, int(port))
ADDRESS = f"{host}:{port}"
checks = []
sessions = []


def ready_local():
    # Refresh launches the app before its IPC listener is ready. Wait for
    # that actual boundary, bounded to 15 seconds, rather than a fixed delay.
    deadline = time.monotonic() + 15
    while True:
        try:
            return connect(
                f"ws://127.0.0.1:{args.ipc_port}",
                proxy=None,
                open_timeout=5,
                legacy=True,
            )
        except ConnectionRefusedError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.1)


local = ready_local()
seq = 0


def rpc(ws, method, params):
    global seq
    seq += 1
    ws.send(json.dumps({"id": seq, "method": method, "params": params}))
    while True:
        frame = json.loads(ws.recv(timeout=5))
        if frame.get("id") == seq:
            return frame


def owner(method, params=None):
    frame = rpc(local, method, params or {})
    assert "err" not in frame, f"local operation refused: {method}"
    return frame["ok"]


def trusted_socket(pin):
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE  # Trust is the exact host-owned SPKI pin below.
    tcp = socket.create_connection(BIND, timeout=5)
    tls = ctx.wrap_socket(tcp, server_hostname=BIND[0])
    certificate = x509.load_der_x509_certificate(tls.getpeercert(binary_form=True))
    spki = certificate.public_key().public_bytes(
        serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
    )
    if hashlib.sha256(spki).hexdigest() != pin:
        tls.close()
        raise ValueError("TLS pin mismatch")
    return tls


def post(pin, path, body, token=None):
    connection = http.client.HTTPConnection(*BIND, timeout=5)
    connection.sock = trusted_socket(pin)  # Pin checked before sending any credential.
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    try:
        connection.request("POST", "/remote/v1/" + path, json.dumps(body), headers)
        reply = connection.getresponse()
        return reply.status, json.loads(reply.read())
    finally:
        connection.close()


def remote(pin, ticket):
    # The already-authenticated TLS socket carries the HTTP upgrade. Using ws
    # here prevents a second TLS handshake on the pre-wrapped socket.
    return connect(
        f"ws://{ADDRESS}/remote/v1/ws",
        sock=trusted_socket(pin),
        proxy=None,
        additional_headers={"Authorization": "Ticket " + ticket},
        open_timeout=5,
        legacy=True,
    )


def pair(pin, label):
    issued = owner(
        "CreateRemotePairing",
        {
            "label": label,
            "scopes": ["environment-read"],
            "pairingTtlSecs": 60,
            "sessionTtlSecs": 120,
        },
    )
    code, session = post(
        pin,
        "pair",
        {
            "target": issued["target"],
            "credential": issued["credential"],
            "clientLabel": label,
        },
    )
    assert code == 200, "native pairing must succeed"
    sessions.append(session["id"])
    return session


def ticket(pin, session):
    code, grant = post(
        pin, "socket-ticket", {"target": session["target"]}, session["sessionToken"]
    )
    assert code == 200, "ticket must be issued"
    assert grant["expiresAtMs"] <= int(time.time() * 1000) + 30_000
    return grant["ticket"]


try:
    owner("EngineReady")
    info = owner("EngineInfo")
    initial = owner("GetRemoteAccess")
    assert not initial["enabled"], (
        "disable remote access before running this isolated native check"
    )
    status = owner("SetRemoteAccess", {"enabled": True, "bind": ADDRESS})
    assert status["enabled"] and status["listening"]
    pin = status["spkiSha256"]
    try:
        trusted_socket("0" * 64)
    except ValueError:
        checks.append("wrong TLS SPKI pin refused before any credential")
    else:
        raise AssertionError("wrong pin accepted")
    a, b = pair(pin, "RM01 native QA A"), pair(pin, "RM01 native QA B")
    first = ticket(pin, a)
    tampered = first[:-1] + ("1" if first[-1] == "0" else "0")
    try:
        remote(pin, tampered).close()
    except InvalidStatus as error:
        assert error.response.status_code == 401
        assert (
            "invalid socket ticket signature"
            in json.loads(error.response.body)["error"]
        )
    else:
        raise AssertionError("a changed signed capability must be refused")
    checks.append("changed ticket signature refused without consuming the original")
    wa, wb = remote(pin, first), remote(pin, ticket(pin, b))
    assert rpc(wa, "EngineInfo", {})["ok"]["deviceId"] == info["deviceId"]
    assert rpc(wb, "EngineInfo", {})["ok"]["deviceId"] == info["deviceId"]
    checks.append(
        "two native headed sessions authenticate over pinned LAN TLS/WebSocket"
    )
    command_id = "rm01-native-denial-" + str(uuid.uuid4())
    denied = rpc(
        wa,
        "ExecuteTaskCommand",
        {
            "commandId": command_id,
            "taskId": "rm01-native-no-task",
            "intent": {"kind": "rename", "title": "denied"},
        },
    )
    assert "local only" in denied["err"]
    assert owner("GetTaskCommandReceipt", {"commandId": command_id}) is None
    checks.append("remote local-only command refused with no durable receipt")
    outstanding = ticket(pin, a)
    owner("RevokeRemoteSession", {"id": a["id"]})
    code, _ = post(pin, "session/check", {"target": a["target"]}, a["sessionToken"])
    assert code == 403
    revoked = remote(pin, outstanding)
    try:
        result = rpc(revoked, "EngineInfo", {})
        assert "revoked" in result.get("err", ""), "revoked call must be refused"
    except ConnectionClosed:
        pass
    finally:
        revoked.close()
    assert rpc(wb, "EngineInfo", {})["ok"]["deviceId"] == info["deviceId"]
    checks.append(
        "pre-revoke ticket upgrades once, revoked socket refuses calls, other session survives"
    )
    stopped = owner("SetRemoteAccess", {"enabled": False})
    assert not stopped["enabled"] and not stopped["listening"]
    assert owner("EngineInfo")["deviceId"] == info["deviceId"]
    try:
        socket.create_connection(BIND, timeout=2).close()
    except (ConnectionRefusedError, TimeoutError):
        pass
    else:
        raise AssertionError("disabled listener remains reachable")
    ports = subprocess.run(
        ["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN"],
        capture_output=True,
        check=False,
    )
    assert ports.returncode == 1 and not ports.stdout, (
        "disabled port has no kernel listener"
    )
    again = owner("SetRemoteAccess", {"enabled": True, "bind": ADDRESS})
    assert again["spkiSha256"] == pin
    code, _ = post(pin, "session/check", {"target": b["target"]}, b["sessionToken"])
    assert code == 200
    checks.append(
        "disable stops LAN listener and preserves local IPC; re-enable preserves pin and valid session"
    )
    result = {
        "ok": True,
        "checks": checks,
        "route": "same Mac via real LAN address",
        "physicalPhone": False,
        "ipcPort": args.ipc_port,
        "remoteAddress": ADDRESS,
        "spkiSha256": pin,
    }
except (
    AssertionError,
    OSError,
    ValueError,
    KeyError,
    ConnectionClosed,
    InvalidStatus,
    TimeoutError,
) as error:
    result = {"ok": False, "checks": checks, "errorType": type(error).__name__}
finally:
    for session_id in sessions:
        owner("RevokeRemoteSession", {"id": session_id})
    if "initial" in globals() and not initial["enabled"]:
        stopped = owner("SetRemoteAccess", {"enabled": False})
    else:
        stopped = owner("GetRemoteAccess")
    result["finalDisabled"] = not stopped["enabled"] and not stopped["listening"]
    local.close()
    (OUT / "native-readback.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
raise SystemExit(0 if result["ok"] and result["finalDisabled"] else 1)
