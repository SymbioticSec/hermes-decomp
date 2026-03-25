use crate::error::Result;
use crate::file::{BytecodeFile, Instruction, LiteralValue, ShapeTableEntry, StringTableEntry};
use crate::format::{FunctionHeaderLayout, HeaderLayout};
use crate::opcode::BytecodeFormat;

pub mod buffer;
pub mod function;
pub mod header;
mod helpers;
mod instructions;
mod parsing;
pub mod table;

impl BytecodeFile {
    pub fn parse_auto(bytes: &[u8]) -> Result<Self> {
        parsing::parse_auto(bytes)
    }

    pub fn parse_with_layout(
        bytes: &[u8],
        layout: HeaderLayout,
        function_layout: FunctionHeaderLayout,
    ) -> Result<Self> {
        parsing::parse_with_layout(bytes, layout, function_layout)
    }

    pub fn decode_function_instructions(
        &self,
        format: &BytecodeFormat,
        function_id: u32,
    ) -> Result<Vec<Instruction>> {
        instructions::decode_function_instructions(self, format, function_id)
    }

    pub fn string_at(&self, string_id: u32) -> Option<&StringTableEntry> {
        self.strings.get(string_id as usize)
    }

    pub fn shape_at(&self, shape_id: u32) -> Option<ShapeTableEntry> {
        self.obj_shape_table.get(shape_id as usize).copied()
    }

    // Get BigInt value at the given index.
    pub fn bigint_at(&self, bigint_id: u32) -> Option<String> {
        helpers::bigint_at(self, bigint_id)
    }

    pub fn read_array_buffer_series(&self, offset: u32, count: u32) -> Result<Vec<LiteralValue>> {
        helpers::read_array_buffer_series(self, offset, count)
    }

    pub fn read_key_buffer_series(&self, offset: u32, count: u32) -> Result<Vec<LiteralValue>> {
        helpers::read_key_buffer_series(self, offset, count)
    }

    pub fn read_value_buffer_series(&self, offset: u32, count: u32) -> Result<Vec<LiteralValue>> {
        helpers::read_value_buffer_series(self, offset, count)
    }
}
