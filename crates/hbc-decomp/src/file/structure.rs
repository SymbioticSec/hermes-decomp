use crate::debug::DebugInfo;
use crate::format::{BytecodeHeader, FunctionHeader};
use crate::opcode::Operand;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKindType {
    String,
    Identifier,
}

#[derive(Debug, Clone)]
pub struct StringKindEntry {
    pub kind: StringKindType,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct StringTableEntry {
    pub value: String,
    pub is_utf16: bool,
    pub is_identifier: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct TableEntry {
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ShapeTableEntry {
    pub key_buffer_offset: u32,
    pub num_props: u32,
}

#[derive(Debug, Clone)]
pub enum LiteralValue {
    Null,
    Bool(bool),
    Number(f64),
    Integer(i32),
    String(String),
    Undefined,
}

#[derive(Debug, Clone)]
pub struct SectionInfo {
    pub name: &'static str,
    pub offset: u32,
    pub size: u32,
    pub entries: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct ExceptionHandler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
}

#[derive(Debug, Clone)]
pub struct BytecodeFile {
    pub header: BytecodeHeader,
    pub function_headers: Vec<FunctionHeader>,
    pub string_kinds: Vec<StringKindEntry>,
    pub identifier_hashes: Vec<u32>,
    pub strings: Vec<StringTableEntry>,
    pub big_int_table: Vec<TableEntry>,
    pub big_int_storage: Vec<u8>,
    pub reg_exp_table: Vec<TableEntry>,
    pub reg_exp_storage: Vec<u8>,
    pub array_buffer: Vec<u8>,
    pub literal_value_buffer: Vec<u8>,
    pub obj_key_buffer: Vec<u8>,
    pub obj_value_buffer: Vec<u8>,
    pub obj_shape_table: Vec<ShapeTableEntry>,
    pub cjs_module_table: Vec<(u32, u32)>,
    pub function_source_table: Vec<(u32, u32)>,
    pub instruction_offset: u32,
    pub instructions: Vec<u8>,
    pub debug_info: Option<DebugInfo>,
    pub exception_handlers: BTreeMap<u32, Vec<ExceptionHandler>>,
    pub sections: Vec<SectionInfo>,
    /// Original file bytes when parsed from disk. Used by the write path for
    /// identity serialize and surgical patches (keeps overflow headers intact).
    pub raw_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct Instruction {
    pub offset: u32,
    pub opcode: u8,
    pub operands: Vec<Operand>,
    pub length: u32,
}
