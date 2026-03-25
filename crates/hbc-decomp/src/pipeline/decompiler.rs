use crate::analysis::ClosureContext;
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::opcode::BytecodeFormat;

use super::{
    build_closure_context_from_file, decompile_all_v2_with_closures,
    decompile_function_v2_with_context, generate_ir, DecompileOptionsV2,
};

pub struct Decompiler {
    pub file: BytecodeFile,
    pub format: BytecodeFormat,
    pub closure_ctx: Option<ClosureContext>,
}

impl Decompiler {
    pub fn new(bytes: &[u8]) -> Result<Self> {
        let file = BytecodeFile::parse_auto(bytes)?;
        let (format, _) = BytecodeFormat::for_version_or_latest(file.header.version)?;
        Ok(Self {
            file,
            format,
            closure_ctx: None,
        })
    }

    pub fn from_parts(file: BytecodeFile, format: BytecodeFormat) -> Self {
        Self {
            file,
            format,
            closure_ctx: None,
        }
    }

    pub fn build_closure_context(&mut self) -> Result<()> {
        let ctx = build_closure_context_from_file(&self.file, &self.format)?;
        self.closure_ctx = Some(ctx);
        Ok(())
    }

    pub fn decompile_function(
        &self,
        function_id: u32,
        options: &DecompileOptionsV2,
    ) -> Result<String> {
        decompile_function_v2_with_context(
            &self.file,
            &self.format,
            function_id,
            options,
            self.closure_ctx.as_ref(),
        )
    }

    pub fn decompile_all(&self, options: &DecompileOptionsV2) -> Result<String> {
        decompile_all_v2_with_closures(
            &self.file,
            &self.format,
            options,
        )
    }

    pub fn decompile_to_ir(
        &self,
        function_id: u32,
        options: &DecompileOptionsV2,
    ) -> Result<Vec<Statement>> {
        generate_ir(
            &self.file,
            &self.format,
            function_id,
            options,
            self.closure_ctx.as_ref(),
            true,
        )
    }
}
