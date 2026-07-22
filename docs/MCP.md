# MCP server

`hermes-mcp` exposes decompiler features as MCP tools for AI assistants
(Claude, Cursor, GPT, etc.).

## Build

```bash
cargo build --release -p hbc-decomp-mcp
# binary: target/release/hermes-mcp
```

## Config (stdio)

Template: [`mcp-config.example.json`](../mcp-config.example.json)

```json
{
  "mcpServers": {
    "hermes-decompiler": {
      "command": "/path/to/target/release/hermes-mcp"
    }
  }
}
```

## HTTP transport

```bash
hermes-mcp                                  # stdio (default)
hermes-mcp --transport http                 # 127.0.0.1:8744/mcp
hermes-mcp --transport http --host 0.0.0.0 --port 9000 --path /mcp
```

```json
{ "mcpServers": { "hermes-decompiler": { "url": "http://127.0.0.1:8744/mcp" } } }
```

Each HTTP connection gets its own isolated session.

## Tools

| Tool | Description |
|------|-------------|
| `load_file` | Load a `.hbc` file (must be called first) |
| `file_info` | File header info (version, counts) |
| `decompile_function` | Decompile one function (fast) |
| `decompile_function_full` | Full pipeline (IPA, closures, ESM) |
| `decompile_all` | All functions, grouped by Metro module |
| `decompile_module` | One Metro module as ESM |
| `get_ir_json` | Structured JSON IR |
| `disassemble` | Raw bytecode disassembly |
| `xref_search` | Cross-references |
| `list_modules` | Metro modules (names + export counts) |
| `module_deps` | Module dependency tree |
| `module_exports` | Export names → function IDs |
| `callgraph` | Call graph (text or DOT) |
| `function_info` | Per-function metadata banner |
| `closures` | Closure slot mappings |
| `debug_info` | Variable names, scopes, callees |
| `dead_code` | Unreachable from Metro roots |
| `graphviz` | CFG of a function (DOT) |
| `dump` / `dump_table` | Strings, headers, structural tables |
| `list_versions` | Supported HBC versions (40-99) |
