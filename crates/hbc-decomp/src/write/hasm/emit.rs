// Emit HASM text from bytecode, and assemble HASM back into a patched image.

use crate::error::Result;
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;

use super::parse::parse_hasm_with_context;
use super::HasmModule;

// Emit HASM text for a function via the existing disassembler.
pub fn emit_hasm_function(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
) -> Result<String> {
    crate::disasm::disassemble_function(
        file,
        format,
        function_id,
        &crate::disasm::DisasmOptions {
            show_offsets: true,
            show_labels: true,
            resolve_strings: true,
            enable_color: false,
        },
    )
}

// Assemble: replace function bodies in `base` from a map of id → hasm text.
pub fn assemble_function_hasm(
    base: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    hasm_text: &str,
) -> Result<Vec<u8>> {
    let insns = parse_hasm_with_context(hasm_text, format, base)?;
    crate::write::patch::patch_function_body(base, format, function_id, &insns, &Default::default())
}

// Assemble a whole module against a base file (each function body replaced when present).
pub fn assemble_module(
    base: &BytecodeFile,
    format: &BytecodeFormat,
    module: &HasmModule,
) -> Result<BytecodeFile> {
    let mut file = base.clone();
    for func in &module.functions {
        if func.instructions.is_empty() {
            continue;
        }
        let _ = crate::write::patch::patch_function_body(
            &mut file,
            format,
            func.id,
            &func.instructions,
            &Default::default(),
        )?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::BytecodeFormat;

    // emit -> parse -> assemble round-trip reparses and keeps the function count.
    #[test]
    fn hasm_roundtrip_v96() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let text = emit_hasm_function(&file, &format, 0).unwrap();
        let out = assemble_function_hasm(&mut file, &format, 0, &text).unwrap();
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.header.function_count, file.header.function_count);
    }
}
