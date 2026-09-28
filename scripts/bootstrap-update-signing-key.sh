#!/usr/bin/env bash
# Create the release signing key once. The private key stays outside the repo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KEY_DIR="${LUNA_UPDATE_KEY_DIR:-$HOME/.config/luna-release-signing}"
PRIVATE_KEY="$KEY_DIR/update-signing.pem"
PUBLIC_KEY="$ROOT/dist/update-signing-public.hex"

if [[ -e "$PRIVATE_KEY" || -e "$PUBLIC_KEY" ]]; then
  echo "Refusing to replace an existing update signing key" >&2
  exit 1
fi

umask 077
mkdir -p "$KEY_DIR"
chmod 700 "$KEY_DIR"
openssl genpkey -algorithm Ed25519 -out "$PRIVATE_KEY"
chmod 600 "$PRIVATE_KEY"
openssl pkey -in "$PRIVATE_KEY" -pubout -outform DER \
  | tail -c 32 | xxd -p -c 256 > "$PUBLIC_KEY"
chmod 644 "$PUBLIC_KEY"

[[ "$(wc -c < "$PUBLIC_KEY" | tr -d ' ')" == 65 ]] || {
  echo "Invalid generated public key" >&2
  exit 1
}
echo "Private key: $PRIVATE_KEY"
echo "Public key: $PUBLIC_KEY"
