#!/usr/bin/env bash
# Signed local GitHub-release fixture -> rejected updates -> QA app install/relaunch.
# This intentionally leaves the successful QA app running for native review.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME='LUNA QA Updates'
BUNDLE_ID='com.novnski.luna.qa.updates'
IPC_PORT=59788
DATA_DIR="$ROOT/target/luna-qa-data/updates"
APP="$ROOT/target/luna-macos-dev/$APP_NAME.app"
EXE="$APP/Contents/MacOS/luna"
EVIDENCE="$ROOT/target/update-e2e/$(date -u +%Y%m%dT%H%M%SZ)-$$"
SERVER_PID=''
STATE='preflight'

mkdir -p "$EVIDENCE"
finish() {
  local result=$?
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  RESULT_EXIT_CODE="$result" RESULT_STATE="$STATE" RESULT_EVIDENCE="$EVIDENCE" \
    python3 - <<'PY' >"$EVIDENCE/result.json"
import json, os, pathlib
evidence = pathlib.Path(os.environ['RESULT_EVIDENCE'])
cases = []
for line in (evidence / 'cases.tsv').read_text().splitlines() if (evidence / 'cases.tsv').exists() else []:
    name, expected, actual = line.split('\t')
    cases.append({'name': name, 'expected': expected, 'exit_code': int(actual)})
print(json.dumps({
    'status': 'passed' if os.environ['RESULT_EXIT_CODE'] == '0' else 'failed',
    'exit_code': int(os.environ['RESULT_EXIT_CODE']),
    'last_state': os.environ['RESULT_STATE'],
    'evidence': str(evidence),
    'cases': cases,
}, indent=2))
PY
  python3 - "$EVIDENCE" <<'PY' >"$EVIDENCE/SHA256SUMS"
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
for path in sorted(root.rglob('*')):
    if not path.is_file() or path.name == 'SHA256SUMS': continue
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''): digest.update(chunk)
    print(f'{digest.hexdigest()}  {path.relative_to(root)}')
PY
  printf 'Updater E2E %s; evidence: %s\n' \
    "$( [[ "$result" -eq 0 ]] && printf passed || printf failed )" "$EVIDENCE" >&2
}
trap finish EXIT

fail() { printf 'Updater E2E: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "missing $1"; }
for tool in codesign curl ditto lsof openssl plutil python3 rg shasum tar xxd; do need "$tool"; done
[[ "$(uname -s)" == Darwin ]] || fail 'macOS is required'
[[ -f "$ROOT/Cargo.toml" ]] || fail 'run from a LUNA checkout'

# Match the single-app rule before the expensive build. The packaging wrapper
# repeats this check before replacing the QA bundle.
while IFS= read -r pid; do
  [[ "$pid" =~ ^[0-9]+$ ]] || continue
  command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  if [[ "$command_line" =~ ^/.+\.app/Contents/MacOS/luna([[:space:]]|$) ]]; then
    fail "Luna app PID $pid is running; let it save and quit gracefully first"
  fi
done < <(pgrep -f '/Contents/MacOS/luna' 2>/dev/null || true)

STATE='budget'
DEVELOPER_DIR=/Library/Developer/CommandLineTools \
  python3 "$ROOT/scripts/build-budget.py" --status >"$EVIDENCE/build-budget.json"

STATE='build-qa-app'
export LUNA_DEV_APP_NAME="$APP_NAME" LUNA_DEV_BUNDLE_ID="$BUNDLE_ID"
export LUNA_DEV_IPC_PORT="$IPC_PORT" LUNA_DEV_DATA_DIR="$DATA_DIR"
if ! "$ROOT/scripts/run-macos-dev.sh" --build-only >"$EVIDENCE/build.log" 2>&1; then
  tail -n 30 "$EVIDENCE/build.log" >&2
  fail 'QA bundle build failed'
fi
[[ -x "$EXE" ]] || fail "missing QA executable at $EXE"
VERSION_PAIR="$(python3 - "$ROOT/Cargo.toml" <<'PY'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'(?m)^version = "(\d+)\.(\d+)\.(\d+)"$', text)
if not match: raise SystemExit('cannot read workspace version')
major, minor, patch = map(int, match.groups())
print(f'{major}.{minor}.{patch} {major}.{minor}.{patch + 1}')
PY
)" || fail 'cannot derive QA release versions'
read -r CURRENT_VERSION VERSION <<<"$VERSION_PAIR"
# The dev bundle labels its plist with `-luna.dev.N`; a real release uses a
# numeric version. Normalize this disposable QA bundle to model that install.
AUTHORITY="$(codesign -dv --verbose=4 "$APP" 2>&1 | sed -n 's/^Authority=//p' | head -1 || true)"
[[ -n "$AUTHORITY" ]] || AUTHORITY='-'
plutil -replace CFBundleShortVersionString -string "$CURRENT_VERSION" \
  "$APP/Contents/Info.plist"
codesign --force --sign "$AUTHORITY" --identifier "$BUNDLE_ID" "$APP" \
  >"$EVIDENCE/old-resign.log" 2>&1 || fail 'could not re-sign numeric-version QA bundle'
codesign --verify --deep --strict "$APP" >"$EVIDENCE/old-codesign.log" 2>&1 \
  || fail 'initial QA bundle signature failed'
OLD_BINARY_SHA="$(shasum -a 256 "$EXE" | awk '{print $1}')"
OLD_PLIST_SHA="$(shasum -a 256 "$APP/Contents/Info.plist" | awk '{print $1}')"
printf '%s\n' "$OLD_BINARY_SHA" >"$EVIDENCE/old-binary.sha256"
printf '%s\n' "$OLD_PLIST_SHA" >"$EVIDENCE/old-plist.sha256"

STATE='fixture'
ARCH="$(uname -m)"
ARTIFACT="luna-$VERSION-macos-$ARCH-app.tar.gz"
FIXTURE="$EVIDENCE/fixture"
mkdir -p "$FIXTURE/assets" "$FIXTURE/payload"
ditto "$APP" "$FIXTURE/payload/LUNA.app"
plutil -replace CFBundleShortVersionString -string "$VERSION" \
  "$FIXTURE/payload/LUNA.app/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$(date -u +%Y%m%d%H%M%S)" \
  "$FIXTURE/payload/LUNA.app/Contents/Info.plist"
printf 'signed updater E2E payload %s\n' "$VERSION" \
  >"$FIXTURE/payload/LUNA.app/Contents/Resources/update-e2e-marker.txt"
codesign --force --sign "$AUTHORITY" --identifier "$BUNDLE_ID" \
  "$FIXTURE/payload/LUNA.app" >"$EVIDENCE/payload-sign.log" 2>&1 \
  || fail 'could not sign payload with the installed app authority'
codesign --verify --deep --strict "$FIXTURE/payload/LUNA.app" \
  >"$EVIDENCE/payload-verify.log" 2>&1 || fail 'payload signature failed'
COPYFILE_DISABLE=1 tar -czf "$FIXTURE/assets/$ARTIFACT" -C "$FIXTURE/payload" LUNA.app
python3 - "$FIXTURE/assets/$ARTIFACT" <<'PY' >"$EVIDENCE/archive-members.txt"
import sys, tarfile
with tarfile.open(sys.argv[1], 'r:gz') as archive:
    for entry in archive:
        if entry.name != 'LUNA.app' and not entry.name.startswith('LUNA.app/'):
            raise SystemExit(f'archive has unexpected member: {entry.name!r}')
        print(entry.name)
PY
ASSET_SHA="$(shasum -a 256 "$FIXTURE/assets/$ARTIFACT" | awk '{print $1}')"
printf '%s  %s\n' "$ASSET_SHA" "assets/$ARTIFACT" >"$EVIDENCE/asset.SHA256SUMS"

umask 077
openssl genpkey -algorithm ED25519 -out "$EVIDENCE/test-signing.pem" \
  >"$EVIDENCE/keygen.log" 2>&1 || fail 'could not create local test key'
openssl pkey -in "$EVIDENCE/test-signing.pem" -pubout \
  -out "$EVIDENCE/test-signing-public.pem" \
  >"$EVIDENCE/pubkey.log" 2>&1 || fail 'could not export test public key'
PUBLIC_HEX="$(openssl pkey -in "$EVIDENCE/test-signing.pem" -pubout -outform DER \
  | tail -c 32 | xxd -p -c 256)"
[[ "$PUBLIC_HEX" =~ ^[0-9a-f]{64}$ ]] || fail 'invalid test public key'
printf '%s\n' "$PUBLIC_HEX" >"$EVIDENCE/test-signing-public.hex"

# The server binds an ephemeral loopback port; the manifest uses immutable
# versioned artifact URLs on that same origin, as the real release does.
PORT_FILE="$EVIDENCE/server.port"
python3 -u - "$FIXTURE" "$PORT_FILE" >"$EVIDENCE/http.log" 2>&1 <<'PY' &
import http.server, pathlib, sys
root, port_file = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(root), **kwargs)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
port_file.write_text(str(server.server_port))
server.serve_forever()
PY
SERVER_PID=$!
for _ in {1..50}; do
  [[ -s "$PORT_FILE" ]] && break
  kill -0 "$SERVER_PID" 2>/dev/null || fail 'fixture server stopped'
  sleep 0.1
done
[[ -s "$PORT_FILE" ]] || fail 'fixture server did not bind'
HTTP_PORT="$(cat "$PORT_FILE")"
BASE="http://127.0.0.1:$HTTP_PORT"
URL="$BASE/assets/$ARTIFACT"

make_manifest() {
  local case_name="$1" sha="$2" asset_url="$3" mode="${4:-normal}"
  local out="$FIXTURE/$case_name"
  mkdir -p "$out"
  python3 - "$out/manifest.json" "$VERSION" "$ARTIFACT" "$sha" "$asset_url" "$mode" <<'PY'
import json, pathlib, sys
path, version, name, sha, url, mode = sys.argv[1:]
meta = {'url': url}
if mode != 'missing-sha': meta['sha256'] = sha
pathlib.Path(path).write_bytes(json.dumps({'version': version, 'files': {name: meta}}, separators=(',', ':')).encode())
PY
  openssl pkeyutl -sign -rawin -inkey "$EVIDENCE/test-signing.pem" \
    -in "$out/manifest.json" -out "$out/manifest.sig" \
    >"$EVIDENCE/$case_name-sign.log" 2>&1
  [[ "$(wc -c <"$out/manifest.sig" | tr -d ' ')" == 64 ]] \
    || fail "signature is not 64 bytes for $case_name"
}
make_manifest valid "$ASSET_SHA" "$URL"
make_manifest wrong-sha "$(printf '%064d' 0)" "$URL"
make_manifest missing-sha "$ASSET_SHA" "$URL" missing-sha
make_manifest cross-origin "$ASSET_SHA" "https://example.invalid/releases/download/v$VERSION/$ARTIFACT"
mkdir -p "$FIXTURE/bad-archive"
printf 'this is deliberately not a gzip archive\n' >"$FIXTURE/bad-archive/$ARTIFACT"
BAD_ARCHIVE_SHA="$(shasum -a 256 "$FIXTURE/bad-archive/$ARTIFACT" | awk '{print $1}')"
make_manifest bad-archive "$BAD_ARCHIVE_SHA" "$BASE/bad-archive/$ARTIFACT"
cp -R "$FIXTURE/valid" "$FIXTURE/tampered"
python3 - "$FIXTURE/tampered/manifest.json" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
data = path.read_bytes()
path.write_bytes(data.replace(b'"version":"', b'"version":"9', 1))
PY
cp -R "$FIXTURE/valid" "$FIXTURE/bad-signature"
python3 - "$FIXTURE/bad-signature/manifest.sig" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
data = bytearray(path.read_bytes())
data[0] ^= 1
path.write_bytes(data)
PY
mkdir -p "$FIXTURE/missing-signature"
cp "$FIXTURE/valid/manifest.json" "$FIXTURE/missing-signature/manifest.json"

openssl pkeyutl -verify -rawin -pubin -inkey "$EVIDENCE/test-signing-public.pem" \
  -in "$FIXTURE/valid/manifest.json" -sigfile "$FIXTURE/valid/manifest.sig" \
  >"$EVIDENCE/valid-verify.log" 2>&1 || fail 'fixture signature verification failed'
curl --fail --silent --show-error "$BASE/valid/manifest.json" \
  -o "$EVIDENCE/served-manifest.json" || fail 'fixture HTTP server unavailable'
cmp "$FIXTURE/valid/manifest.json" "$EVIDENCE/served-manifest.json" \
  || fail 'served manifest bytes changed'

check_unchanged() {
  [[ "$(shasum -a 256 "$EXE" | awk '{print $1}')" == "$OLD_BINARY_SHA" ]] \
    || fail 'QA executable changed after a rejected update'
  [[ "$(shasum -a 256 "$APP/Contents/Info.plist" | awk '{print $1}')" == "$OLD_PLIST_SHA" ]] \
    || fail 'QA Info.plist changed after a rejected update'
  [[ ! -e "$APP/Contents/Resources/update-e2e-marker.txt" ]] \
    || fail 'QA marker appeared after a rejected update'
  codesign --verify --deep --strict "$APP" >/dev/null 2>&1 \
    || fail 'QA signature changed after a rejected update'
}

run_case() {
  local name="$1" expected="$2" action="$3" feed="${4:-$1}"
  local code=0
  local -a args=(update)
  [[ -z "$action" ]] || args+=("$action")
  STATE="case-$name"
  env LUNA_RELEASES_URL="$BASE/$feed" \
    LUNA_UPDATE_TEST_PUBLIC_KEY_HEX="$PUBLIC_HEX" \
    LUNA_DATA_DIR="$DATA_DIR" LUNA_IPC_PORT="$IPC_PORT" \
    "$EXE" "${args[@]}" >"$EVIDENCE/$name.log" 2>&1 || code=$?
  printf '%s\t%s\t%s\n' "$name" "$expected" "$code" >>"$EVIDENCE/cases.tsv"
  if [[ "$expected" == reject && "$code" -eq 0 ]]; then
    fail "$name unexpectedly succeeded (see $EVIDENCE/$name.log)"
  fi
  if [[ "$expected" == accept && "$code" -ne 0 ]]; then
    tail -n 30 "$EVIDENCE/$name.log" >&2
    fail "$name failed with exit $code"
  fi
  [[ "$expected" != reject ]] || check_unchanged
}

STATE='negative-checks'
run_case tampered reject --check
run_case bad-signature reject --check
run_case missing-signature reject --check
run_case missing-sha reject --check
run_case cross-origin reject --check
run_case wrong-sha reject ''
run_case bad-archive reject ''
run_case valid-check accept --check valid
rg -q "$VERSION" "$EVIDENCE/valid-check.log" \
  || fail 'valid check did not report the candidate version'
check_unchanged

STATE='install'
run_case valid-install accept '' valid
for _ in {1..120}; do
  PID="$(lsof -nP -t -iTCP:"$IPC_PORT" -sTCP:LISTEN 2>/dev/null | head -1 || true)"
  [[ -n "$PID" ]] && break
  sleep 1
done
[[ -n "${PID:-}" ]] || fail "updated QA app did not listen on $IPC_PORT"
COMMAND_LINE="$(ps -p "$PID" -o command=)"
[[ "$COMMAND_LINE" == "$EXE" || "$COMMAND_LINE" == "$EXE "* ]] \
  || fail "IPC listener $PID is not the exact QA executable: $COMMAND_LINE"
[[ "$(cat "$APP/Contents/Resources/update-e2e-marker.txt")" == "signed updater E2E payload $VERSION" ]] \
  || fail 'updated bundle marker is absent or incorrect'
[[ "$(plutil -extract CFBundleIdentifier raw "$APP/Contents/Info.plist")" == "$BUNDLE_ID" ]] \
  || fail 'updated bundle identifier changed'
[[ "$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")" == "$VERSION" ]] \
  || fail 'updated bundle version is wrong'
codesign --verify --deep --strict "$APP" >"$EVIDENCE/new-codesign.log" 2>&1 \
  || fail 'updated bundle failed strict code-sign verification'
lsof -nP -iTCP:"$IPC_PORT" -sTCP:LISTEN >"$EVIDENCE/ipc-listener.txt" \
  || fail 'could not capture updated app IPC listener'
plutil -p "$APP/Contents/Info.plist" >"$EVIDENCE/installed-info-plist.txt"
codesign -dv --verbose=4 "$APP" >"$EVIDENCE/installed-signature.txt" 2>&1
printf '%s\n' "$PID" >"$EVIDENCE/relaunch.pid"
printf '%s\n' "$COMMAND_LINE" >"$EVIDENCE/relaunch-command.txt"
printf '%s\n' "$VERSION" >"$EVIDENCE/installed-version.txt"
shasum -a 256 "$APP/Contents/MacOS/luna" "$APP/Contents/Info.plist" \
  "$APP/Contents/Resources/update-e2e-marker.txt" >"$EVIDENCE/installed.SHA256SUMS"
STATE='complete'
