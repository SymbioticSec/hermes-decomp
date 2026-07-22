# Contributing

Contributions are welcome.

**Please open an issue first** before submitting a pull request. That avoids
duplicate work and lets us agree on an approach before code is written.

1. [Open an issue](https://github.com/SymbioticSec/hermes-decomp/issues/new) for the bug or feature.
2. Wait for feedback / confirmation that a PR is welcome.
3. Fork the repo and create a branch from `main`.
4. Make your change. Ensure:

   ```bash
   cargo build --release --workspace
   cargo test --workspace
   ```

   CI builds on **Linux**, **macOS**, and **Windows** keep all three green.

5. Open a pull request that references the issue.

## Docs

| File | Content |
|------|---------|
| [README.md](README.md) | Overview, install, quick start |
| [docs/USAGE.md](docs/USAGE.md) | Full CLI reference |
| [docs/MCP.md](docs/MCP.md) | MCP server setup & tools |
| [docs/LIBRARY.md](docs/LIBRARY.md) | Rust crate API |

## Scope notes

- Decompiler output is **best-effort** recovery, not source restoration.
- Bytecode patching (`asm`, `patch-*`, …) does **not** recompile decompiled JS.
