#!/usr/bin/env bash
# Project-scoped Cargo entrypoint; no global Cargo shim or Rust toolchain changes.
set -euo pipefail

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
case "$cargo_command" in
  build|b|check|c|test|t|run|r|bench|clippy|doc|rustdoc|rustc) ;;
  *) exec cargo "$@" ;;
esac

if [[ "$cache_mode" == 0 ]]; then
  exec cargo "$@"
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
  exec cargo "$@"
fi

# target/ also holds the dev bundle and persistent app data, so mbx must never
# adopt it. Keep incremental state in Cargo and allow plain Cargo to overwrite
# restored artifacts even on filesystems without copy-on-write cloning.
export MBX_TARGET_VIEWS=0
export MBX_LEARNED_INCREMENTAL=0
export MBX_RESTORE_HARDLINK=0
export MBX_GC_MAX_SIZE="${MBX_GC_MAX_SIZE:-20GiB}"
export MBX_SAVINGS="${MBX_SAVINGS:-plain}"

# exec preserves Cargo's status/signals. A cache failure is never retried as a
# second build; use LUNA_BUILD_CACHE=0 explicitly when diagnosing one.
exec "$mbx_bin" "$@"
