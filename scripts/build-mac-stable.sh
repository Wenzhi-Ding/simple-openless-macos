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
# Release codegen stays opt-level=3 + thin LTO as set in src-tauri/Cargo.toml.
# Only codegen parallelism and the strip level change: codegen-units=16 lets
# LLVM compile the large crates in parallel, and strip=debuginfo avoids the
# mis-aligned LINKEDIT string pool that Homebrew's strip tool can produce for
# proc-macro dylibs at strip=symbols.
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_STRIP=debuginfo
# Drop only previous delivery bundles; Cargo's compiled artifacts under
# src-tauri/target/release stay cached and are reused.
rm -rf "$ROOT/src-tauri/target/release/bundle"
APPLE_SIGNING_IDENTITY="$IDENTITY" npx tauri build --bundles app \
  --config src-tauri/tauri.macos-mlx.conf.json \
  --config '{"build":{"beforeBuildCommand":"npm run build"}}'
APP="$ROOT/src-tauri/target/release/bundle/macos/OpenLess.app"
APP_BIN="$APP/Contents/MacOS/openless"
# Existence is enough to prove the bundle belongs to this build: set -euo
# pipefail aborts on any failing step, so packaging always succeeds before we
# get here, and the delivery directory was emptied just above. A freshness
# check against a build start timestamp is deliberately absent: Cargo reuses
# cached binaries when sources did not change, and the packaging copy keeps the
# old mtime, so a timestamp comparison would reject a perfectly valid build.
# Upstream dropped the same mtime check in b268dcf9 for that reason ("a hot
# build reuses a binary with an old timestamp; mtime cannot prove freshness").
if [[ ! -d "$APP" || ! -f "$APP_BIN" ]]; then
  printf 'ERROR: %s was not produced by this build (packaging did not finish).\n' "$APP" >&2
  exit 1
fi
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -d -r- "$APP"
# DR continuity must be compared with the previous certificate-signed release.
# Migrating an older build may require one-time user permission review;
# no certificate or signature can promise that every TCC grant survives updates.
