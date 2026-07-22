// Create a minimal valid `.hbc` from scratch.

use crate::error::{Error, Result};
use crate::file::Instruction;
use crate::opcode::{BytecodeFormat, Operand, OperandType, OperandValue};

use super::encode::encode_function_body;
use super::serialize::{build_minimal_legacy, build_minimal_modern};

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub version: u32,
    /// Optional pre-encoded global body. If empty, a trivial
    /// `LoadConstUndefined; Ret` is built for the version.
    pub global_body: Vec<u8>,
    pub strings: Vec<String>,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            version: 96,
            global_body: Vec::new(),
            strings: vec!["global".into()],
        }
    }
}

// Build a minimal loadable bytecode image.
pub fn create_minimal(options: &CreateOptions) -> Result<Vec<u8>> {
    let body = if options.global_body.is_empty() {
        let format = BytecodeFormat::for_version_or_latest(options.version)?.0;
        let load = format
            .definitions
            .iter()
            .find(|d| d.name == "LoadConstUndefined")
            .map(|d| d.opcode)
            .ok_or_else(|| Error::Write("LoadConstUndefined missing".into()))?;
        let ret = format
            .definitions
            .iter()
            .find(|d| d.name == "Ret")
            .map(|d| d.opcode)
            .ok_or_else(|| Error::Write("Ret missing".into()))?;
        let insns = vec![
            Instruction {
                offset: 0,
                opcode: load,
                operands: vec![Operand {
                    ty: OperandType::Reg8,
                    value: OperandValue::U8(0),
                }],
                length: 2,
            },
            Instruction {
                offset: 2,
                opcode: ret,
                operands: vec![Operand {
                    ty: OperandType::Reg8,
                    value: OperandValue::U8(0),
                }],
                length: 2,
            },
        ];
        encode_function_body(&format, &insns)?
    } else {
        options.global_body.clone()
    };

    let mut strings = options.strings.clone();
    if strings.is_empty() {
        strings.push("global".into());
    }
    if options.version >= 97 {
        build_minimal_modern(options.version, &strings, &body)
    } else {
        build_minimal_legacy(options.version, &strings, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::BytecodeFile;
    use crate::write::footer::verify_footer;

    #[test]
    fn create_minimal_v96_parses() {
        let bytes = create_minimal(&CreateOptions {
            version: 96,
            ..Default::default()
        })
        .expect("create");
        assert!(verify_footer(&bytes));
        let file = BytecodeFile::parse_auto(&bytes).expect("parse created file");
        assert_eq!(file.header.version, 96);
        assert_eq!(file.header.function_count, 1);
        assert!(!file.strings.is_empty());
    }

    // Modern create builds an overflowed global with an out-of-line large header.
    // The whole image must parse back with a modern layout and a running v98 VM
    // accepts it (checked separately against the external toolchain).
    #[test]
    fn create_minimal_v98_parses() {
        let bytes = create_minimal(&CreateOptions {
            version: 98,
            global_body: Vec::new(),
            strings: vec!["global".into(), "hello".into()],
        })
        .expect("create v98");
        assert!(verify_footer(&bytes));
        let file = BytecodeFile::parse_auto(&bytes).expect("parse created v98 file");
        assert_eq!(file.header.version, 98);
        assert_eq!(file.header.function_count, 1);
        assert!(matches!(
            file.header.function_header_layout,
            crate::format::FunctionHeaderLayout::Modern12
        ));
        // The global is overflowed, so parsing had to follow the out-of-line large
        // header pointer to recover the body. A non-zero body offset and size prove
        // that indirection resolved.
        assert!(file.function_headers[0].offset() > 0);
        assert_eq!(file.strings.len(), 2);
    }
}
