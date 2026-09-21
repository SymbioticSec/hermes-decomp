#!/usr/bin/env bash
# Best-effort: build `hermesc` from a facebook/hermes source tag, for HBC versions
# that have no prebuilt CLI (roughly 40-58, predating v0.1.0). This is fragile
# (old CMake/LLVM expectations); failures are recorded, not fatal.
#
# Usage: build_hermesc_from_source.sh <git-tag>     e.g. v0.0.1
# On success the built compiler is probed and appended to .toolchains/manifest.tsv.
set -uo pipefail

TAG="${1:-}"
[ -z "$TAG" ] && { echo "usage: $0 <hermes-git-tag>" >&2; exit 2; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLDIR="$ROOT/examples/react-native/.toolchains"
SRC="$TOOLDIR/src-$TAG"
BUILD="$TOOLDIR/build-$TAG"
MANIFEST="$TOOLDIR/manifest.tsv"
UNAVAIL="$TOOLDIR/UNAVAILABLE.md"
mkdir -p "$TOOLDIR"

fail() { echo "- $TAG: $1" >> "$UNAVAIL"; echo "FAILED ($TAG): $1" >&2; exit 1; }

for tool in git cmake ninja; do command -v "$tool" >/dev/null || fail "missing build tool: $tool"; done

[ -d "$SRC" ] || git clone --depth 1 --branch "$TAG" https://github.com/facebook/hermes "$SRC" 2>/dev/null \
  || fail "git clone of tag failed"

# Hermes historically builds with its bundled build script; fall back to a plain
# cmake configure of the hermesc target.
cmake -S "$SRC" -B "$BUILD" -G Ninja -DCMAKE_BUILD_TYPE=Release >/dev/null 2>&1 \
  || fail "cmake configure failed (old toolchain expectations)"
cmake --build "$BUILD" --target hermesc >/dev/null 2>&1 \
  || fail "hermesc build failed"

COMPILER="$(find "$BUILD" -type f -name hermesc | head -1)"
[ -x "$COMPILER" ] || fail "hermesc binary not produced"

PROBE="$(mktemp).js"; printf 'var x=1+2; print(x);\n' > "$PROBE"
OUT="$(mktemp).hbc"
"$COMPILER" -emit-binary -out "$OUT" "$PROBE" >/dev/null 2>&1 || fail "probe compile failed"
VER="$(python3 -c "import sys,struct;d=open('$OUT','rb').read();print(struct.unpack('<I',d[8:12])[0])")"
printf '%s\t%s\t%s\n' "$VER" "$TAG" "$COMPILER" >> "$MANIFEST"
echo "OK: $TAG -> HBC $VER ($COMPILER)"
