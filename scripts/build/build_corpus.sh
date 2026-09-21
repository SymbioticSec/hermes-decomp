#!/usr/bin/env bash
# Build the per-version corpus: compile the canonical app + each expression
# snippet with every acquired hermesc, then decompile/disassemble each with our
# tool. Lays out examples/react-native/v<HBC>/.
#
# Requires: scripts/build/fetch_hermesc.sh has populated .toolchains/manifest.tsv.
# Everything is LOCAL (examples/ is gitignored).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RN="$ROOT/examples/react-native"
TOOLDIR="$RN/.toolchains"
MANIFEST="$TOOLDIR/manifest.tsv"
DECOMP="$ROOT/target/release/hermes-decomp"
SAMPLE="$RN/sample.js"
EXPRDIR="$RN/expressions"

[ -f "$MANIFEST" ] || { echo "No manifest; run fetch_hermesc.sh first." >&2; exit 1; }
[ -x "$DECOMP" ] || { echo "Build the decompiler: cargo build --release -p hbc-decomp-cli" >&2; exit 1; }

hbc_version() { python3 -c "import sys,struct;d=open(sys.argv[1],'rb').read();print(struct.unpack('<I',d[8:12])[0] if len(d)>=12 else 0)" "$1"; }

# Pick the newest tag per HBC version (manifest is sorted asc by version then tag).
declare -A COMPILER_FOR
declare -A TAG_FOR
while IFS=$'\t' read -r ver tag comp; do
  [ -z "$ver" ] && continue
  COMPILER_FOR[$ver]="$comp"   # later line (newer tag) overwrites -> newest wins
  TAG_FOR[$ver]="$tag"
done < "$MANIFEST"

versions=$(printf '%s\n' "${!COMPILER_FOR[@]}" | sort -n)
echo "Building corpus for HBC versions: $(echo $versions | tr '\n' ' ')"

for ver in $versions; do
  comp="${COMPILER_FOR[$ver]}"
  tag="${TAG_FOR[$ver]}"
  vdir="$RN/v$ver"
  echo "=== HBC $ver (hermes $tag) ==="
  rm -rf "$vdir"; mkdir -p "$vdir/expressions"

  # Metro module fixture (require -> import / exports -> export coverage).
  if [ -f "$RN/metro_sample.js" ]; then
    cp "$RN/metro_sample.js" "$vdir/metro_source.js"
    if "$comp" -emit-binary -O -out "$vdir/metro.hbc" "$RN/metro_sample.js" >/dev/null 2>&1; then
      "$DECOMP" decompile "$vdir/metro.hbc" --output "$vdir/metro_decompiled.js" >/dev/null 2>&1 || true
    fi
  fi

  # Canonical app.
  cp "$SAMPLE" "$vdir/source.js"
  if "$comp" -emit-binary -O -out "$vdir/bytecode.hbc" "$SAMPLE" >/dev/null 2>&1; then
    "$DECOMP" decompile "$vdir/bytecode.hbc" --output "$vdir/decompiled.js" >/dev/null 2>&1 \
      || echo "  WARN: decompile failed for app" >&2
    "$DECOMP" disasm "$vdir/bytecode.hbc" --output "$vdir/disasm.txt" >/dev/null 2>&1 \
      || echo "  WARN: disasm failed for app" >&2
  else
    echo "  WARN: hermesc failed on canonical app" >&2
  fi

  # Expression suite. A snippet that doesn't compile on this version (feature too
  # new) is recorded as a compile gap, not a corpus failure.
  ok=0; gap=0
  for f in "$EXPRDIR"/*.js; do
    name="$(basename "$f" .js)"
    edir="$vdir/expressions/$name"; mkdir -p "$edir"
    # Wrap each snippet in a function (like real RN module code) rather than a
    # bare top-level script. Top-level `var`s compile to global-object property
    # accesses (a niche, messier decompile path); wrapping exercises the normal
    # function-body path that real bundles use.
    printf 'function main() {\n%s\n}\nmain();\n' "$(cat "$f")" > "$edir/source.js"
    if "$comp" -emit-binary -O -out "$edir/bytecode.hbc" "$edir/source.js" >/dev/null 2>&1; then
      "$DECOMP" decompile "$edir/bytecode.hbc" --output "$edir/decompiled.js" >/dev/null 2>&1 || true
      ok=$((ok+1))
    else
      echo "feature-unsupported-by-hermes-$tag" > "$edir/COMPILE_GAP.txt"
      rm -f "$edir/bytecode.hbc"
      gap=$((gap+1))
    fi
  done
  echo "  expressions: $ok compiled, $gap gaps"
done

echo
echo "Corpus laid out under $RN/v*/"
ls -d "$RN"/v*/ 2>/dev/null | sed "s#$RN/##"
