# Library API (`hbc-decomp`)

## Cargo

```toml
[dependencies]
hbc-decomp = { git = "https://github.com/SymbioticSec/hermes-decomp" }
```

## Example

```rust
use hbc_decomp::{Decompiler, DecompileOptionsV2};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("app.hbc")?;
    let mut decompiler = Decompiler::new(&bytes)?;
    decompiler.build_closure_context()?;

    let options = DecompileOptionsV2::optimized();
    let code = decompiler.decompile_all(&options)?;
    println!("{code}");

    let ir = decompiler.decompile_to_ir(0, &options)?;
    let _ = ir;
    Ok(())
}
```

## `DecompileOptionsV2`

| Option | Default | Description |
|--------|---------|-------------|
| `resolve_strings` | `true` | Replace string IDs with literals |
| `include_offsets` | `false` | Bytecode offset comments |
| `propagate` | `true` | Constant / copy propagation |
| `simplify` | `true` | Drop intermediate temporaries |
| `recover_structures` | `true` | Reconstruct `if` / `while` / `for` |

## Pipeline (overview)

1. **Parse** HBC headers, strings, bytecode  
2. **Disassemble** → opcodes  
3. **IR** + CFG (registers → variables, jumps → graph)  
4. **Analysis**, data-flow, structure recovery, Metro/ESM, naming/IPA  
5. **Codegen** → JavaScript  

Write path (`write` module): encode / HASM / patch / serialize, **bytecode only**, not JS → hermesc.
