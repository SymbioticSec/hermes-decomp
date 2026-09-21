#!/usr/bin/env bash
# A->Z functional round-trip: for every expression snippet in every corpus
# version, run the ORIGINAL source and our DECOMPILED output under node and
# compare stdout. Writes v<N>/roundtrip.tsv (name<TAB>PASS|FAIL|NO-DECOMP).
#
# By default each snippet is RE-DECOMPILED with the current release binary before
# comparing, so the round-trip always reflects the code as it is now — never a
# stale pre-generated decompiled.js (which silently makes the run test old
# output). Set ROUNDTRIP_NO_REDECOMP=1 to reuse the existing decompiled.js files.
#
# The decompiled top-level lives in `function global(arg0) { ... }`, so we define
# it and then call it with globalThis (mirrors how Hermes runs the module top).
# `print` is shimmed to console.log so both sides use the same output channel.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RN="$ROOT/examples/react-native"
TMP="$(mktemp -d)"

DECOMP="${DECOMP:-$ROOT/target/release/hermes-decomp}"
if [ -z "${ROUNDTRIP_NO_REDECOMP:-}" ] && [ ! -x "$DECOMP" ]; then
  echo "roundtrip: decompiler not found at $DECOMP" >&2
  echo "  build it first:  cargo build --release -p hbc-decomp-cli" >&2
  echo "  or set ROUNDTRIP_NO_REDECOMP=1 to test the existing decompiled.js files" >&2
  exit 1
fi

SHIM='globalThis.print=(...a)=>console.log(a.map(x=>(typeof x==="object"&&x!==null)?JSON.stringify(x):String(x)).join(" "));'

# Portable per-run timeout (macOS has no `timeout`): perl alarm. Broken decompiled
# output can infinite-loop, so every node run is hard-capped.
run_node() { # <file>
  perl -e 'alarm 5; exec @ARGV' node "$1" 2>&1
}

run_original() { # <src.js>
  printf '%s\n' "$SHIM" > "$TMP/o.js"
  cat "$1" >> "$TMP/o.js"
  run_node "$TMP/o.js"
}

run_decompiled() { # <decompiled.js>
  printf '%s\n' "$SHIM" > "$TMP/d.js"
  cat "$1" >> "$TMP/d.js"
  # Invoke the recovered top-level function if present.
  printf '\ntry{ if(typeof global==="function") global(globalThis); }catch(e){ console.log("__THROW__:"+e); }\n' >> "$TMP/d.js"
  run_node "$TMP/d.js"
}

total_pass=0; total_fail=0
for vdir in "$RN"/v*/; do
  [ -d "$vdir" ] || continue
  ver="$(basename "$vdir")"
  tsv="$vdir/roundtrip.tsv"; : > "$tsv"
  pass=0; fail=0; nodec=0
  for edir in "$vdir"expressions/*/; do
    name="$(basename "$edir")"
    src="$edir/source.js"; dec="$edir/decompiled.js"; hbc="$edir/bytecode.hbc"
    [ -f "$src" ] || continue
    # Regenerate decompiled.js from the current binary (unless opted out), dropping
    # the analysis cache first so a changed decompiler is actually exercised.
    if [ -z "${ROUNDTRIP_NO_REDECOMP:-}" ] && [ -f "$hbc" ]; then
      rm -f "$hbc.hdcache"
      "$DECOMP" decompile "$hbc" --output "$dec" 2>/dev/null || true
    fi
    if [ ! -f "$dec" ]; then
      printf '%s\tNO-DECOMP\n' "$name" >> "$tsv"; nodec=$((nodec+1)); continue
    fi
    exp="$(run_original "$src")"
    got="$(run_decompiled "$dec")"
    if [ "$exp" = "$got" ]; then
      printf '%s\tPASS\n' "$name" >> "$tsv"; pass=$((pass+1))
    else
      printf '%s\tFAIL\n' "$name" >> "$tsv"; fail=$((fail+1))
    fi
  done
  printf '%-6s pass=%-3s fail=%-3s no-decomp=%-3s\n' "$ver" "$pass" "$fail" "$nodec"
  total_pass=$((total_pass+pass)); total_fail=$((total_fail+fail))
done

echo "TOTAL: pass=$total_pass fail=$total_fail"
echo "Per-version detail in v*/roundtrip.tsv; re-run corpus_report to fold into CORPUS_REPORT.md."

# Compare what failed against the recorded list. The check runs both ways: a
# failure nobody wrote down is a regression, and a recorded failure that now
# passes means the list is stale and has to shrink.
KNOWN="$ROOT/scripts/build/roundtrip_known_failures.tsv"
[ -f "$KNOWN" ] || exit 0

actual="$(for tsv in "$RN"/v*/roundtrip.tsv; do
  v="$(basename "$(dirname "$tsv")")"
  awk -F'\t' -v v="$v" '$2=="FAIL" {print v"\t"$1}' "$tsv"
done | sort)"
expected="$(grep -v '^#' "$KNOWN" | awk -F'\t' 'NF>=2 {print $1"\t"$2}' | sort)"

unexpected="$(comm -23 <(printf '%s\n' "$actual") <(printf '%s\n' "$expected") | grep -v '^$')"
fixed="$(comm -13 <(printf '%s\n' "$actual") <(printf '%s\n' "$expected") | grep -v '^$')"

status=0
if [ -n "$unexpected" ]; then
  echo
  echo "FAIL: these are not in roundtrip_known_failures.tsv:"
  printf '%s\n' "$unexpected" | sed 's/^/  /'
  status=1
fi
if [ -n "$fixed" ]; then
  echo
  echo "FAIL: these are recorded as failing but now pass, remove them:"
  printf '%s\n' "$fixed" | sed 's/^/  /'
  status=1
fi
[ "$status" -eq 0 ] && echo "known failures: $(printf '%s\n' "$expected" | grep -c . ) recorded, all accounted for"
exit "$status"
