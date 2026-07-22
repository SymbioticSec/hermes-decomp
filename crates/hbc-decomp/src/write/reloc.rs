// Relocation helpers after size-changing edits.
//
// Most reloc logic currently lives in `patch::patch_function_bytes` (surgical
// raw image edits). This module holds shared plan types for future rebuilds.

use crate::error::{Error, Result};
use crate::file::BytecodeFile;

#[derive(Debug, Clone, Default)]
pub struct RelocPlan {
    pub code_delta: i64,
    pub string_storage_delta: i64,
    pub resized_functions: Vec<u32>,
}

impl RelocPlan {
    pub fn identity() -> Self {
        Self::default()
    }
}

// Apply a reloc plan to structured headers (not the raw image).
pub fn apply_reloc(_file: &mut BytecodeFile, plan: &RelocPlan) -> Result<()> {
    if plan.code_delta == 0 && plan.string_storage_delta == 0 {
        return Ok(());
    }
    Err(Error::Write(
        "apply_reloc on structured headers: use patch_function_bytes / finalize_raw_image instead"
            .into(),
    ))
}
