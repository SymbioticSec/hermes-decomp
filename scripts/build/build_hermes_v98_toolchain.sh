#!/usr/bin/env bash
# Fetch a modern Hermes toolchain (HBC 98) from Maven Central and build a small
# standalone VM runner. Produces examples/react-native/.toolchains/hermes-v98/
# with hermesc (the compiler), framework (the hermesvm dylib), and hermes-run
# (the VM runner).
#
# This is a verification helper for development. The Rust crate stays fully Rust.
# The runner is an external binary, like the prebuilt hermesc, hermes and node
# tools.
set -euo pipefail
VER="${HERMES_IOS_VERSION:-250829098.0.16}"   # emits HBC 98
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$ROOT/examples/react-native/.toolchains/hermes-v98"

# Platform note: the `hermes-ios` Maven artifact ships Apple .framework bundles,
# so the linkable VM (and this runner) is macOS-only. hermesc itself is a macOS
# host binary too. On Linux/Windows there is no prebuilt modern Hermes host VM
# from this source; build Hermes from source (scripts/build/build_hermesc_from_source.sh
# extended with the `hermes` target) or run the .hbc on an Android device whose
# app embeds a matching libhermes.so.
OS="$(uname -s)"
if [ "$OS" != "Darwin" ]; then
  echo "This modern-Hermes verifier is macOS-only (the hermes-ios artifact is an" >&2
  echo "Apple framework). Detected: $OS. See the platform note in this script." >&2
  exit 1
fi
if ! command -v clang++ >/dev/null 2>&1; then
  echo "clang++ not found (install Xcode command line tools: xcode-select --install)" >&2
  exit 1
fi
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
URL="https://repo1.maven.org/maven2/com/facebook/hermes/hermes-ios/$VER/hermes-ios-$VER-hermes-ios-debug.tar.gz"
echo "Downloading $URL"
curl -fsSL -o "$TMP/h.tar.gz" "$URL"
tar xzf "$TMP/h.tar.gz" -C "$TMP" ./destroot/bin ./destroot/Library/Frameworks/macosx ./destroot/include
mkdir -p "$OUT/framework"
cp "$TMP/destroot/bin/hermesc" "$OUT/hermesc"
cp -R "$TMP/destroot/Library/Frameworks/macosx/hermesvm.framework" "$OUT/framework/"
chmod +x "$OUT/hermesc"
echo "Compiling hermes-run"
clang++ -std=c++17 "$(dirname "$0")/hermes_runner.cpp" \
  -I "$TMP/destroot/include" \
  -F "$OUT/framework" -framework hermesvm \
  -Wl,-rpath,"@executable_path/framework" \
  -o "$OUT/hermes-run"
chmod +x "$OUT/hermes-run"
echo "Toolchain ready: $OUT"
printf 'print("hermes-v98 ok");\n' > "$TMP/t.js"
"$OUT/hermesc" -emit-binary -O -out "$TMP/t.hbc" "$TMP/t.js"
echo -n "smoke: "; "$OUT/hermes-run" "$TMP/t.hbc"
