#!/usr/bin/env bash
# Project-scoped Cargo entrypoint; no global Cargo shim or Rust toolchain changes.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cache_mode="${LUNA_BUILD_CACHE:-auto}"
case "$cache_mode" in
  auto|0|1) ;;
  *) echo "LUNA_BUILD_CACHE must be auto, 0, or 1" >&2; exit 2 ;;
esac

command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"

# mbx owns a different `clean` command. Keep Cargo's cleanup, metadata, fmt,
# installed subcommands, and other non-compilation commands unchanged.
cargo_command="${1:-}"
if [[ "$cargo_command" == +* ]]; then
  cargo_command="${2:-}"
fi
# Mutation testing launches Cargo internally. Keep the whole campaign under
# one build lock, with copied sources and output inside the accounted target/.
if [[ "$cargo_command" == mutants ]]; then
  if [[ "${1:-}" == +* ]]; then
    export RUSTUP_TOOLCHAIN="${1#+}"
    shift
  fi
  shift
  exec python3 "$ROOT/scripts/mutants.py" "$@"
fi
case "$cargo_command" in
  build|b|check|c|test|t|run|r|bench|clippy|doc|rustdoc|rustc) ;;
  *) exec cargo "$@" ;;
esac

# Keep targets discoverable by the repository-wide guard. Cargo's explicit
# target-dir also overrides machine-local Cargo configuration.
target_dir="${CARGO_TARGET_DIR:-$ROOT/target}"
previous=""
for argument in "$@"; do
  [[ "$argument" != -- ]] || break
  if [[ "$previous" == --target-dir ]]; then target_dir="$argument"; fi
  case "$argument" in
    --target-dir=*) target_dir="${argument#--target-dir=}" ;;
    --config|--config=*|--build-dir|--build-dir=*)
      echo "Build budget cannot account for Cargo path overrides; use the checkout target" >&2; exit 2 ;;
  esac
  previous="$argument"
done
target_dir="$(python3 - "$ROOT" "$target_dir" <<'PY'
from pathlib import Path
import sys
root, target = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
base = root / 'target'
if base.is_symlink() or (target != base and base not in target.parents):
    sys.exit('Build target must be inside this checkout target/ (no target symlink)')
print(target)
PY
)" || exit 2
if [[ -n "${CARGO_BUILD_BUILD_DIR:-}" || -n "${CARGO_BUILD_TARGET_DIR:-}" ]]; then
  echo "Unset CARGO_BUILD_BUILD_DIR/CARGO_BUILD_TARGET_DIR; use the checkout target" >&2
  exit 2
fi
export CARGO_TARGET_DIR="$target_dir"

guarded_exec() {
  exec python3 "$ROOT/scripts/build-budget.py" -- "$@"
}

if [[ "$cache_mode" == 0 ]]; then
  guarded_exec cargo "$@"
fi

mbx_bin="${LUNA_MBX_BIN:-}"
if [[ -z "$mbx_bin" ]]; then
  mbx_bin="$(command -v mbx || true)"
  if [[ -z "$mbx_bin" && -x "$HOME/.local/bin/mbx" ]]; then
    mbx_bin="$HOME/.local/bin/mbx"
  fi
fi
if [[ -z "$mbx_bin" || ! -x "$mbx_bin" ]]; then
  if [[ "$cache_mode" == 1 ]]; then
    echo "Mr Boxington is required but unavailable; see docs/reference/build-cache.md" >&2
    exit 127
  fi
  echo "Mr Boxington unavailable; using Cargo (see docs/reference/build-cache.md)" >&2
  guarded_exec cargo "$@"
fi

# target/ also holds the dev bundle and persistent app data, so mbx must never
# adopt it. Keep incremental state in Cargo and allow plain Cargo to overwrite
# restored artifacts even on filesystems without copy-on-write cloning.
export MBX_TARGET_VIEWS=0
export MBX_LEARNED_INCREMENTAL=0
export MBX_RESTORE_HARDLINK=0
if [[ "${MBX_GC_MAX_SIZE:-20GiB}" != 20GiB ]]; then
  echo "Keep MBX_GC_MAX_SIZE=20GiB during the shared audit; coordinate policy changes" >&2
  exit 2
fi
export MBX_GC_MAX_SIZE=20GiB
export MBX_SAVINGS="${MBX_SAVINGS:-plain}"

# exec preserves Cargo's status/signals. A cache failure is never retried as a
# second build; use LUNA_BUILD_CACHE=0 explicitly when diagnosing one.
guarded_exec "$mbx_bin" "$@"
