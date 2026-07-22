// MCP server for the Hermes decompiler. Split into: `params` (tool parameter
// types), `tools_analyze` (read and analysis tools), `tools_write` (write path
// and RE tools). Each tool group builds its own router; `new` merges them.

mod params;
mod tools_analyze;
mod tools_write;

use rmcp::ErrorData as McpError;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{ServerCapabilities, ServerInfo},
    tool_handler, ServerHandler,
};
use std::sync::Mutex;

use hbc_decomp::opcode::BytecodeFormat;
use hbc_decomp::{BytecodeFile, DecompileOptionsV2, PipelineContext};

pub(crate) struct LoadedFile {
    pub(crate) file: BytecodeFile,
    pub(crate) format: BytecodeFormat,
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) pipeline_ctx: Option<PipelineContext>,
}

pub struct HermesService {
    loaded: Mutex<Option<LoadedFile>>,
    tool_router: ToolRouter<Self>,
}

impl HermesService {
    pub fn new() -> Self {
        Self {
            loaded: Mutex::new(None),
            tool_router: Self::analyze_router() + Self::write_router(),
        }
    }

    pub(crate) fn with_file<F, T>(&self, f: F) -> Result<T, McpError>
    where
        F: FnOnce(&LoadedFile) -> Result<T, McpError>,
    {
        let guard = self
            .loaded
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock: {e}"), None))?;
        let loaded = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No file loaded. Use load_file first.", None)
        })?;
        f(loaded)
    }

    pub(crate) fn with_file_mut<F, T>(&self, f: F) -> Result<T, McpError>
    where
        F: FnOnce(&mut LoadedFile) -> Result<T, McpError>,
    {
        let mut guard = self
            .loaded
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock: {e}"), None))?;
        let loaded = guard.as_mut().ok_or_else(|| {
            McpError::invalid_params("No file loaded. Use load_file first.", None)
        })?;
        f(loaded)
    }
}

impl LoadedFile {
    fn ensure_pipeline(&mut self) -> Result<(), McpError> {
        if self.pipeline_ctx.is_none() {
            // Reuse an on-disk analysis cache (`<file>.hdcache`) keyed by the
            // bytecode, so repeated sessions on the same file don't re-analyze.
            let cache_path = hbc_decomp::default_cache_path(std::path::Path::new(&self.path));
            let ctx = PipelineContext::build_cached(
                &self.file,
                &self.format,
                &DecompileOptionsV2::optimized(),
                &self.bytes,
                &cache_path,
            )
            .map_err(|e| McpError::internal_error(format!("Pipeline build error: {e}"), None))?;
            self.pipeline_ctx = Some(ctx);
        }
        Ok(())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HermesService {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (InitializeResult) is #[non_exhaustive] in rmcp 2, so it
        // cannot be built with a struct literal; set fields on a default value.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Hermes bytecode decompiler for React Native apps (HBC 40 to 99). Load a .hbc file with load_file, then use the decompile, disassemble, xref and module tools to analyze. Use decompile_function for quick single function output, or decompile_function_full and decompile_module for full quality analysis with IPA naming and ESM imports and exports. For structural inspection use dump_table (kinds cjs-modules, regexp, obj-shapes, function-sources, string-kinds, sections, big-int, array-buffer), callgraph (caller to callee edges, optional DOT), and function_info (per function metadata banner).".into()
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}
