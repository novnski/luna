#!/usr/bin/env bash
# Prepare a second signed local release for a human-driven Updates-screen test.
# This script never launches, quits, or replaces an application bundle.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_EVIDENCE="${1:-$ROOT/target/update-e2e/20260928T150548Z-9292}"
APP="$ROOT/target/luna-macos-dev/LUNA QA Updates.app"
APP_ID='com.novnski.luna.qa.updates'
OLD_VERSION='0.2.74'
NEW_VERSION='0.2.75'
IPC_PORT=59788
DATA_DIR="$ROOT/target/luna-qa-data/updates"
EVIDENCE="$ROOT/target/update-e2e/ui-$(date -u +%Y%m%dT%H%M%SZ)-$$"
FIXTURE="$EVIDENCE/fixture"
SERVER_PID=''
LAUNCH_LABEL=''
LAUNCH_DOMAIN="gui/$(id -u)"
SERVER_REGISTERED=false
READY=false

mkdir -p "$EVIDENCE"
umask 077
on_exit() {
  local code=$?
  if [[ "$READY" != true && "$SERVER_REGISTERED" == true ]]; then
    launchctl bootout "$LAUNCH_DOMAIN/$LAUNCH_LABEL" 2>/dev/null || true
  fi
  if [[ "$code" -ne 0 ]]; then
    printf 'failed with exit %s\n' "$code" >"$EVIDENCE/status.txt"
    printf 'UI fixture preparation failed; evidence: %s\n' "$EVIDENCE" >&2
  fi
}
trap on_exit EXIT

fail() { printf 'UI fixture: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "missing $1"; }
for tool in codesign curl ditto launchctl lsof openssl plutil python3 shasum tar xxd; do need "$tool"; done
[[ "$(uname -s)" == Darwin ]] || fail 'macOS is required'
[[ -d "$APP" ]] || fail "QA bundle is missing: $APP"
[[ -f "$SOURCE_EVIDENCE/result.json" ]] || fail 'passed E2E evidence is missing'
[[ -f "$SOURCE_EVIDENCE/test-signing.pem" ]] || fail 'test signing private key is missing'
[[ -f "$SOURCE_EVIDENCE/test-signing-public.pem" ]] || fail 'test signing public key is missing'
[[ -f "$SOURCE_EVIDENCE/test-signing-public.hex" ]] || fail 'test signing public hex is missing'
[[ "$(plutil -extract CFBundleIdentifier raw "$APP/Contents/Info.plist")" == "$APP_ID" ]] \
  || fail 'source bundle identifier is not the QA identity'
[[ "$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")" == "$OLD_VERSION" ]] \
  || fail "source QA bundle is not $OLD_VERSION"
[[ -x "$APP/Contents/MacOS/luna" ]] || fail 'source QA executable is missing'
codesign --verify --deep --strict "$APP" >"$EVIDENCE/source-codesign.log" 2>&1 \
  || fail 'source QA signature is invalid'

# The parent coordinates all GUI lifecycle. Refuse to copy a live app and
# explicitly leave the primary app alone.
while IFS= read -r pid; do
  [[ "$pid" =~ ^[0-9]+$ ]] || continue
  command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  if [[ "$command_line" =~ ^/.+\.app/Contents/MacOS/luna([[:space:]]|$) ]]; then
    fail "Luna app PID $pid is running; let the coordinator manage its quit"
  fi
done < <(pgrep -f '/Contents/MacOS/luna' 2>/dev/null || true)
if lsof -nP -iTCP:"$IPC_PORT" -sTCP:LISTEN >"$EVIDENCE/source-port.txt" 2>&1; then
  fail "QA IPC port $IPC_PORT is still in use"
fi

python3 - "$SOURCE_EVIDENCE/result.json" "$OLD_VERSION" <<'PY'
import json, pathlib, sys
result = json.loads(pathlib.Path(sys.argv[1]).read_text())
if result.get('status') != 'passed' or not any(
    c.get('name') == 'valid-install' and c.get('exit_code') == 0
    for c in result.get('cases', [])
):
    raise SystemExit('source evidence did not pass the install case')
PY
TEST_KEY="$SOURCE_EVIDENCE/test-signing.pem"
PUBLIC_PEM="$SOURCE_EVIDENCE/test-signing-public.pem"
PUBLIC_HEX="$(cat "$SOURCE_EVIDENCE/test-signing-public.hex")"
ACTUAL_HEX="$(openssl pkey -in "$TEST_KEY" -pubout -outform DER \
  | tail -c 32 | xxd -p -c 256)"
[[ "$ACTUAL_HEX" == "$PUBLIC_HEX" && "$PUBLIC_HEX" =~ ^[0-9a-f]{64}$ ]] \
  || fail 'test private/public key mismatch'
openssl pkeyutl -verify -rawin -pubin -inkey "$PUBLIC_PEM" \
  -in "$SOURCE_EVIDENCE/fixture/valid/manifest.json" \
  -sigfile "$SOURCE_EVIDENCE/fixture/valid/manifest.sig" \
  >"$EVIDENCE/source-manifest-verify.log" 2>&1 \
  || fail 'source test key did not verify passed manifest'

mkdir -p "$FIXTURE/releases/download/v$NEW_VERSION" "$FIXTURE/feed" "$EVIDENCE/payload"
PAYLOAD="$EVIDENCE/payload/LUNA.app"
ditto "$APP" "$PAYLOAD"
plutil -replace CFBundleShortVersionString -string "$NEW_VERSION" \
  "$PAYLOAD/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$(date -u +%Y%m%d%H%M%S)" \
  "$PAYLOAD/Contents/Info.plist"
printf 'signed updater UI E2E payload %s\n' "$NEW_VERSION" \
  >"$PAYLOAD/Contents/Resources/update-e2e-marker.txt"
AUTHORITY="$(codesign -dv --verbose=4 "$APP" 2>&1 | sed -n 's/^Authority=//p' | head -1 || true)"
[[ -n "$AUTHORITY" ]] || AUTHORITY='-'
codesign --force --sign "$AUTHORITY" --identifier "$APP_ID" "$PAYLOAD" \
  >"$EVIDENCE/payload-sign.log" 2>&1 || fail 'could not sign UI payload'
codesign --verify --deep --strict "$PAYLOAD" >"$EVIDENCE/payload-verify.log" 2>&1 \
  || fail 'UI payload signature is invalid'
[[ "$(plutil -extract CFBundleIdentifier raw "$PAYLOAD/Contents/Info.plist")" == "$APP_ID" ]] \
  || fail 'UI payload identity changed'

ARCH="$(uname -m)"
ASSET="luna-$NEW_VERSION-macos-$ARCH-app.tar.gz"
ASSET_PATH="$FIXTURE/releases/download/v$NEW_VERSION/$ASSET"
COPYFILE_DISABLE=1 tar -czf "$ASSET_PATH" -C "$EVIDENCE/payload" LUNA.app
python3 - "$ASSET_PATH" <<'PY' >"$EVIDENCE/archive-members.txt"
import sys, tarfile
with tarfile.open(sys.argv[1], 'r:gz') as archive:
    for entry in archive:
        if entry.name != 'LUNA.app' and not entry.name.startswith('LUNA.app/'):
            raise SystemExit(f'archive has unexpected member: {entry.name!r}')
        print(entry.name)
PY
ASSET_SHA="$(shasum -a 256 "$ASSET_PATH" | awk '{print $1}')"
printf '%s  %s\n' "$ASSET_SHA" "releases/download/v$NEW_VERSION/$ASSET" \
  >"$EVIDENCE/asset.SHA256SUMS"

# A long-lived loopback server allows the coordinator to operate the real UI.
cat >"$EVIDENCE/server.py" <<'PY'
import http.server, pathlib, sys
root, port_file = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(root), **kwargs)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
port_file.write_text(str(server.server_port))
server.serve_forever()
PY
PYTHON="$(command -v python3)"
LAUNCH_LABEL="com.novnski.luna.qa.updates.feed.$(basename "$EVIDENCE" | tr -cd '[:alnum:]')"
python3 - "$EVIDENCE/server.plist" "$LAUNCH_LABEL" "$PYTHON" \
  "$EVIDENCE/server.py" "$FIXTURE" "$EVIDENCE/server.port" "$EVIDENCE/http.log" <<'PY'
import pathlib, plistlib, sys
path, label, python, server, root, port_file, log = sys.argv[1:]
with pathlib.Path(path).open('wb') as output:
    plistlib.dump({
        'Label': label,
        'ProgramArguments': [python, '-u', server, root, port_file],
        'RunAtLoad': True,
        'StandardOutPath': log,
        'StandardErrorPath': log,
    }, output)
PY
launchctl bootstrap "$LAUNCH_DOMAIN" "$EVIDENCE/server.plist" \
  >"$EVIDENCE/launchd-bootstrap.log" 2>&1 || fail 'could not register fixture server'
SERVER_REGISTERED=true
for _ in {1..50}; do
  [[ -s "$EVIDENCE/server.port" ]] && break
  sleep 0.1
done
[[ -s "$EVIDENCE/server.port" ]] || fail 'fixture server did not bind'
launchctl print "$LAUNCH_DOMAIN/$LAUNCH_LABEL" >"$EVIDENCE/launchd-print.txt" \
  || fail 'fixture server is not registered'
SERVER_PID="$(awk '/^[[:space:]]*pid = [0-9]+/ { print $3; exit }' "$EVIDENCE/launchd-print.txt")"
if [[ ! "$SERVER_PID" =~ ^[0-9]+$ ]] || ! kill -0 "$SERVER_PID" 2>/dev/null; then
  fail 'fixture server has no running PID'
fi
PORT="$(cat "$EVIDENCE/server.port")"
BASE="http://127.0.0.1:$PORT"
FEED="$BASE/feed"
URL="$BASE/releases/download/v$NEW_VERSION/$ASSET"
python3 - "$FIXTURE/feed/manifest.json" "$NEW_VERSION" "$ASSET" "$ASSET_SHA" "$URL" <<'PY'
import json, pathlib, sys
path, version, name, digest, url = sys.argv[1:]
manifest = {'version': version, 'files': {name: {'sha256': digest, 'url': url}}}
pathlib.Path(path).write_bytes(json.dumps(manifest, separators=(',', ':')).encode())
PY
openssl pkeyutl -sign -rawin -inkey "$TEST_KEY" \
  -in "$FIXTURE/feed/manifest.json" -out "$FIXTURE/feed/manifest.sig" \
  >"$EVIDENCE/manifest-sign.log" 2>&1 || fail 'could not sign UI manifest'
[[ "$(wc -c <"$FIXTURE/feed/manifest.sig" | tr -d ' ')" == 64 ]] \
  || fail 'UI manifest signature is not 64 bytes'
openssl pkeyutl -verify -rawin -pubin -inkey "$PUBLIC_PEM" \
  -in "$FIXTURE/feed/manifest.json" -sigfile "$FIXTURE/feed/manifest.sig" \
  >"$EVIDENCE/manifest-verify.log" 2>&1 || fail 'UI manifest signature is invalid'
curl --fail --silent --show-error "$FEED/manifest.json" \
  -o "$EVIDENCE/served-manifest.json" || fail 'manifest was not served'
cmp "$FIXTURE/feed/manifest.json" "$EVIDENCE/served-manifest.json" \
  || fail 'served manifest bytes changed'
curl --fail --silent --show-error "$FEED/manifest.sig" \
  -o "$EVIDENCE/served-manifest.sig" || fail 'signature was not served'
cmp "$FIXTURE/feed/manifest.sig" "$EVIDENCE/served-manifest.sig" \
  || fail 'served signature bytes changed'

printf '%s\n' "$SERVER_PID" >"$EVIDENCE/server.pid"
printf '%s\n' "$LAUNCH_DOMAIN/$LAUNCH_LABEL" >"$EVIDENCE/server.launchd-target"
printf '%s\n' "$FEED" >"$EVIDENCE/feed.url"
printf '%s\n' "$PUBLIC_HEX" >"$EVIDENCE/test-signing-public.hex"
printf '%s\n' "$APP" >"$EVIDENCE/qa-app.path"
printf '%s\n' "$DATA_DIR" >"$EVIDENCE/qa-data.path"
python3 - "$EVIDENCE/result.json" "$FEED" "$SERVER_PID" "$APP" "$ASSET_SHA" "$PUBLIC_PEM" "$LAUNCH_DOMAIN/$LAUNCH_LABEL" <<'PY'
import json, pathlib, sys
path, feed, pid, app, digest, public_pem, target = sys.argv[1:]
pathlib.Path(path).write_text(json.dumps({
    'status': 'prepared', 'feed_url': feed, 'server_pid': int(pid),
    'launchd_target': target,
    'app_path': app, 'from_version': '0.2.74', 'to_version': '0.2.75',
    'asset_sha256': digest, 'test_public_key_pem': public_pem,
    'app_launched': False,
}, indent=2) + '\n')
PY
python3 - "$EVIDENCE" <<'PY' >"$EVIDENCE/SHA256SUMS"
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
for path in sorted(root.rglob('*')):
    if not path.is_file() or path.name in {'SHA256SUMS', 'http.log'}: continue
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''): digest.update(chunk)
    print(f'{digest.hexdigest()}  {path.relative_to(root)}')
PY
READY=true
printf 'Prepared signed UI fixture: %s\n' "$EVIDENCE"
printf 'Feed: %s\nPublic key: %s\nServer PID: %s\n' \
  "$FEED" "$EVIDENCE/test-signing-public.hex" "$SERVER_PID"
printf 'Stop server: launchctl bootout %s\n' "$LAUNCH_DOMAIN/$LAUNCH_LABEL"
printf 'QA app remains stopped. Coordinator may launch it with test feed/key.\n'
