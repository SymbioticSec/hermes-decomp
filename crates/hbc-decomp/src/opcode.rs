use serde::Deserialize;

use crate::error::{Error, Result};
use crate::io::ByteReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum OperandType {
    Reg8,
    Reg32,
    UInt8,
    UInt16,
    UInt32,
    UInt8S,
    UInt16S,
    UInt32S,
    Addr8,
    Addr32,
    Imm32,
    Double,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OperandValue {
    U8(u8),
    U16(u16),
    U32(u32),
    I8(i8),
    I32(i32),
    F64(f64),
}

impl OperandValue {
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            OperandValue::U8(v) => Some(*v as u32),
            OperandValue::U16(v) => Some(*v as u32),
            OperandValue::U32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            OperandValue::I8(v) => Some(*v as i32),
            OperandValue::I32(v) => Some(*v),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Operand {
    pub ty: OperandType,
    pub value: OperandValue,
}

impl OperandType {
    pub fn read(self, reader: &mut ByteReader<'_>) -> Result<OperandValue> {
        let value = match self {
            OperandType::Reg8 | OperandType::UInt8 | OperandType::UInt8S => {
                OperandValue::U8(reader.read_u8()?)
            }
            OperandType::Reg32 | OperandType::UInt32 | OperandType::UInt32S => {
                OperandValue::U32(reader.read_u32()?)
            }
            OperandType::UInt16 | OperandType::UInt16S => OperandValue::U16(reader.read_u16()?),
            OperandType::Addr8 => OperandValue::I8(reader.read_i8()?),
            OperandType::Addr32 => OperandValue::I32(reader.read_i32()?),
            OperandType::Imm32 => OperandValue::I32(reader.read_i32()?),
            OperandType::Double => OperandValue::F64(reader.read_f64()?),
        };
        Ok(value)
    }
}

#[derive(Debug, Clone)]
pub struct InstructionDef {
    pub opcode: u8,
    pub name: String,
    pub operand_types: Vec<OperandType>,
    pub is_jump: bool,
    pub abstract_definition: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct AbstractDefinition {
    pub name: String,
    pub variant_opcodes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BytecodeFormat {
    pub version: u32,
    pub definitions: Vec<InstructionDef>,
    pub abstract_definitions: Vec<AbstractDefinition>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct JsonFormat {
    Version: u32,
    Definitions: Vec<JsonDefinition>,
    #[serde(default)]
    AbstractDefinitions: Vec<JsonAbstractDefinition>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct JsonDefinition {
    Opcode: u8,
    Name: String,
    OperandTypes: Vec<OperandType>,
    IsJump: bool,
    #[serde(default)]
    AbstractDefinition: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct JsonAbstractDefinition {
    Name: String,
    VariantOpcodes: Vec<u8>,
}

impl BytecodeFormat {
    pub fn from_json_str(json: &str) -> Result<Self> {
        let parsed: JsonFormat = serde_json::from_str(json)
            .map_err(|err| Error::Parse(format!("failed to parse format json: {err}")))?;

        let mut definitions = Vec::new();
        let mut max_opcode = 0u8;
        for def in &parsed.Definitions {
            max_opcode = max_opcode.max(def.Opcode);
        }
        definitions.resize_with((max_opcode as usize) + 1, || InstructionDef {
            opcode: 0,
            name: "<invalid>".to_string(),
            operand_types: Vec::new(),
            is_jump: false,
            abstract_definition: None,
        });

        for def in parsed.Definitions {
            definitions[def.Opcode as usize] = InstructionDef {
                opcode: def.Opcode,
                name: def.Name,
                operand_types: def.OperandTypes,
                is_jump: def.IsJump,
                abstract_definition: def.AbstractDefinition,
            };
        }

        let abstract_definitions = parsed
            .AbstractDefinitions
            .into_iter()
            .map(|def| AbstractDefinition {
                name: def.Name,
                variant_opcodes: def.VariantOpcodes,
            })
            .collect();

        Ok(Self {
            version: parsed.Version,
            definitions,
            abstract_definitions,
        })
    }

    pub fn for_version(version: u32) -> Result<Self> {
        let json = format_json_for_version(version).ok_or(Error::MissingFormat(version))?;
        Self::from_json_str(json)
    }

    pub fn for_version_or_latest(version: u32) -> Result<(Self, u32)> {
        if let Ok(format) = Self::for_version(version) {
            return Ok((format, version));
        }
        let available = available_versions();
        let mut fallback = None;
        for v in available.iter().copied() {
            if v <= version {
                fallback = Some(v);
            }
        }
        let fallback = fallback.ok_or(Error::MissingFormat(version))?;
        let format = Self::for_version(fallback)?;
        Ok((format, fallback))
    }
}

include!(concat!(env!("OUT_DIR"), "/bytecode_formats.rs"));

// CallBuiltin index -> name table for a given HBC version. The builtin ordering
// is version-specific (parsed from each Hermes release's Builtins.def). If the
// exact version has no table, fall back to the nearest available version that is
// `<= version` (the table changes only on release boundaries).
pub fn builtins_for_version(version: u32) -> Vec<String> {
    let json = builtins_json_for_version(version).or_else(|| {
        builtin_table_versions()
            .iter()
            .rev()
            .find(|&&v| v <= version)
            .and_then(|&v| builtins_json_for_version(v))
    });
    json.and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
        .unwrap_or_default()
}
