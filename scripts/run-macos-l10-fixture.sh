#!/usr/bin/env bash
# Build and launch the isolated, mock-only L10 native QA bundle.
#
# The bundle has its own LaunchServices identity and receives an explicit
# synthetic profile root. drafts-fixture disables native credential fallbacks
# and places every provider home below that root.

set -euo pipefail

BUILD_ONLY=false
if [[ "${1:-}" == "--build-only" ]]; then
  BUILD_ONLY=true
  shift
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/target/luna-l10-fixture"
APP="$OUT/LUNA L10 QA.app"
PROFILE="${LUNA_L10_FIXTURE_ROOT:-$OUT/profile}"
EXECUTABLE="$APP/Contents/MacOS/drafts-fixture"
BUILD_EXECUTABLE="$ROOT/target/debug/examples/drafts-fixture"
SOURCE_ICONSET="$ROOT/dist/macos/luna.iconset"
ICONUTIL_BIN="${LUNA_L10_ICONUTIL_BIN:-/usr/bin/iconutil}"
STAGE_ROOT=""
BACKUP_ROOT=""
REPLACEMENT_PENDING=false
HAD_PREVIOUS_APP=false

remove_generated_tree() {
  local path="$1"

  if [[ -n "$path" && "$path" == "$OUT/"* && -d "$path" ]]; then
    /bin/rm -rf -- "$path"
  fi
}

cleanup() {
  local status=$?

  if [[ -n "$BACKUP_ROOT" && -e "$BACKUP_ROOT/LUNA L10 QA.app" ]]; then
    if [[ -e "$APP" ]]; then
      remove_generated_tree "$APP"
    fi
    /bin/mv -- "$BACKUP_ROOT/LUNA L10 QA.app" "$APP"
  elif $REPLACEMENT_PENDING && ! $HAD_PREVIOUS_APP && [[ -e "$APP" ]]; then
    remove_generated_tree "$APP"
  fi

  remove_generated_tree "$STAGE_ROOT"
  remove_generated_tree "$BACKUP_ROOT"
  trap - EXIT
  exit "$status"
}
trap cleanup EXIT

fail() {
  echo "LUNA L10 QA bundle validation failed: $*" >&2
  return 1
}

plist_value() {
  local plist="$1"
  local key="$2"
  /usr/libexec/PlistBuddy -c "Print :$key" "$plist"
}

validate_bundle() {
  local bundle="$1"
  local roundtrip_iconset="$2"
  local contents="$bundle/Contents"
  local executable="$contents/MacOS/drafts-fixture"
  local plist="$contents/Info.plist"
  local icon="$contents/Resources/luna.icns"
  local signature_details
  local icon_count

  [[ -x "$executable" ]] || { fail "missing executable at $executable"; return 1; }
  /usr/bin/file "$executable" | /usr/bin/grep -Fq "Mach-O" || {
    fail "fixture executable is not Mach-O"
    return 1
  }
  /usr/bin/plutil -lint "$plist" >/dev/null || { fail "invalid Info.plist"; return 1; }

  [[ "$(plist_value "$plist" CFBundleDisplayName)" == "LUNA L10 QA" ]] || {
    fail "unexpected CFBundleDisplayName"
    return 1
  }
  [[ "$(plist_value "$plist" CFBundleExecutable)" == "drafts-fixture" ]] || {
    fail "unexpected CFBundleExecutable"
    return 1
  }
  [[ "$(plist_value "$plist" CFBundleIconFile)" == "luna.icns" ]] || {
    fail "unexpected CFBundleIconFile"
    return 1
  }
  [[ "$(plist_value "$plist" CFBundleIdentifier)" == "com.novnski.luna.l10-qa" ]] || {
    fail "unexpected CFBundleIdentifier"
    return 1
  }
  [[ "$(plist_value "$plist" CFBundleName)" == "LUNA L10 QA" ]] || {
    fail "unexpected CFBundleName"
    return 1
  }
  [[ "$(plist_value "$plist" CFBundleShortVersionString)" == "0.2.73-l10-qa" ]] || {
    fail "unexpected CFBundleShortVersionString"
    return 1
  }
  [[ "$(plist_value "$plist" CFBundleVersion)" == "1" ]] || {
    fail "unexpected CFBundleVersion"
    return 1
  }
  if /usr/libexec/PlistBuddy -c "Print :CFBundleURLTypes" "$plist" >/dev/null 2>&1; then
    fail "CFBundleURLTypes must not be present"
    return 1
  fi
  if /usr/libexec/PlistBuddy -c "Print :LSEnvironment" "$plist" >/dev/null 2>&1; then
    fail "LSEnvironment must not be present"
    return 1
  fi

  [[ -s "$icon" ]] || { fail "missing or empty icon at $icon"; return 1; }
  /usr/bin/file "$icon" | /usr/bin/grep -Fq "Mac OS X icon" || {
    fail "luna.icns is not a valid macOS icon container"
    return 1
  }
  "$ICONUTIL_BIN" -c iconset "$icon" -o "$roundtrip_iconset" || {
    fail "luna.icns could not be decoded"
    return 1
  }
  icon_count="$(/usr/bin/find "$roundtrip_iconset" -maxdepth 1 -type f -name '*.png' | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
  [[ "$icon_count" == "10" ]] || {
    fail "decoded iconset has $icon_count PNGs instead of 10"
    return 1
  }

  /usr/bin/codesign --verify --deep --strict --verbose=2 "$bundle" || {
    fail "ad-hoc signature verification failed"
    return 1
  }
  signature_details="$(/usr/bin/codesign -d --verbose=4 "$bundle" 2>&1)" || {
    fail "could not inspect bundle signature"
    return 1
  }
  /usr/bin/grep -Fq "Identifier=com.novnski.luna.l10-qa" <<<"$signature_details" || {
    fail "signature has the wrong identifier"
    return 1
  }
  /usr/bin/grep -Fq "Signature=adhoc" <<<"$signature_details" || {
    fail "bundle is not ad-hoc signed"
    return 1
  }
}

# Match the final fixture executable exactly, while allowing its synthetic
# profile and scenario arguments after the executable path.
while IFS= read -r candidate_pid; do
  [[ -n "$candidate_pid" ]] || continue
  if ! candidate_command="$(/bin/ps -p "$candidate_pid" -o command= 2>/dev/null)"; then
    continue
  fi
  case "$candidate_command" in
    "$EXECUTABLE"|"$EXECUTABLE"\ *)
      echo "LUNA L10 QA is already running. Quit it before rebuilding the bundle." >&2
      exit 1
      ;;
  esac
done < <(/usr/bin/pgrep -f "drafts-fixture" 2>/dev/null || true)

cd "$ROOT"
"$ROOT/scripts/cargo.sh" build --locked -p luna-ui --example drafts-fixture --features appshots-fixture

/bin/mkdir -p "$OUT" "$PROFILE"
STAGE_ROOT="$(/usr/bin/mktemp -d "$OUT/.luna-l10-stage.XXXXXX")"
STAGED_APP="$STAGE_ROOT/LUNA L10 QA.app"
STAGED_CONTENTS="$STAGED_APP/Contents"
/bin/mkdir -p "$STAGED_CONTENTS/MacOS" "$STAGED_CONTENTS/Resources"
/usr/bin/install -m 755 "$BUILD_EXECUTABLE" "$STAGED_CONTENTS/MacOS/drafts-fixture"
/usr/bin/install -m 644 "$ROOT/dist/macos/Info-dev.plist" "$STAGED_CONTENTS/Info.plist"
/usr/bin/plutil -replace CFBundleDisplayName -string "LUNA L10 QA" "$STAGED_CONTENTS/Info.plist"
/usr/bin/plutil -replace CFBundleExecutable -string "drafts-fixture" "$STAGED_CONTENTS/Info.plist"
/usr/bin/plutil -replace CFBundleIdentifier -string "com.novnski.luna.l10-qa" "$STAGED_CONTENTS/Info.plist"
/usr/bin/plutil -replace CFBundleName -string "LUNA L10 QA" "$STAGED_CONTENTS/Info.plist"
/usr/bin/plutil -replace CFBundleShortVersionString -string "0.2.73-l10-qa" "$STAGED_CONTENTS/Info.plist"
/usr/bin/plutil -replace CFBundleVersion -string "1" "$STAGED_CONTENTS/Info.plist"
/usr/bin/plutil -remove CFBundleURLTypes "$STAGED_CONTENTS/Info.plist" 2>/dev/null || true
/usr/bin/plutil -remove LSEnvironment "$STAGED_CONTENTS/Info.plist" 2>/dev/null || true

STAGED_ICON="$STAGED_CONTENTS/Resources/luna.icns"
if ! "$ICONUTIL_BIN" -c icns "$SOURCE_ICONSET" -o "$STAGED_ICON"; then
  echo "Direct icon compilation failed; retrying from temporary normalized copies." >&2
  NORMALIZED_ICONSET="$STAGE_ROOT/luna.iconset"
  FALLBACK_ICON="$STAGE_ROOT/luna-fallback.icns"
  /bin/mkdir -p "$NORMALIZED_ICONSET"
  for source_png in "$SOURCE_ICONSET"/*.png; do
    /usr/bin/sips -s format png "$source_png" \
      --out "$NORMALIZED_ICONSET/$(/usr/bin/basename "$source_png")" >/dev/null
  done
  "$ICONUTIL_BIN" -c icns "$NORMALIZED_ICONSET" -o "$FALLBACK_ICON"
  /bin/mv -f -- "$FALLBACK_ICON" "$STAGED_ICON"
fi

/usr/bin/cmp -s "$BUILD_EXECUTABLE" "$STAGED_CONTENTS/MacOS/drafts-fixture" || {
  fail "staged executable does not match the completed Cargo build"
  exit 1
}
/usr/bin/codesign --force --sign - --identifier com.novnski.luna.l10-qa "$STAGED_APP"
validate_bundle "$STAGED_APP" "$STAGE_ROOT/staged-icon-roundtrip.iconset"

BACKUP_ROOT="$(/usr/bin/mktemp -d "$OUT/.luna-l10-backup.XXXXXX")"
if [[ -e "$APP" ]]; then
  if ! /bin/mv -- "$APP" "$BACKUP_ROOT/LUNA L10 QA.app"; then
    fail "could not preserve the previous fixture bundle"
    exit 1
  fi
  HAD_PREVIOUS_APP=true
fi
REPLACEMENT_PENDING=true
/bin/mv -- "$STAGED_APP" "$APP"
validate_bundle "$APP" "$STAGE_ROOT/final-icon-roundtrip.iconset"
REPLACEMENT_PENDING=false
remove_generated_tree "$BACKUP_ROOT"
BACKUP_ROOT=""

if $BUILD_ONLY; then
  echo "Built and validated mock-only LUNA L10 QA at $APP" >&2
  echo "Profile: $PROFILE" >&2
  exit 0
fi

echo "Launching mock-only LUNA L10 QA" >&2
echo "Profile: $PROFILE" >&2
echo "Control: scripts/l10-fixture-control.sh '$PROFILE' subscription <reset-at-ms>" >&2
exec open -W "$APP" --args "$PROFILE" turn-recovery
