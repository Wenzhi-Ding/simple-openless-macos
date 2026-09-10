#!/bin/bash
# Sole supported certificate-signed macOS build entrypoint.
# Usage: OPENLESS_SIGNING_IDENTITY=<40-character identity SHA-1> bash scripts/build-mac-stable.sh
# Obtain/import an Apple-issued signing identity only after explicit user approval.
# Never generates certificates, changes trust, resets TCC, or weakens requirements.
set -euo pipefail
IDENTITY="${OPENLESS_SIGNING_IDENTITY:-}"
if [[ ! "$IDENTITY" =~ ^[[:xdigit:]]{40}$ ]]; then
  printf '%s\n' 'ERROR: Set OPENLESS_SIGNING_IDENTITY to a valid codesigning identity SHA-1. No ad-hoc fallback. Inspect: security find-identity -v -p codesigning' >&2
  exit 2
fi
IDENTITIES="$(security find-identity -v -p codesigning)"
if ! command grep -Fqi "$IDENTITY" <<< "$IDENTITIES"; then
  printf '%s\n' 'ERROR: Requested signing identity is not currently valid/available. Obtain/import it with user authorization; no unsigned fallback.' >&2
  exit 2
fi
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
APPLE_SIGNING_IDENTITY="$IDENTITY" npx tauri build --bundles app \
  --config src-tauri/tauri.macos-mlx.conf.json \
  --config '{"build":{"beforeBuildCommand":"npm run build"}}'
APP="$ROOT/src-tauri/target/release/bundle/macos/OpenLess.app"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -d -r- "$APP"
# DR continuity must be compared with the previous certificate-signed release.
# Migrating an older build may require one-time user permission review;
# no certificate or signature can promise that every TCC grant survives updates.
