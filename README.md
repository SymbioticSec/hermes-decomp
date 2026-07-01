![Hermes Bytecode Decompiler](readme_banner.png)

[![Build](https://github.com/SymbioticSec/hermes-decomp/actions/workflows/build.yml/badge.svg)](https://github.com/SymbioticSec/hermes-decomp/actions/workflows/build.yml)

A Rust-based decompiler for Hermes bytecode files (`.hbc`), the JavaScript engine used by React Native applications.

## Installation

### Download pre-built binaries

Every push is built automatically by GitHub Actions for **Linux**, **macOS** (Apple Silicon), and **Windows**.
Grab the latest binaries (`hermes-decomp` and the `hermes-mcp` server) from the
[Actions tab](https://github.com/SymbioticSec/hermes-decomp/actions/workflows/build.yml):
open the most recent successful run and download the artifact for your platform:

- `hermes-decomp-x86_64-unknown-linux-gnu`
- `hermes-decomp-aarch64-apple-darwin`
- `hermes-decomp-x86_64-pc-windows-msvc`

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

### Commands

**1. Info**
Display metadata about the HBC file (version, headers, counts).
```bash
hermes-decomp info app.hbc
```

**2. Disasm**
Disassemble bytecode instructions into readable mnemonics.
```bash
hermes-decomp disasm app.hbc --function 5 --output disasm.txt
# Options:
#   --show-offsets    Show bytecode offsets
#   --no-labels       Hide jump labels
#   --no-strings      Don't resolve string IDs
#   --info            Prepend a per-function metadata banner (params, frame,
#                     registers, size, offset, flags, exception-handler count)
```

![Disassembly Example](disasm.png)

**3. Decompile**
Lift bytecode into readable JavaScript (with control-flow recovery, naming, and ESM output for Metro bundles).
```bash
hermes-decomp decompile app.hbc --output decompiled.js
hermes-decomp decompile app.hbc --function 5
# Options:
#   --resolve-closures    Closure resolution across functions (auto-enabled when decompiling all)
#   --expand              Inline referenced functions
#   --expand-depth N      Expansion depth (default: 2)
#   --show-offsets        Include bytecode offsets as comments
#   --no-strings          Don't resolve string IDs
#   --no-propagate        Disable constant/copy propagation
#   --no-simplify         Disable expression simplification
#   --no-structure        Disable if/while/for reconstruction
#   --check-dead-code     Report functions unreachable from Metro roots
#   --assembly            Binary Ninja-style output with absolute offsets
#   --json                Export IR as JSON instead of JS
#   --no-cache            Skip the on-disk analysis cache (always re-analyze)
```

> **Analysis cache:** the first decompile/modules/deps/extract run on a file
> writes a `<input>.hdcache` next to it holding the full analysis. Subsequent
> runs (any of those commands, the TUI, and the MCP server) load it in ~0.2s
> instead of re-running the multi-second pipeline. The cache is keyed by a
> SHA-256 of the bytecode, so it rebuilds automatically when the file changes.
> Delete the `.hdcache` file or pass `--no-cache` to force a fresh analysis.

![Decompilation Example](decompile.png)

**4. BinDiff**
Compare two HBC files to find added, removed, or modified functions.
```bash
hermes-decomp bin-diff v1.hbc v2.hbc
#   --diff-code    Compare decompiled code for modified functions
```

**5. TUI**
Interactive terminal interface to browse functions and switch between disassembly and decompiled view.
```bash
hermes-decomp tui app.hbc

# Split-View BinDiff
hermes-decomp tui app.hbc --input2 app_v2.hbc
```

**6. Xref**
Find cross-references to strings or functions.
```bash
hermes-decomp xref app.hbc --query "loginWithToken"
hermes-decomp xref app.hbc --query 42 --kind function
```

**7. Graphviz**
Generate a Control Flow Graph (DOT format).
```bash
hermes-decomp graphviz app.hbc --function 5 --open
hermes-decomp graphviz app.hbc --function 5 --output cfg.dot
```

**8. Callgraph**
Build the function call graph (text or Graphviz DOT), optionally a depth-limited subgraph from a root function.
```bash
hermes-decomp callgraph app.hbc
hermes-decomp callgraph app.hbc --function 42 --depth 3
hermes-decomp callgraph app.hbc --function 42 --dot --output calls.dot
```

**9. Extract**
Extract all Metro modules into separate files (full-quality ESM per module).
```bash
hermes-decomp extract app.hbc --output modules/
```

**10. Modules / Deps**
Inspect Metro module registry and dependencies.
```bash
hermes-decomp modules app.hbc
hermes-decomp modules app.hbc --limit 50
hermes-decomp deps app.hbc --module 0 --depth 3
```

**11. Dump**
Extract raw structural data from the bytecode file. `--json` emits machine-readable output.
```bash
hermes-decomp dump app.hbc --kind strings
hermes-decomp dump app.hbc --kind functions
hermes-decomp dump app.hbc --kind obj-shapes --json
# --kind values: strings, functions, identifiers, cjs-modules, regexp,
#   obj-shapes, function-sources, string-kinds, sections, big-int, array-buffer
```

**12. Closures**
Show closure slot mappings for a function.
```bash
hermes-decomp closures app.hbc --function 5
```

**13. Debug**
Show debug info (variable names, scopes, callees).
```bash
hermes-decomp debug app.hbc --vars
hermes-decomp debug app.hbc --scopes
hermes-decomp debug app.hbc --callees
```

**14. Versions**
List all supported Hermes bytecode versions.
```bash
hermes-decomp versions
```

**15. JSON Export**
Export the Intermediate Representation (IR) in JSON format for external tools.
```bash
hermes-decomp decompile app.hbc --function 5 --json
hermes-decomp decompile app.hbc --json
```

## MCP Server (AI Integration)

The project includes an MCP (Model Context Protocol) server that exposes all decompiler features as tools for AI assistants (Claude, GPT, etc.).

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
subprocess). It can also serve over **Streamable HTTP** for remote/multiple clients —
each connection gets its own isolated session:

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
| `recover_structures`| `true` | Reconstructs `if`, `while`, `for` from jumps. |

## Technical Overview

### What is Hermes?
Hermes is a JavaScript engine optimized for React Native. Unlike V8 or JSC which parse JS source at runtime, Hermes precompiles JavaScript into **bytecode** (`.hbc`) during the build process. This improves startup time but makes reverse engineering harder.

### Decompilation Process

1.  **Parsing**: The binary HBC file is parsed to extract headers, string tables, and raw bytecode instructions.
2.  **Disassembly**: Raw bytes are converted into readable opcodes (e.g., `Mov`, `Call`, `Add`).
3.  **IR Generation**: Bytecode is lifted into a high-level **Intermediate Representation (IR)**.
    *   Registers (`r0`, `r1`) are mapped to variables.
    *   Control flow (Jumps) is analyzed to build a Control Flow Graph (CFG).
4.  **Analysis & Transformation**:
    *   **Data Flow**: Constant propagation, copy propagation.
    *   **Structure Recovery**: Reconstructing `if`, `while`, `for` loops from graph edges.
    *   **Pattern Matching**: Detecting `class`, `async`, `generator` state machines.
5.  **Code Generation**: The optimized IR is converted back into valid JavaScript syntax.

## Contributing

Contributions are welcome!

**Please open an issue first** before submitting a pull request. This lets us discuss the
problem or feature, avoid duplicate work, and agree on an approach before any code is written.

1. [Open an issue](https://github.com/SymbioticSec/hermes-decomp/issues/new) describing the bug or feature.
2. Wait for feedback / confirmation that a PR is welcome.
3. Fork the repo and create a branch from `main`.
4. Make your change and ensure `cargo build --release --workspace` and `cargo test --workspace` pass.
   The CI builds on Linux, macOS, and Windows — keep all three green.
5. Open a pull request that references the issue.

## Resources

- [Hermes Engine](https://hermesengine.dev/)
- [React Native](https://reactnative.dev/)

## License

MIT License - see [LICENSE](LICENSE) for details.
