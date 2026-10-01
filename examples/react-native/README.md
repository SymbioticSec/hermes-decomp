# React Native / Hermes per-version corpus

A local, reproducible corpus that compiles the same JS with **every obtainable
Hermes compiler version**, decompiles it, and validates the result — so the
decompiler can be checked and improved version by version. Everything here is
local (this tree is gitignored); nothing is committed.

## Layout
```
sample.js                 canonical "app" (broad feature spread, deterministic output)
expressions/*.js          one tiny program per expression / operator / feature
.toolchains/              downloaded hermes CLIs + manifest.tsv (hbc_version -> tag -> compiler)
v<N>/                     one folder per HBC bytecode version
  source.js bytecode.hbc decompiled.js disasm.txt quality.json
  expressions/<name>/{source.js,bytecode.hbc,decompiled.js[,COMPILE_GAP.txt]}
  roundtrip.tsv
CORPUS_REPORT.md          aggregate metrics across versions
FINDINGS.md               current baseline + prioritized bugs
```

## Workflow
```bash
# 1. Download every prebuilt Hermes CLI (darwin) and map each to its HBC version.
bash scripts/build/fetch_hermesc.sh           # ~300 MB cached locally

# 2. Compile the sample + expression suite with each compiler, decompile each.
bash scripts/build/build_corpus.sh

# 3. Measure parse/decode robustness + output quality per version.
cargo run --release -p hbc-decomp --example corpus_report

# 4. A->Z functional round-trip: run original vs decompiled under node, diff.
bash scripts/build/roundtrip.sh
cargo run --release -p hbc-decomp --example corpus_report   # fold round-trip into report
```

## Notes
- HBC output version is fixed by the compiler binary; there is no "emit version N"
  flag. Prebuilt darwin CLIs from facebook/hermes cover **HBC 59->96**
  (tags v0.1.0->v0.13.0). Older versions (40-58) predate published binaries and
  need building hermesc from source (`scripts/build/build_hermesc_from_source.sh`,
  best-effort).
- Some snippets don't compile on older Hermes (e.g. ES6 `class` is unsupported by
  the static compiler through v0.13); these are marked `COMPILE_GAP.txt` and
  excluded from the round-trip — they are Hermes limitations, not decompiler bugs.
- To compile from a full React Native app instead of these snippets: build the app
  with Hermes enabled and point the decompiler at the produced `.hbc`/`.bundle`.
```
cargo run --release -- decompile <bundle>     # or: tui <bundle>
```
