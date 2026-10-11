#!/usr/bin/env python3
"""RM-00 (M0) direct-host boundary: Mac host checks plus a physical iPhone run.

Owns the failure cases in docs/pending-tasks/control-this-mac/rm-00/failures.md:

  host   H1-H5  identity stability, damaged identities, bind rules, the two
                harmless routes, plain HTTP;
  phone  P1-P9  ticket/endpoint validation, pinned trust, wrong identity,
                descriptor validation, route, changed address;
  bridge B1-B3  Rust core driving the Swift transport, late reply after
                cancel, reply after the core is released.

Prerequisites (built once, reused so the macOS firewall decision is stable):
  scripts/cargo.sh build --locked -p luna-remote --example probe
  scripts/ios/build-mobile-core.sh iphoneos apps/ios/RemoteProbe/Generated
  xcodebuild -project apps/ios/RemoteProbe.xcodeproj -scheme RemoteProbe \
    -destination id=<udid> -derivedDataPath target/ios-probe-dd build

    /usr/bin/python3 scripts/qa-remote-direct-m0.py --device <udid> [--run-dir DIR]
        [--bind-ip IP] [--host-only]

Use Apple's /usr/bin/python3: the scripted fixture host runs in this
process, and with the macOS firewall in stealth mode an ad-hoc-signed
interpreter (Homebrew) has its inbound LAN traffic silently dropped, which
shows up as phone timeouts for the P6/P7 cases.

Every negative phone case targets its own host, so the host logs alone show
whether a request (or any connection) happened, without clock correlation.
Writes report.json, host logs, the phone console and sha256.json into the run
directory; exit status 0 only when every check passes.
"""

import argparse
import base64
import hashlib
import json
import os
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROBE = ROOT / "target/debug/examples/probe"
APP = ROOT / "target/ios-probe-dd/Build/Products/Debug-iphoneos/RemoteProbe.app"
BUNDLE = "com.novnski.luna.ios.remoteprobe"
DESCRIPTOR = "/remote/v1/descriptor"

checks = []


def check(case_id, ok, detail):
    checks.append({"case": case_id, "passed": bool(ok), "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {case_id}: {detail}", flush=True)


def free_port(ip):
    with socket.socket() as s:
        s.bind((ip, 0))
        return s.getsockname()[1]


def listening(ip, port):
    with socket.socket() as s:
        s.settimeout(0.5)
        return s.connect_ex((ip, port)) == 0


class Host:
    """One probe host process with its JSON event log."""

    def __init__(self, name, run, identity, ip, port, advertise=()):
        self.name, self.ip, self.port = name, ip, port
        self.log_path = run / f"host-{name}.jsonl"
        args = [str(PROBE), "--identity", str(identity), "--bind", f"{ip}:{port}"]
        for url in advertise:
            args += ["--advertise", url]
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(args, stdout=self.log, stderr=subprocess.STDOUT)
        self.ready = self._wait_ready()

    def _wait_ready(self):
        for _ in range(100):
            events = self.events()
            if events:
                return events[0]
            if self.proc.poll() is not None:
                break
            time.sleep(0.05)
        events = self.events()
        return events[0] if events else {"event": "no-output"}

    def events(self):
        self.log.flush()
        lines = Path(self.log_path).read_text().splitlines()
        return [json.loads(line) for line in lines if line.startswith("{")]

    def stop(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGINT)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.log.close()


def run_refusal(identity, bind):
    proc = subprocess.run([str(PROBE), "--identity", str(identity), "--bind", bind],
                          capture_output=True, text=True, timeout=20)
    line = (proc.stdout.strip().splitlines() or ["{}"])[0]
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        event = {"raw": line}
    return proc.returncode, event


def openssl_pin(key_path):
    der = subprocess.run(["openssl", "pkey", "-in", str(key_path), "-pubout", "-outform", "DER"],
                         capture_output=True, check=True).stdout
    return hashlib.sha256(der).hexdigest()


def curl(url, pin_hex, *extra):
    pin = base64.b64encode(bytes.fromhex(pin_hex)).decode()
    proc = subprocess.run(["curl", "-sk", "-m", "5", "--pinnedpubkey", f"sha256//{pin}",
                           "-o", "-", "-w", "\n%{http_code}", *extra, url],
                          capture_output=True, text=True)
    body, _, status = proc.stdout.rpartition("\n")
    return proc.returncode, status, body


# --- Host-side checks (H1-H5) ----------------------------------------------

def host_checks(run, ip):
    ident = run / "identities"
    ident.mkdir(mode=0o700)
    a_dir = ident / "a"

    print("H1 identity stability")
    port = free_port(ip)
    first = Host("h1-first", run, a_dir, ip, port)
    first.stop()
    second = Host("h1-second", run, a_dir, ip, port)
    second.stop()
    same = (first.ready.get("pin") == second.ready.get("pin")
            and first.ready.get("target") == second.ready.get("target"))
    check("H1", same and first.ready.get("pin") == openssl_pin(a_dir / "tls-key.pem"),
          f"restart kept pin {str(first.ready.get('pin'))[:12]} and target; openssl agrees")

    print("H2 damaged identities")

    def fixture(name, make):
        d = ident / f"bad-{name}"
        shutil.copytree(a_dir, d)
        make(d)
        return d

    def chmod(path, mode):
        os.chmod(path, mode)

    def symlink_key(d):
        (d / "tls-key.pem").rename(d / "real.pem")
        (d / "tls-key.pem").symlink_to(d / "real.pem")

    def rsa_key(d):
        (d / "tls-key.pem").unlink()
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048",
                        "-out", str(d / "tls-key.pem")], capture_output=True, check=True)
        os.chmod(d / "tls-key.pem", 0o600)

    damaged = {
        "key-world-readable": (lambda d: chmod(d / "tls-key.pem", 0o644), "accessible by other users"),
        "dir-group-readable": (lambda d: chmod(d, 0o750), "accessible by other users"),
        "key-symlink": (symlink_key, "not a regular file"),
        "key-corrupt": (lambda d: (d / "tls-key.pem").write_text("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"), "not a P-256"),
        "key-rsa": (rsa_key, "not a P-256"),
        "target-invalid": (lambda d: (d / "target.json").write_text("{\"deviceId\": 1}"), "target is invalid"),
        "target-missing": (lambda d: (d / "target.json").unlink(), "incomplete"),
        "key-missing": (lambda d: (d / "tls-key.pem").unlink(), "incomplete"),
    }
    for name, (make, needle) in damaged.items():
        d = fixture(name, make)
        port = free_port(ip)
        status, event = run_refusal(d, f"{ip}:{port}")
        error = event.get("error", "")
        check(f"H2/{name}", status == 2 and event.get("event") == "refused" and needle in error
              and not listening(ip, port), f"exit {status}: {error}")

    print("H3 bind rules")
    lan_port = free_port(ip)
    blocker = socket.socket()
    blocker.bind((ip, lan_port))
    blocker.listen()
    loop_port = free_port("127.0.0.1")
    local_service = socket.socket()
    local_service.bind(("127.0.0.1", loop_port))
    local_service.listen()
    luna_ipc = 59777 if listening("127.0.0.1", 59777) else None
    binds = {
        "wildcard-v4": (f"0.0.0.0:{free_port(ip)}", "wildcard"),
        "wildcard-v6": (f"[::]:{free_port(ip)}", "wildcard"),
        "loopback": (f"127.0.0.1:{free_port(ip)}", "loopback"),
        "port-zero": (f"{ip}:0", "nonzero port"),
        "lan-port-in-use": (f"{ip}:{lan_port}", "already in use"),
        "loopback-port-in-use": (f"{ip}:{loop_port}", "in use on loopback"),
    }
    if luna_ipc:
        binds["luna-ipc-port"] = (f"{ip}:{luna_ipc}", "in use on loopback")
    for name, (bind, needle) in binds.items():
        status, event = run_refusal(a_dir, bind)
        check(f"H3/{name}", status == 2 and needle in event.get("error", ""),
              f"{bind}: exit {status}: {event.get('error')}")
    blocker.close()
    local_service.close()
    if luna_ipc:
        check("H3/luna-ipc-kept", listening("127.0.0.1", luna_ipc), "LUNA Dev still listens on its IPC port")

    print("H4/H5 routes and plain HTTP")
    host = Host("h4", run, a_dir, ip, free_port(ip))
    pin, base = host.ready["pin"], f"https://{ip}:{host.port}"
    _, status, body = curl(base + "/remote/v1/health", pin)
    check("H4/health", status == "200" and set(json.loads(body)) == {"protocol"}, f"{status} {body}")
    _, status, body = curl(base + DESCRIPTOR, pin)
    keys = set(json.loads(body)) if status == "200" else set()
    check("H4/descriptor", keys == {"protocol", "target", "catalogVersion", "spkiSha256"},
          f"{status} keys={sorted(keys)}")
    for name, path, extra, want in [
        ("unknown-path", "/nope", (), "404"),
        ("engine-method", "/EngineInfo", (), "404"),
        # Bodies are refused before routing, so an RPC frame never reaches a handler.
        ("rpc-frame", "/", ("-X", "POST", "--data", '{"id":1,"method":"EngineInfo"}'), "413"),
        ("post-descriptor", DESCRIPTOR, ("-X", "POST"), "405"),
        ("get-with-body", "/remote/v1/health", ("-X", "GET", "--data", "x"), "413"),
    ]:
        _, status, _ = curl(base + path, pin, *extra)
        check(f"H4/{name}", status == want, f"{path} -> {status}")
    code, _, _ = curl(base + "/remote/v1/health", "00" * 32)
    check("H4/curl-wrong-pin", code == 90, f"curl with a wrong pin exits {code}")
    plain = subprocess.run(["curl", "-s", "-m", "5", "-o", "/dev/null", "-w", "%{http_code}",
                            f"http://{ip}:{host.port}/remote/v1/health"], capture_output=True, text=True)
    time.sleep(0.2)
    failed = [e for e in host.events() if e.get("event") == "handshake" and not e.get("ok")]
    check("H5", plain.returncode != 0 and plain.stdout in ("", "000") and failed,
          f"plain HTTP: curl exit {plain.returncode}, host handshake failures {len(failed)}")
    host.stop()
    return a_dir


# --- Scripted hosts: an independent TLS server holding key A ---------------
# P6/P7 need descriptors the real host never sends; B2/B3 need a reply that is
# still in flight. Scripting them here keeps the product free of test hooks.

class Scripted:
    VARIANTS = ["unsupported-protocol", "unsupported-catalog", "descriptor-pin-mismatch",
                "oversized-descriptor", "malformed-descriptor", "http-500"]
    SLOW = "slow-descriptor"
    SLOW_SECS = 3

    def __init__(self, run, key, ip, descriptor, other_pin):
        cert = run / "scripted-cert.pem"
        subprocess.run(["openssl", "req", "-new", "-x509", "-key", str(key), "-subj", "/CN=scripted-fixture",
                        "-days", "2", "-out", str(cert)], capture_output=True, check=True)
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(cert, key)
        self.log_path = run / "host-scripted.jsonl"
        self.lock = threading.Lock()
        self.servers, self.ports = [], {}
        bodies = {
            "unsupported-protocol": (200, json.dumps({**descriptor, "protocol": "remote-direct-v9"})),
            "unsupported-catalog": (200, json.dumps({**descriptor, "catalogVersion": 99})),
            "descriptor-pin-mismatch": (200, json.dumps({**descriptor, "spkiSha256": other_pin})),
            "oversized-descriptor": (200, json.dumps({**descriptor, "padding": "x" * 65536})),
            "malformed-descriptor": (200, '{"protocol": "remote-direct-v1", "target": '),
            "http-500": (500, "{}"),
            self.SLOW: (200, json.dumps(descriptor)),
        }
        for variant in [*self.VARIANTS, self.SLOW]:
            status, body = bodies[variant]
            server = ThreadingHTTPServer((ip, 0), self._handler(variant, status, body.encode()))
            server.socket = self.context.wrap_socket(server.socket, server_side=True)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            self.servers.append(server)
            self.ports[variant] = server.server_address[1]

    def _handler(self, variant, status, body):
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if variant == outer.SLOW:
                    time.sleep(outer.SLOW_SECS)
                with outer.lock, open(outer.log_path, "a") as log:
                    log.write(json.dumps({"event": "request", "variant": variant, "path": self.path,
                                          "peer": f"{self.client_address[0]}:{self.client_address[1]}",
                                          "atMs": int(time.time() * 1000)}) + "\n")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        return Handler

    def requests(self, variant):
        if not self.log_path.exists():
            return []
        rows = [json.loads(line) for line in self.log_path.read_text().splitlines()]
        return [row for row in rows if row["variant"] == variant]

    def stop(self):
        for server in self.servers:
            server.shutdown()


# --- Phone run (P1-P9, B1-B3) ----------------------------------------------

def phone_run(run, ip, device, a_dir, mdns):
    ident = run / "identities"
    hosts = {}
    port = {name: free_port(ip) for name in ("a", "a2", "b", "c", "s")}
    hosts["a"] = Host("a", run, a_dir, ip, port["a"], advertise=[f"https://{mdns}:{port['a']}"])
    hosts["a2"] = Host("a2", run, a_dir, ip, port["a2"])
    hosts["b"] = Host("b", run, ident / "b", ip, port["b"])
    c_dir = ident / "c"
    c_dir.mkdir(mode=0o700)
    shutil.copy2(a_dir / "tls-key.pem", c_dir / "tls-key.pem")
    other = {**hosts["a"].ready["target"], "deviceId": str(uuid.uuid4()), "environmentId": str(uuid.uuid4())}
    (c_dir / "target.json").write_text(json.dumps(other))
    os.chmod(c_dir / "target.json", 0o600)
    hosts["c"] = Host("c", run, c_dir, ip, port["c"])
    hosts["s"] = Host("s", run, a_dir, ip, port["s"])
    for name, host in hosts.items():
        if host.ready.get("event") != "ready":
            check(f"setup/{name}", False, f"host {name} did not start: {host.ready}")
            return hosts, None
    ticket_a = hosts["a"].ready["ticket"]
    pin_a, pin_b = hosts["a"].ready["pin"], hosts["b"].ready["pin"]
    descriptor = json.loads(curl(f"https://{ip}:{port['a']}{DESCRIPTOR}", pin_a)[2])
    bad = Scripted(run, a_dir / "tls-key.pem", ip, descriptor, pin_b)

    def ticket(endpoints, **override):
        return json.dumps({**ticket_a, "endpoints": endpoints, **override})

    def at(name):
        return f"https://{ip}:{port[name]}"

    sentinel = at("s")
    slow = f"https://{ip}:{bad.ports[Scripted.SLOW]}"
    cases = [
        {"id": "verified-ip", "ticket": ticket([at("a")])},
        {"id": "verified-mdns", "ticket": ticket([f"https://{mdns}:{port['a']}"])},
        {"id": "wrong-pin", "ticket": ticket([at("a2")], spkiSha256=pin_b)},
        {"id": "swapped-host", "ticket": ticket([at("b")])},
        {"id": "same-key-other-target", "ticket": ticket([at("c")])},
        *({"id": v, "ticket": ticket([f"https://{ip}:{bad.ports[v]}"])} for v in Scripted.VARIANTS),
        {"id": "cancel-late", "ticket": ticket([slow]), "action": "cancelAfterMs", "ms": 500, "waitMs": 4000},
        {"id": "released-core", "ticket": ticket([slow]), "action": "releaseAfterMs", "ms": 300, "waitMs": 4500},
        {"id": "ticket-malformed", "ticket": "{not json " + sentinel},
        {"id": "ticket-oversized", "ticket": ticket([sentinel]) + " " * 5000},
        {"id": "ticket-version-2", "ticket": ticket([sentinel], version=2)},
        {"id": "ticket-bad-pin", "ticket": ticket([sentinel], spkiSha256=pin_a.upper())},
        {"id": "ticket-unknown-field", "ticket": ticket([sentinel], credential="not-in-m0")},
        {"id": "endpoint-http", "ticket": ticket([sentinel.replace("https", "http")])},
        {"id": "endpoint-ws", "ticket": ticket([sentinel.replace("https", "wss")])},
        {"id": "endpoint-path", "ticket": ticket([sentinel + "/remote/v1/descriptor"])},
        {"id": "endpoint-userinfo", "ticket": ticket([sentinel.replace("https://", "https://u:p@")])},
        {"id": "endpoint-query", "ticket": ticket([sentinel + "?x=1"])},
    ]
    for case in cases:
        case.setdefault("endpoint", 0)
    plan = {"runId": run.name, "prime": {"id": "prime", "ticket": ticket([at("a")]), "endpoint": 0},
            "primeTimeoutSecs": 150, "cases": cases}
    (run / "plan.json").write_text(json.dumps(plan, indent=1))

    print("installing and launching the probe app on the iPhone")
    install = subprocess.run(["xcrun", "devicectl", "device", "install", "app", "--device", device, str(APP)],
                             capture_output=True, text=True)
    (run / "install.log").write_text(install.stdout + install.stderr)
    if install.returncode:
        check("setup/install", False, f"devicectl install exit {install.returncode}")
        return hosts, bad
    env = json.dumps({"LUNA_RM00_PLAN": base64.b64encode(json.dumps(plan).encode()).decode(),
                      "LUNA_RM00_EXIT": "1"})
    console = run / "phone-console.log"
    with open(console, "w") as out:
        launch = subprocess.run(["xcrun", "devicectl", "device", "process", "launch", "--device", device,
                                 "--console", "--terminate-existing", "-e", env, BUNDLE],
                                stdout=out, stderr=subprocess.STDOUT, timeout=600)
    events = [json.loads(line.split("RM00 ", 1)[1]) for line in console.read_text().splitlines()
              if "RM00 {" in line]
    copy = subprocess.run(["xcrun", "devicectl", "device", "copy", "from", "--device", device,
                           "--domain-type", "appDataContainer", "--domain-identifier", BUNDLE,
                           "--source", "Documents/rm00-result.json",
                           "--destination", str(run / "phone-result.json")], capture_output=True, text=True)
    check("setup/launch", launch.returncode == 0 and any(e.get("event") == "done" for e in events),
          f"devicectl exit {launch.returncode}; result copied: {copy.returncode == 0}")
    time.sleep(0.5)
    evaluate(events, hosts, bad, ip, port, mdns, pin_a, pin_b)
    return hosts, bad


def evaluate(events, hosts, bad, ip, port, mdns, pin_a, pin_b):
    results = {e["id"]: e for e in events if e.get("event") == "case"}
    start = next((e for e in events if e.get("event") == "start"), {})
    phone_ips = set()

    def host_view(name):
        rows = hosts[name].events()
        return ([r for r in rows if r.get("event") == "handshake"],
                [r for r in rows if r.get("event") == "request"])

    print("P/B phone cases")
    target_a = hosts["a"].ready["target"]
    for case_id in ("verified-ip", "verified-mdns"):
        r = results.get(case_id, {})
        route = r.get("route") or {}
        local = (route.get("localAddress") or "").rsplit(":", 1)[0]
        phone_ips.add(local)
        ok = (r.get("outcome") == "verified" and r.get("deviceId") == target_a["deviceId"]
              and r.get("environmentId") == target_a["environmentId"]
              and (route.get("remoteAddress") or "").startswith(f"{ip}:")
              and route.get("interface") == "wifi" and route.get("proxied") is False)
        check(f"{'P8' if case_id == 'verified-ip' else 'P9'}/{case_id}", ok,
              f"{r.get('outcome')} via {route.get('remoteAddress')} from {route.get('localAddress')} "
              f"{route.get('interface')} proxied={route.get('proxied')} {route.get('tlsVersion')}")
    _, requests_a = host_view("a")
    peers_a = {r["peer"].rsplit(":", 1)[0] for r in requests_a if r["path"] == DESCRIPTOR}
    check("P8/host-peer", phone_ips and phone_ips <= peers_a and ip not in phone_ips,
          f"host A saw descriptor reads from {sorted(peers_a)}; phone reported {sorted(phone_ips)}")
    threads = results.get("verified-ip", {}).get("states", [])
    check("B1", results.get("verified-ip", {}).get("outcome") == "verified"
          and any(s.startswith("verified@") for s in threads),
          f"core states via listener: {threads}")

    for case_id, host, presented in (("wrong-pin", "a2", pin_a), ("swapped-host", "b", pin_b)):
        r = results.get(case_id, {})
        trust = [e for e in r.get("transportEvents", []) if e.get("event") == "trust"]
        handshakes, requests = host_view(host)
        ok = (r.get("outcome") == "rejected" and r.get("reason") == "trustRejected"
              and trust and trust[0].get("accepted") == "false"
              and trust[0].get("presentedSpkiSha256") == presented
              and handshakes and not requests)
        check(f"{'P3' if case_id == 'wrong-pin' else 'P4'}/{case_id}", ok,
              f"{r.get('outcome')}/{r.get('reason')}; presented {str(trust[0].get('presentedSpkiSha256'))[:12] if trust else None}; "
              f"host {host}: {len(handshakes)} handshake(s), {len(requests)} request(s)")

    r = results.get("same-key-other-target", {})
    _, requests_c = host_view("c")
    check("P5", r.get("outcome") == "rejected" and r.get("reason") == "targetMismatch" and requests_c,
          f"{r.get('outcome')}/{r.get('reason')}: {r.get('detail')}; host C requests {len(requests_c)}")

    expected = {"unsupported-protocol": "unsupportedVersion", "unsupported-catalog": "unsupportedVersion",
                "descriptor-pin-mismatch": "pinMismatch", "oversized-descriptor": "body",
                "malformed-descriptor": "body", "http-500": "httpStatus"}
    for variant, reason in expected.items():
        r = results.get(variant, {})
        served = bad.requests(variant)
        check(f"{'P6' if reason in ('unsupportedVersion', 'pinMismatch') else 'P7'}/{variant}",
              r.get("outcome") == "rejected" and r.get("reason") == reason and served,
              f"{r.get('outcome')}/{r.get('reason')}: {r.get('detail')}; served {len(served)}")

    # The cancelled request's own completion still reaches the core
    # ("delivered" after "cancel"); the state staying "cancelled" shows the
    # core ignored it.
    r = results.get("cancel-late", {})
    names = [e.get("event") for e in r.get("transportEvents", [])]
    late = "cancel" in names and "delivered" in names[names.index("cancel"):]
    check("B2", r.get("outcome") == "cancelled" and late,
          f"{r.get('outcome')}; transport {names}; states {r.get('states')}")
    r = results.get("released-core", {})
    names = [e.get("event") for e in r.get("transportEvents", [])]
    check("B3", r.get("outcome") == "released" and r.get("coreDeallocated") is True and "dropped" in names,
          f"{r.get('outcome')}; coreDeallocated={r.get('coreDeallocated')}; transport {names}")

    for case_id in ("ticket-malformed", "ticket-oversized", "ticket-version-2", "ticket-bad-pin",
                    "ticket-unknown-field", "endpoint-http", "endpoint-ws", "endpoint-path",
                    "endpoint-userinfo", "endpoint-query"):
        r = results.get(case_id, {})
        prefix = "ticket:" if case_id.startswith("ticket") else "endpoint:"
        check(f"{'P1' if prefix == 'ticket:' else 'P2'}/{case_id}",
              r.get("outcome") == "constructorError" and str(r.get("error", "")).startswith(prefix),
              f"{r.get('outcome')}: {r.get('error')}")
    handshakes_s, requests_s = host_view("s")
    check("P1P2/no-network", not handshakes_s and not requests_s,
          f"sentinel host saw {len(handshakes_s)} connection(s), {len(requests_s)} request(s)")
    check("P8/interface", start.get("interface") == "wifi", f"phone default path: {start.get('interface')}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--device")
    parser.add_argument("--run-dir", type=Path)
    parser.add_argument("--bind-ip")
    parser.add_argument("--host-only", action="store_true")
    args = parser.parse_args()
    ip = args.bind_ip or subprocess.run(["ipconfig", "getifaddr", "en0"], capture_output=True,
                                        text=True).stdout.strip()
    mdns = subprocess.run(["scutil", "--get", "LocalHostName"], capture_output=True,
                          text=True).stdout.strip() + ".local"
    run = args.run_dir or ROOT / "target/qa/control-this-mac" / time.strftime("rm00-%Y%m%dT%H%M%SZ", time.gmtime())
    run.mkdir(parents=True)
    os.chmod(run, 0o700)
    print(f"run {run} on {ip} ({mdns})")
    a_dir = host_checks(run, ip)
    hosts, bad = {}, None
    if not args.host_only:
        if not args.device:
            sys.exit("--device is required unless --host-only")
        try:
            hosts, bad = phone_run(run, ip, args.device, a_dir, mdns)
        finally:
            for host in hosts.values():
                host.stop()
            if bad:
                bad.stop()
    passed = all(c["passed"] for c in checks)
    report = {"runId": run.name, "bindIp": ip, "mdns": mdns, "device": args.device,
              "hostOnly": args.host_only, "checks": checks, "passed": passed,
              "counts": {"passed": sum(c["passed"] for c in checks), "total": len(checks)}}
    (run / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    hashes = {str(p.relative_to(run)): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sorted(run.rglob("*")) if p.is_file() and "identities" not in p.parts}
    (run / "sha256.json").write_text(json.dumps(hashes, indent=1) + "\n")
    print(json.dumps(report["counts"]), "passed" if passed else "FAILED")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
