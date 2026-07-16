![Hermes Bytecode Decompiler](readme_banner.png)

[![Build](https://github.com/SymbioticSec/hermes-decomp/actions/workflows/build.yml/badge.svg)](https://github.com/SymbioticSec/hermes-decomp/actions/workflows/build.yml)

A Rust-based decompiler for Hermes bytecode files (`.hbc`), the JavaScript engine used by React Native applications. Supports **HBC versions 40-99**.

## Installation

### Download pre-built binaries

Every push is built automatically by GitHub Actions for **Linux**, **macOS** (Apple Silicon / Intel), and **Windows**.
Grab the latest binaries (`hermes-decomp` and the `hermes-mcp` server) from the
[Actions tab](https://github.com/SymbioticSec/hermes-decomp/actions/workflows/build.yml)
or from [GitHub Releases](https://github.com/SymbioticSec/hermes-decomp/releases):

| Asset suffix | Platform |
|---|---|
| `linux-x86_64` | Linux x86_64 |
| `linux-arm64` | Linux aarch64 |
| `macos-arm64` | macOS Apple Silicon |
| `macos-x86_64` | macOS Intel |
| `windows-x86_64` | Windows x86_64 |

Each release archive contains `hermes-decomp` and `hermes-mcp`. Verify with `shasum -a 256 -c SHA256SUMS`.

### Self-update

If you already have a release binary installed:

```bash
hermes-decomp update --check     # print latest version + changelog
hermes-decomp update --install   # download, SHA-256 verify, replace in place
hermes-decomp update --version v0.1.7   # pin a specific release tag
```

Optional: set `HERMES_DECOMP_UPDATE_CHECK=1` to print a one-line notice when a newer release is available.

### Build from Source

#### Prerequisites

- Rust 1.70 or later
- Cargo (comes with Rust)

```bash
git clone https://github.com/SymbioticSec/hermes-decomp.git
cd hermes-decomp
cargo build --release
```

The binaries will be at `target/release/hermes-decomp` and `target/release/hermes-mcp`.

## Usage

Binary name: **`hermes-decomp`**. All commands accept a path to a `.hbc` file or a React Native `.bundle`.

Common optional flags on most commands:

| Flag | Description |
|---|---|
| `--layout <auto\|legacy\|modern>` | File header layout (default: `auto`) |
| `--function-layout <auto\|legacy16\|modern12>` | Per-function header layout (default: `auto`) |
| `--format-version <N>` | Override detected HBC bytecode version |

### Commands

**1. Info**, Print bytecode header info (version, function/string counts, sections).

```bash
hermes-decomp info app.hbc
```

**2. Versions**, List all supported HBC opcode-table versions (40-99).

```bash
hermes-decomp versions
```

**3. Disasm**, Disassemble functions to Hermes assembly.

```bash
hermes-decomp disasm app.hbc --function 5 --output disasm.txt
# Options:
#   --show-offsets    Annotate each instruction with its bytecode offset
#   --no-labels       Don't emit jump labels (raw offsets only)
#   --no-strings      Don't resolve string-table indices to literals
#   --info            Per-function metadata banner (params, frame, regs,
#                     size, offset, flags, exception-handler count)
```

![Disassembly Example](disasm.png)

**4. Decompile**, Lift bytecode into readable JavaScript (control-flow recovery, naming, ESM for Metro bundles).

```bash
hermes-decomp decompile app.hbc --output decompiled.js
hermes-decomp decompile app.hbc --function 5
hermes-decomp decompile app.hbc --modules 100-150,200
hermes-decomp decompile app.hbc --module-name "Login*,Auth*"
hermes-decomp decompile app.hbc --exclude-module-name "react*,lodash*"
hermes-decomp decompile app.hbc --from-module 42 --module-depth 3
# Options:
#   --resolve-closures    Resolve closure vars across functions (auto when decompiling all)
#   --expand              Expand referenced functions inline
#   --expand-depth N      Expansion depth (default: 2)
#   --show-offsets        Include bytecode offsets as comments
#   --no-strings          Don't resolve string-table indices to literals
#   --no-propagate        Disable constant/copy propagation
#   --no-simplify         Disable expression simplification
#   --no-structure        Disable if/while/for reconstruction (flat goto form)
#   --check-dead-code     Report functions unreachable from Metro roots
#   --assembly            Absolute binary offsets on each line
#   --json                Export IR as JSON instead of JS
#   --modules SPEC        Only these Metro module IDs (ranges/list: "100-150,200,5")
#   --module-name GLOB    Only modules whose name matches a glob (comma-separated)
#   --exclude-module-name GLOB  Exclude modules by name glob
#   --from-module N       Emit a module and its dependency subtree
#   --module-depth N      Max dependency depth for --from-module (default: 3)
#   --no-cache            Skip the on-disk analysis cache (always re-analyze)
```

> **Analysis cache:** the first `decompile` / `modules` / `deps` / `extract` run on a file
> writes a `<input>.hdcache` next to it holding the full analysis. Subsequent
> runs (any of those commands, the TUI, and the MCP server) load it in ~0.2s
> instead of re-running the multi-second pipeline. The cache is keyed by
> SHA-256(bytecode) **and** SHA-256(decompiler binary), so it rebuilds when the
> input *or* the tool changes (no manual version bump needed for output fixes).
> Delete the `.hdcache` file or pass `--no-cache` to force a fresh analysis.

![Decompilation Example](decompile.png)

**5. BinDiff**, Compare two HBC files (added / removed / modified functions).

```bash
hermes-decomp bin-diff v1.hbc v2.hbc
#   --diff-code    Compare decompiled code for modified functions (slower)
```

**6. TUI**, Interactive terminal UI (browse, search, decompile, split-view diff).

```bash
hermes-decomp tui app.hbc
hermes-decomp tui app.hbc --input2 app_v2.hbc
#   --diff-code    In diff mode, compare decompiled code per function
```

**7. Xref**, Find cross-references to strings or functions.

```bash
hermes-decomp xref app.hbc --query "loginWithToken"
hermes-decomp xref app.hbc --query 42 --kind function
#   --kind string|function   (default: string)
```

**8. Graphviz**, Emit a Graphviz DOT control-flow graph for a function.

```bash
hermes-decomp graphviz app.hbc --function 5 --open
hermes-decomp graphviz app.hbc --function 5 --output cfg.dot
```

**9. Callgraph**, Print the function call graph (text or Graphviz DOT). Writes to stdout (redirect to save).

```bash
hermes-decomp callgraph app.hbc
hermes-decomp callgraph app.hbc --function 42 --depth 3
hermes-decomp callgraph app.hbc --function 42 --dot > calls.dot
#   --function N   Root function for a depth-limited subgraph
#   --depth N      Max hops from --function (default: 3)
#   --dot          Emit Graphviz DOT instead of a text edge listing
```

**10. Extract**, Extract each Metro module to its own ESM file.

```bash
hermes-decomp extract app.hbc --output modules/
#   --no-strings   Don't resolve string-table indices to literals
```

**11. Modules / Deps**, Inspect the Metro module registry and dependency tree.

```bash
hermes-decomp modules app.hbc
hermes-decomp modules app.hbc --limit 50
hermes-decomp deps app.hbc --module 0 --depth 3
```

**12. Dump**, Dump raw structural tables from the bytecode file. `--json` emits machine-readable output.

```bash
hermes-decomp dump app.hbc --kind strings
hermes-decomp dump app.hbc --kind functions
hermes-decomp dump app.hbc --kind obj-shapes --json
# --kind values: strings, functions, cjs-modules, regexp,
#   obj-shapes, function-sources, string-kinds, sections, big-int, array-buffer
```

**13. Closures**, Show closure slot mappings for a function.

```bash
hermes-decomp closures app.hbc --function 5
```

**14. Debug**, Show embedded debug info (variable names, scopes, callees).

```bash
hermes-decomp debug app.hbc --vars
hermes-decomp debug app.hbc --scopes
hermes-decomp debug app.hbc --callees
```

**15. Update**, Check for and install updates from GitHub releases.

```bash
hermes-decomp update --check
hermes-decomp update --install
hermes-decomp update --version v0.1.7
```

**16. JSON IR export**, Use `decompile --json` (not a separate subcommand).

```bash
hermes-decomp decompile app.hbc --function 5 --json
hermes-decomp decompile app.hbc --json
```

## MCP Server (AI Integration)

The project includes an MCP (Model Context Protocol) server that exposes decompiler features as tools for AI assistants (Claude, GPT, etc.).

### Build

```bash
cargo build --release -p hbc-decomp-mcp
```

### Configuration

Add to your AI assistant's MCP config (e.g. `claude_desktop_config.json`, Cursor, etc.).
A ready-to-edit template is provided at [`mcp-config.example.json`](mcp-config.example.json):

```json
{
  "mcpServers": {
    "hermes-decompiler": {
      "command": "/path/to/target/release/hermes-mcp"
    }
  }
}
```

### Transports

By default the server speaks MCP over **stdio** (the config above launches it as a
subprocess). It can also serve over **Streamable HTTP** for remote/multiple clients, each connection gets its own isolated session:

```bash
hermes-mcp                                  # stdio (default)
hermes-mcp --transport http                 # Streamable HTTP on 127.0.0.1:8744/mcp
hermes-mcp --transport http --host 0.0.0.0 --port 9000 --path /mcp
```

Point an HTTP-capable MCP client at the URL instead of a command:

```json
{ "mcpServers": { "hermes-decompiler": { "url": "http://127.0.0.1:8744/mcp" } } }
```

### Available Tools

| Tool | Description |
|------|-------------|
| `load_file` | Load a `.hbc` file (must be called first) |
| `file_info` | File header info (version, counts) |
| `decompile_function` | Decompile one function to JS (fast, single-function) |
| `decompile_function_full` | Decompile one function with the full pipeline (IPA, closures, ESM) |
| `decompile_all` | Decompile all functions, grouped by Metro module |
| `decompile_module` | Decompile a whole Metro module as ESM |
| `get_ir_json` | Structured JSON IR for analysis |
| `disassemble` | Raw bytecode disassembly |
| `xref_search` | Cross-references to strings or functions |
| `list_modules` | List Metro modules (names + export counts) |
| `module_deps` | Module dependency tree (named) |
| `module_exports` | List a module's exported names and their function IDs |
| `callgraph` | Function call graph (text or DOT), optional depth-limited subgraph |
| `function_info` | Per-function metadata banner (params, frame, regs, flags) |
| `closures` | Closure slot mappings for a function |
| `debug_info` | Debug info (variable names, scopes, callees) |
| `dead_code` | Functions unreachable from Metro roots |
| `graphviz` | Control-flow graph of a function (DOT) |
| `dump` | Dump strings or function headers |
| `dump_table` | Dump a structural table (cjs-modules, regexp, obj-shapes, sections, …) |
| `list_versions` | Supported bytecode versions (HBC 40-99) |

## Library Usage (Core API)

The core library `hbc-decomp` can be used in other Rust projects.

### Add to Cargo.toml

```toml
[dependencies]
hbc-decomp = { git = "https://github.com/SymbioticSec/hermes-decomp" }
```

### Example Usage

```rust
use hbc_decomp::{Decompiler, DecompileOptionsV2};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("app.hbc")?;
    let mut decompiler = Decompiler::new(&bytes)?;

    // Optional: build closure context for cross-function analysis
    decompiler.build_closure_context()?;

    let options = DecompileOptionsV2::optimized();

    // Decompile everything
    let code = decompiler.decompile_all(&options)?;
    println!("{}", code);

    // Or export IR for programmatic analysis
    let ir = decompiler.decompile_to_ir(0, &options)?;

    Ok(())
}
```

### Configuration Options

| Option | Default | Description |
|--------|---------|-------------|
| `resolve_strings` | `true` | Replaces string IDs with actual text. |
| `include_offsets` | `false` | Adds bytecode offset comments. |
| `propagate` | `true` | Constant and copy propagation. |
| `simplify` | `true` | Cleans up intermediate temporaries. |
| `recover_structures` | `true` | Reconstructs `if`, `while`, `for` from jumps. |

## Technical Overview

### What is Hermes?

Hermes is a JavaScript engine optimized for React Native. Unlike V8 or JSC which parse JS source at runtime, Hermes precompiles JavaScript into **bytecode** (`.hbc`) during the build process. This improves startup time but makes reverse engineering harder.

### Decompilation Process

1. **Parsing**: The binary HBC file is parsed to extract headers, string tables, and raw bytecode instructions.
2. **Disassembly**: Raw bytes are converted into readable opcodes (e.g., `Mov`, `Call`, `Add`).
3. **IR Generation**: Bytecode is lifted into a high-level **Intermediate Representation (IR)**.
   - Registers (`r0`, `r1`) are mapped to variables.
   - Control flow (jumps) is analyzed to build a Control Flow Graph (CFG).
4. **Analysis & Transformation**:
   - **Data Flow**: Constant propagation, copy propagation.
   - **Structure Recovery**: Reconstructing `if`, `while`, `for` loops from graph edges.
   - **Pattern Matching**: Detecting `class`, `async`, `generator` state machines.
   - **Metro / ESM**: Module factories unwrapped; `require`/`exports` rewritten to `import`/`export`.
5. **Code Generation**: The optimized IR is converted back into valid JavaScript syntax.

## Contributing

Contributions are welcome!

**Please open an issue first** before submitting a pull request. This lets us discuss the
problem or feature, avoid duplicate work, and agree on an approach before any code is written.

1. [Open an issue](https://github.com/SymbioticSec/hermes-decomp/issues/new) describing the bug or feature.
2. Wait for feedback / confirmation that a PR is welcome.
3. Fork the repo and create a branch from `main`.
4. Make your change and ensure `cargo build --release --workspace` and `cargo test --workspace` pass.
   The CI builds on Linux, macOS, and Windows, keep all three green.
5. Open a pull request that references the issue.

## Resources

- [Hermes Engine](https://hermesengine.dev/)
- [React Native](https://reactnative.dev/)

## License

MIT License - see [LICENSE](LICENSE) for details.
