#!/usr/bin/env bash
# Subprocess acceptance contract; created before the implementation.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$ROOT/target/qa"
report="$(mktemp -d "$ROOT/target/qa/qa-infrastructure.XXXXXX")"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/luna-qa-e2e.XXXXXX")"
cleanup() {
  result=$?
  trap - EXIT
  rm -rf -- "$scratch"
  printf 'exit=%s\n' "$result" > "$report/result.txt"
  echo "QA infrastructure exit=$result; artifacts: $report"
  exit "$result"
}
trap cleanup EXIT
export QA_E2E_SCRATCH="$scratch" QA_E2E_REPORT="$report" QA_E2E_SOURCE="$ROOT"
python3 - <<'PY' > "$report/checks.log" 2>&1
import json, os, pathlib, shutil, subprocess, time
p = pathlib.Path
scratch, report, source = map(p, (os.environ['QA_E2E_SCRATCH'], os.environ['QA_E2E_REPORT'], os.environ['QA_E2E_SOURCE']))
scratch = scratch.resolve()
root = scratch / 'repo'
root.mkdir()
def call(args, **kw):
    return subprocess.run(args, text=True, capture_output=True, **kw)
def git(*args):
    r = call(['git', '-C', str(root), *args]); assert r.returncode == 0, r.stderr
git('init', '-q')
git('-c', 'user.name=QA', '-c', 'user.email=qa@example.invalid', 'commit', '--allow-empty', '-qm', 'fixture')
peer = scratch / 'peer'
git('worktree', 'add', '--detach', str(peer))
for repo in (root, peer):
    (repo / 'scripts').mkdir()
    for name in ('build-budget.py', 'run-macos-dev.sh'):
        shutil.copy2(source / 'scripts' / name, repo / 'scripts' / name)
    (repo / 'target').mkdir()
(root / 'Cargo.toml').write_text('version = "0.0.1"\n')
(peer / 'Cargo.toml').write_text('version = "0.0.1"\n')
shutil.copytree(source / 'dist' / 'macos', root / 'dist' / 'macos')
binpath = scratch / 'bin'; binpath.mkdir()
def script(path, content):
    path.write_text('#!/usr/bin/env bash\nset -eu\n' + content + '\n'); path.chmod(0o755)
script(root / 'scripts' / 'cargo.sh', 'echo build >> "$QA_E2E_SCRATCH/actions"; exit "${QA_BUILD_EXIT:-0}"')
script(binpath / 'codesign', 'exit "${QA_SIGN_EXIT:-0}"')
script(binpath / 'security', 'exit 0')
script(binpath / 'iconutil', 'exit 0')
script(binpath / 'open', 'printf "%s\\n" "$@" > "$QA_E2E_SCRATCH/open-args"; exit 1')
script(binpath / 'pgrep', 'if [[ "${QA_RUNNING:-0}" == 1 ]]; then echo 98765; fi; if [[ "${QA_OTHER_RUNNING:-0}" == 1 ]]; then echo 98766; fi')
script(binpath / 'ps', 'if [[ "${QA_RUNNING:-0}" == 1 && "$*" == "-p 98765 -o command=" ]]; then printf \'%s\\n\' "$QA_E2E_SCRATCH/repo/target/luna-macos-dev/LUNA QA Fixture.app/Contents/MacOS/luna"; elif [[ "${QA_OTHER_RUNNING:-0}" == 1 && "$*" == "-p 98766 -o command=" ]]; then printf \'%s\\n\' "$QA_E2E_SCRATCH/peer/target/luna-macos-dev/LUNA QA Other.app/Contents/MacOS/luna"; else /bin/ps "$@"; fi')
script(binpath / 'osascript', 'echo "$*" >> "$QA_E2E_SCRATCH/quit-request"; exit 1')
script(binpath / 'sleep', 'if [[ "${QA_RUNNING:-0}" != 1 ]]; then /bin/sleep "$@"; fi')
store = scratch / 'cache'; store.mkdir()
script(binpath / 'mbx', 'printf \'{"store":"%s/actions"}\\n\' "$QA_E2E_SCRATCH/cache"')
env = {**os.environ, 'QA_E2E_SCRATCH': str(scratch), 'PATH': str(binpath) + ':' + os.environ['PATH'],
       'LUNA_MBX_BIN': str(binpath / 'mbx'), 'LUNA_DEV_APP_NAME': 'LUNA QA Fixture',
       'LUNA_DEV_BUNDLE_ID': 'com.novnski.luna.qa.fixture', 'LUNA_DEV_IPC_PORT': '59885',
       'LUNA_DEV_DATA_DIR': str(root / 'target' / 'qa-data')}
env.pop('LUNA_DEV_WORKTREES_DIR', None)
env.pop('LUNA_WORKTREES_DIR', None)
checks = []
def expect(name, args, code, cwd=root, overrides=None):
    r = call(args, cwd=cwd, env={**env, **(overrides or {})})
    (report / (name + '.log')).write_text(r.stdout + r.stderr)
    assert r.returncode == code, (name, r.returncode, code, r.stdout, r.stderr)
    checks.append(name); print('PASS', name, flush=True)
    return r
appcmd = ['bash', 'scripts/run-macos-dev.sh', '--refresh']
outside_worktrees = scratch / 'outside-worktrees'; outside_worktrees.mkdir()
worktrees_link = root / 'target' / 'qa-worktrees-link'
worktrees_link.symlink_to(outside_worktrees, target_is_directory=True)
for name, overrides in {
    'partial-name': {'LUNA_DEV_BUNDLE_ID': ''},
    'unsafe-name': {'LUNA_DEV_APP_NAME': '../LUNA Dev'},
    'main-id': {'LUNA_DEV_BUNDLE_ID': 'com.novnski.luna.dev'},
    'main-port': {'LUNA_DEV_IPC_PORT': '59777'},
    'invalid-port': {'LUNA_DEV_IPC_PORT': '65536'},
    'leading-zero-port': {'LUNA_DEV_IPC_PORT': '059785'},
    'invalid-revision': {'LUNA_DEV_REVISION': '1/2'},
    'outside-data': {'LUNA_DEV_DATA_DIR': str(scratch / 'data')},
    'outside-worktrees': {'LUNA_DEV_WORKTREES_DIR': str(outside_worktrees)},
    'symlink-worktrees': {'LUNA_DEV_WORKTREES_DIR': str(worktrees_link / 'checkouts')},
    'empty-worktrees': {'LUNA_DEV_WORKTREES_DIR': ''},
    'data-root-as-worktrees': {'LUNA_DEV_WORKTREES_DIR': str(root / 'target' / 'qa-data')},
    'worktrees-cannot-replace-data': {'LUNA_DEV_DATA_DIR': '', 'LUNA_DEV_WORKTREES_DIR': str(root / 'target' / 'qa-data' / 'worktrees')},
    'main-rejects-qa-worktrees': {
        'LUNA_DEV_APP_NAME': 'LUNA Dev', 'LUNA_DEV_BUNDLE_ID': 'com.novnski.luna.dev',
        'LUNA_DEV_IPC_PORT': '59777', 'LUNA_DEV_DATA_DIR': str(root / 'target/luna-macos-dev/data'),
        'LUNA_DEV_WORKTREES_DIR': str(root / 'target/qa-data/worktrees'),
    },
}.items(): expect(name, appcmd, 2, overrides=overrides)
assert not (scratch / 'actions').exists(), 'invalid configuration reached build'
# A separately packaged peer must reserve its identity even while stopped.
import plistlib
peer_info = peer / 'target/luna-macos-dev/LUNA QA Peer.app/Contents/Info.plist'
peer_info.parent.mkdir(parents=True)
peer_info.write_bytes(plistlib.dumps({'CFBundleIdentifier': env['LUNA_DEV_BUNDLE_ID']}))
expect('peer-collision', appcmd, 2)
peer_info.unlink()
app = root / 'target/luna-macos-dev/LUNA QA Fixture.app'
(root / 'target/debug').mkdir()
(root / 'target/debug/luna').write_text('fixture binary')
expect('qa-worktrees-default', ['bash', 'scripts/run-macos-dev.sh', '--build-only'], 0)
info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
assert info['LSEnvironment']['LUNA_WORKTREES_DIR'] == str(root / 'target/qa-data/worktrees')
explicit_worktrees = root / 'target/qa-data/alternate-worktrees'
expect('qa-worktrees-explicit', ['bash', 'scripts/run-macos-dev.sh', '--build-only'], 0,
       overrides={'LUNA_DEV_WORKTREES_DIR': str(explicit_worktrees)})
info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
assert info['LSEnvironment']['LUNA_WORKTREES_DIR'] == str(explicit_worktrees)
expect('main-worktrees-default-unchanged', ['bash', 'scripts/run-macos-dev.sh', '--build-only'], 0,
       overrides={'LUNA_DEV_APP_NAME': 'LUNA Dev', 'LUNA_DEV_BUNDLE_ID': 'com.novnski.luna.dev',
                  'LUNA_DEV_IPC_PORT': '59777', 'LUNA_DEV_DATA_DIR': str(root / 'target/luna-macos-dev/data')})
main_info = plistlib.loads((root / 'target/luna-macos-dev/LUNA Dev.app/Contents/Info.plist').read_bytes())
assert 'LUNA_WORKTREES_DIR' not in main_info.get('LSEnvironment', {})
sentinel = app / 'Contents/sentinel'; sentinel.write_text('preserve')
build_count = (scratch / 'actions').read_text().count('build')
expect('other-luna-running', appcmd, 75, overrides={'QA_OTHER_RUNNING': '1'})
assert sentinel.read_text() == 'preserve'
assert (scratch / 'actions').read_text().count('build') == build_count
app_slot = root / '.git/luna-app-operation.lock'
app_slot.mkdir()
expect('app-operation-lock', appcmd, 75)
expect('linked-worktree-app-operation-lock', appcmd, 75, cwd=peer, overrides={
    'LUNA_DEV_APP_NAME': 'LUNA QA Other',
    'LUNA_DEV_BUNDLE_ID': 'com.novnski.luna.qa.other',
    'LUNA_DEV_IPC_PORT': '59886',
    'LUNA_DEV_DATA_DIR': str(peer / 'target/qa-data'),
})
app_slot.rmdir()
assert sentinel.read_text() == 'preserve'
assert (scratch / 'actions').read_text().count('build') == build_count
expect('failed-build', appcmd, 37, overrides={'QA_BUILD_EXIT': '37'})
assert sentinel.read_text() == 'preserve'
expect('failed-signing', appcmd, 38, overrides={'QA_SIGN_EXIT': '38'})
assert sentinel.read_text() == 'preserve'
# An occupied real TCP socket must leave the existing bundle in place.
import socket
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0)); listener.listen()
    expect('occupied-port', appcmd, 1, overrides={'LUNA_DEV_IPC_PORT': str(listener.getsockname()[1])})
assert sentinel.read_text() == 'preserve'
expect('quit-refused', appcmd, 1, overrides={'QA_RUNNING': '1'})
assert sentinel.read_text() == 'preserve'
quit_request = (scratch / 'quit-request').read_text()
assert '98765' in quit_request and 'runningApplicationWithProcessIdentifier' in quit_request
assert 'tell application id' not in quit_request
expect('failed-launch', appcmd, 1)
assert sentinel.read_text() == 'preserve'
open_args = (scratch / 'open-args').read_text().splitlines()
assert ('--env', 'LUNA_WORKTREES_DIR=' + str(root / 'target/qa-data/worktrees')) in zip(open_args, open_args[1:])
guard = ['python3', 'scripts/build-budget.py']
small = {'LUNA_BUILD_BUDGET_GIB': '0.05', 'LUNA_BUILD_RESERVE_GIB': '0.01'}
expect('invalid-ceiling', guard + ['--status'], 2, overrides={'LUNA_BUILD_BUDGET_GIB': '200'})
expect('admission-blocked', guard + ['--', 'true'], 75, overrides={'LUNA_BUILD_BUDGET_GIB': '0.001'})
expect('child-exit', guard + ['--', 'sh', '-c', 'exit 42'], 42, overrides=small)
expect('accounting', guard + ['--status'], 0, overrides=small)
# Both linked worktrees must share one advisory lock for the child's lifetime.
marker = scratch / 'started'
first = subprocess.Popen(guard + ['--', 'sh', '-c', 'touch "$1"; sleep 3', 'sh', str(marker)], cwd=root, env={**env, **small}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
for _ in range(100):
    if marker.exists(): break
    time.sleep(0.1)
assert marker.exists(), first.communicate()
expect('concurrent-blocked', guard + ['--', 'true'], 75, cwd=peer, overrides=small)
assert first.wait() == 0
expect('lock-released', guard + ['--', 'true'], 0, cwd=peer, overrides=small)
marker.unlink()
interrupted = subprocess.Popen(guard + ['--', 'sh', '-c', 'touch "$1"; sleep 30', 'sh', str(marker)], cwd=root, env={**env, **small}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
for _ in range(100):
    if marker.exists(): break
    time.sleep(0.1)
assert marker.exists(), interrupted.communicate()
interrupted.terminate()
assert interrupted.wait(timeout=15) == 143
expect('lock-after-signal', guard + ['--', 'true'], 0, overrides=small)
# A sparse logical file is deliberately counted conservatively.
growth = 'import pathlib,time; f=pathlib.Path("target/growth").open("wb"); f.truncate(20*1024*1024); f.close(); time.sleep(15)'
expect('growth-stopped', guard + ['--', 'python3', '-c', growth], 75, overrides=small)
(root / 'target/growth').unlink()
expect('lock-after-stop', guard + ['--', 'true'], 0, overrides=small)
# Target symlinks must not traverse an unrelated large tree or a cycle.
outside = scratch / 'outside'; outside.mkdir()
with (outside / 'large').open('wb') as f: f.truncate(1024**3)
(root / 'target/external').symlink_to(outside, target_is_directory=True)
(root / 'target/cycle').symlink_to(root / 'target', target_is_directory=True)
account = expect('no-symlink-traversal', guard + ['--status'], 0, overrides=small)
assert json.loads(account.stdout)['total_bytes'] < 10*1024*1024
# Continuously remove entire directories while the real filesystem scan runs.
churn = 'import pathlib,shutil,time; base=pathlib.Path("target/churn"); end=time.monotonic()+4\nwhile time.monotonic()<end:\n base.mkdir(exist_ok=True)\n for i in range(40): (base/str(i)).mkdir(exist_ok=True)\n shutil.rmtree(base)'
expect('directory-disappearance', guard + ['--', 'python3', '-c', churn], 0, overrides=small)
(report / 'summary.json').write_text(json.dumps({'checks': checks, 'passed': len(checks)}, indent=2) + '\n')
PY
cat "$report/summary.json"
