#!/usr/bin/env bash
# Every gate this project has, in one command.
#
# The bytecode round trip proves the decoder reads a file and runs what it
# produced. It says nothing about whether the emitted JavaScript even parses,
# which is how 2987 modules of one build were unparsable while every gate was
# green. So the parse check runs here too, against a recorded ceiling per
# bundle rather than against zero, and fails on the way up.
#
# Usage: bash scripts/build/gates.sh [--quick]
#   --quick  skip the parse check, which has to decompile whole bundles
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

QUICK=0
[ "${1:-}" = "--quick" ] && QUICK=1

DECOMP="${DECOMP:-$ROOT/target/release/hermes-decomp}"
BASELINE="$ROOT/scripts/build/syntax_baseline.tsv"
failed=0

step() {
  printf '\n=== %s ===\n' "$1"
}

report() { # <name> <exit code>
  if [ "$2" -eq 0 ]; then
    printf 'OK   %s\n' "$1"
  else
    printf 'FAIL %s (exit %s)\n' "$1" "$2"
    failed=1
  fi
}

step "fmt"
cargo fmt --all -- --check
report "fmt" $?

step "clippy"
cargo clippy --workspace --all-targets -- -D warnings
report "clippy" $?

step "tests"
cargo test --workspace
report "tests" $?

# The round trip and the parse check run the release binary; build it from the
# code under test first, otherwise they judge whatever was built last.
step "release build"
cargo build --release -p hbc-decomp-cli
report "release build" $?

step "round trip corpus"
bash "$ROOT/scripts/build/roundtrip.sh"
report "round trip" $?

if [ "$QUICK" -eq 1 ]; then
  printf '\nskipped: parse check (--quick)\n'
else
  step "parse check"
  if [ ! -x "$DECOMP" ]; then
    printf 'skip: decompiler not found at %s\n' "$DECOMP"
    printf '  build it with: cargo build --release --workspace\n'
    failed=1
  else
    while IFS=$'\t' read -r bundle flags ceiling; do
      case "$bundle" in ''|'#'*) continue ;; esac
      if [ ! -f "$ROOT/$bundle" ]; then
        printf 'skip: %s is not on this machine\n' "$bundle"
        continue
      fi
      out="$(mktemp -t gates_syntax)"
      rm -f "$ROOT/$bundle.hdcache"
      # shellcheck disable=SC2086
      "$DECOMP" decompile "$ROOT/$bundle" $flags -o "$out" >/dev/null 2>&1
      node --experimental-vm-modules "$ROOT/scripts/build/syntax_check.mjs" \
        "$out" --max "$ceiling"
      report "parse $bundle (ceiling $ceiling)" $?
      rm -f "$out"
    done < "$BASELINE"
  fi
fi

printf '\n'
if [ "$failed" -eq 0 ]; then
  printf 'all gates passed\n'
else
  printf 'at least one gate failed\n'
fi
exit "$failed"
