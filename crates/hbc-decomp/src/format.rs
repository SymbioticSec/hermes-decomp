#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderLayout {
    Legacy,
    Modern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionHeaderLayout {
    Legacy16,
    Modern12,
}

#[derive(Debug, Clone)]
pub struct BytecodeHeader {
    pub magic: u64,
    pub version: u32,
    pub source_hash: [u8; 20],
    pub file_length: u32,
    pub global_code_index: u32,
    pub function_count: u32,
    pub string_kind_count: u32,
    pub identifier_count: u32,
    pub string_count: u32,
    pub overflow_string_count: u32,
    pub string_storage_size: u32,
    pub big_int_count: Option<u32>,
    pub big_int_storage_size: Option<u32>,
    pub reg_exp_count: u32,
    pub reg_exp_storage_size: u32,
    pub literal_value_buffer_size: Option<u32>,
    pub array_buffer_size: Option<u32>,
    pub obj_key_buffer_size: u32,
    pub obj_value_buffer_size: Option<u32>,
    pub obj_shape_table_count: Option<u32>,
    pub num_string_switch_imms: Option<u32>,
    pub segment_id: Option<u32>,
    pub cjs_module_offset: Option<u32>,
    pub cjs_module_count: u32,
    pub function_source_count: Option<u32>,
    pub debug_info_offset: u32,
    pub options: u8,
    pub layout: HeaderLayout,
    pub function_header_layout: FunctionHeaderLayout,
}

#[derive(Debug, Clone)]
pub struct LegacyFunctionHeader {
    pub function_id: u32,
    pub offset: u32,
    pub param_count: u32,
    pub bytecode_size_in_bytes: u32,
    pub function_name: u32,
    pub info_offset: u32,
    pub frame_size: u32,
    pub environment_size: u32,
    pub highest_read_cache_index: u32,
    pub highest_write_cache_index: u32,
    pub flags: u8,
}

#[derive(Debug, Clone)]
pub struct ModernFunctionHeader {
    pub function_id: u32,
    pub offset: u32,
    pub param_count: u32,
    pub loop_depth: u32,
    pub bytecode_size_in_bytes: u32,
    pub function_name: u32,
    pub number_reg_count: u32,
    pub non_ptr_reg_count: u32,
    pub frame_size: u32,
    pub read_cache_size: u8,
    pub write_cache_size: u8,
    pub num_cache_new_object: u8,
    pub private_name_cache_size: u8,
    pub flags: u8,
}

#[derive(Debug, Clone)]
pub enum FunctionHeader {
    Legacy(LegacyFunctionHeader),
    Modern(ModernFunctionHeader),
}

impl FunctionHeader {
    pub fn function_id(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.function_id,
            FunctionHeader::Modern(header) => header.function_id,
        }
    }

    pub fn offset(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.offset,
            FunctionHeader::Modern(header) => header.offset,
        }
    }

    pub fn bytecode_size_in_bytes(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.bytecode_size_in_bytes,
            FunctionHeader::Modern(header) => header.bytecode_size_in_bytes,
        }
    }

    pub fn function_name(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.function_name,
            FunctionHeader::Modern(header) => header.function_name,
        }
    }

    pub fn frame_size(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.frame_size,
            FunctionHeader::Modern(header) => header.frame_size,
        }
    }

    pub fn param_count(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.param_count,
            FunctionHeader::Modern(header) => header.param_count,
        }
    }

    pub fn flags(&self) -> u8 {
        match self {
            FunctionHeader::Legacy(header) => header.flags,
            FunctionHeader::Modern(header) => header.flags,
        }
    }

    pub fn is_overflowed(&self) -> bool {
        self.flags() & 0x20 != 0
    }

    // Check if the function prohibits construction (cannot be used with `new`).
    // This is a strong indicator of arrow functions.
    pub fn prohibit_construct(&self) -> bool {
        self.flags() & 0x10 != 0
    }

    // Check if the function is in strict mode.
    pub fn is_strict(&self) -> bool {
        self.flags() & 0x01 != 0
    }

    // Heuristic: a function is likely an arrow function if it prohibits construction.
    // Arrow functions in JS cannot be used as constructors.
    pub fn is_likely_arrow(&self) -> bool {
        self.prohibit_construct()
    }

    // Get the environment size (closure slots) - only available in Legacy headers.
    pub fn environment_size(&self) -> Option<u32> {
        match self {
            FunctionHeader::Legacy(header) => Some(header.environment_size),
            FunctionHeader::Modern(_) => None,
        }
    }
}
