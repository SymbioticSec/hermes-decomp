# CLI reference

Binary: **`hermes-decomp`**. Input: a `.hbc` or React Native `.bundle`.

## Common flags

| Flag | Description |
|---|---|
| `--layout <auto\|legacy\|modern>` | File header layout (default: `auto`) |
| `--function-layout <auto\|legacy16\|modern12>` | Per-function header layout (default: `auto`) |
| `--format-version <N>` | Override detected HBC bytecode version |

## Commands

### Info / versions

```bash
hermes-decomp info app.hbc
hermes-decomp versions          # HBC opcode tables 40-99
```

### Disasm

```bash
hermes-decomp disasm app.hbc --function 5 --output disasm.txt
# --show-offsets  --no-labels  --no-strings  --info
```

### Decompile

```bash
hermes-decomp decompile app.hbc -o decompiled.js
hermes-decomp decompile app.hbc --function 5
hermes-decomp decompile app.hbc --modules 100-150,200
hermes-decomp decompile app.hbc --module-name "Login*,Auth*"
hermes-decomp decompile app.hbc --exclude-module-name "react*,lodash*"
hermes-decomp decompile app.hbc --from-module 42 --module-depth 3
hermes-decomp decompile app.hbc --function 5 --json
```

Useful options: `--resolve-closures`, `--expand` / `--expand-depth N`,
`--show-offsets`, `--no-strings`, `--no-propagate`, `--no-simplify`,
`--no-structure`, `--check-dead-code`, `--assembly`, `--json`,
`--modules`, `--module-name`, `--exclude-module-name`,
`--from-module`, `--module-depth`, `--no-cache`.

Full-bundle `decompile` and runs with `-o` print **progress on stderr**.

**Analysis cache:** first run writes `<input>.hdcache` next to the file
(~0.2s reloads later). Keyed by SHA-256(bytecode) **and** SHA-256(binary).
Delete the `.hdcache` or pass `--no-cache` to force a rebuild.

### Explore / analyze

```bash
hermes-decomp tui app.hbc
hermes-decomp tui app.hbc --input2 app_v2.hbc   # split-view diff
# --diff-code

hermes-decomp bin-diff v1.hbc v2.hbc            # --diff-code
hermes-decomp xref app.hbc --query "loginWithToken"
hermes-decomp xref app.hbc --query 42 --kind function

hermes-decomp graphviz app.hbc --function 5 --open
hermes-decomp callgraph app.hbc
hermes-decomp callgraph app.hbc --function 42 --depth 3 --dot > calls.dot

hermes-decomp extract app.hbc -o modules/
hermes-decomp modules app.hbc --limit 50
hermes-decomp deps app.hbc --module 0 --depth 3

hermes-decomp dump app.hbc --kind strings
hermes-decomp dump app.hbc --kind obj-shapes --json
# kinds: strings, functions, cjs-modules, regexp, obj-shapes,
#   function-sources, string-kinds, sections, big-int, array-buffer

hermes-decomp closures app.hbc --function 5
hermes-decomp debug app.hbc --vars    # also --scopes, --callees
```

### Secrets / Frida

```bash
hermes-decomp secrets app.hbc
hermes-decomp secrets app.hbc --json --show-full

hermes-decomp frida-hooks app.hbc --module 42 -o ./hooks
hermes-decomp frida-hooks app.hbc --module 42 --export "login,logout" -o ./hooks
# writes before.js / after.js / agent.js / run.sh
```

### Bytecode write path (not JS recompilation)

HASM = our disasm dialect. Patches the binary. Does **not** recompile decompiled JS.

```bash
hermes-decomp emit-hasm app.hbc --function 5 -o f5.hasm
hermes-decomp asm app.hbc f5.hasm --function 5 -o app_patched.hbc
hermes-decomp asm-check app.hbc --function 5

hermes-decomp patch-string app.hbc --old "done" --new "fini" -o app2.hbc
hermes-decomp patch-string app.hbc --id 42 --new "hello" -o app2.hbc
hermes-decomp patch-function app.hbc --function 5 --hasm f5.hasm -o app2.hbc
hermes-decomp inject-stub app.hbc --function 5 --kind log -o app2.hbc
hermes-decomp create --version 96 -o tiny.hbc
```

Legacy files (HBC 96 and below) are fully supported and verified against the real
Hermes VM. `patch-string` handles both same length edits, done in place, and
length changes, where it rebuilds the string table and relocates the tail. It
refuses to patch Hermes packed strings whose storage overlaps another entry.

Modern files (HBC 97 and above, with 12 byte headers) support only same length
string patches. Length changing string patches, function resize, `inject-stub`
resize and `create` fail cleanly on modern files. Those cases need relocation of
the out of line large function headers, which is not implemented yet. The CLI
prints a warning when it detects a modern file. Nothing broken is ever written,
because unsupported cases stop with an error.

#### Why modern output cannot be verified inside the Rust tool

The correctness of a patched `.hbc` is checked by running it on a real Hermes VM.
For HBC 96 and below a standalone `hermes` binary exists in the facebook/hermes
releases. For HBC 97 and above there is no prebuilt host VM binary, and no way to
run one from Rust. Hermes is a C++ engine. Its `hermesvm` shared library exports
only C++ symbols (name mangled, using `std::shared_ptr` and JSI) with no C ABI,
and there is no Rust binding to its VM. A real modern VM can therefore only be
driven from C++.

The Rust crate stays fully Rust, with no C++, no FFI, and no C++ in `build.rs`.
The modern verifier is a separate helper that runs only on macOS. It is a small
C++ program that links the `hermesvm` framework from the
`com.facebook.hermes:hermes-ios` Maven artifact and runs a `.hbc`. Build it on
macOS with:

```bash
bash scripts/build/build_hermes_v98_toolchain.sh
# writes examples/react-native/.toolchains/hermes-v98/ with hermesc, framework, hermes-run
```

The `hermes-ios` artifact ships Apple frameworks, so this verifier runs only on
macOS. On Linux and Windows there is no prebuilt modern host VM. Build Hermes from
source, or run the `.hbc` on an Android device whose app embeds a matching
`libhermes.so`.

### Self-update

```bash
hermes-decomp update --check
hermes-decomp update --install
hermes-decomp update --version v0.1.7
```

Optional: `HERMES_DECOMP_UPDATE_CHECK=1` for a one-line notice when a newer release exists.
