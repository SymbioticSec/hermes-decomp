// MCP server for the Hermes decompiler. Split into: `params` (tool parameter
// types), `tools_analyze` (read and analysis tools), `tools_write` (write path
// and RE tools). Each tool group builds its own router; `new` merges them.

mod bounds;
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
    // Whether the cached `pipeline_ctx` was built in deep naming mode, so a request
    // that toggles deep rebuilds instead of returning the wrong-mode context.
    pub(crate) pipeline_deep: bool,
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

    // Every tool body runs through one of these two, so this is the single
    // place where the work is moved onto a large-stack thread. The mutex guard
    // stays on the calling thread (it is not Send); only a borrow of the loaded
    // file crosses into the scoped thread, which ends before the guard drops.
    pub(crate) fn with_file<F, T>(&self, f: F) -> Result<T, McpError>
    where
        F: FnOnce(&LoadedFile) -> Result<T, McpError> + Send,
        T: Send,
    {
        let guard = self
            .loaded
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock: {e}"), None))?;
        let loaded = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No file loaded. Use load_file first.", None)
        })?;
        run_scoped_with_large_stack(move || f(loaded))
    }

    pub(crate) fn with_file_mut<F, T>(&self, f: F) -> Result<T, McpError>
    where
        F: FnOnce(&mut LoadedFile) -> Result<T, McpError> + Send,
        T: Send,
    {
        let mut guard = self
            .loaded
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock: {e}"), None))?;
        let loaded = guard.as_mut().ok_or_else(|| {
            McpError::invalid_params("No file loaded. Use load_file first.", None)
        })?;
        run_scoped_with_large_stack(move || f(loaded))
    }
}

// Scoped counterpart of `hbc_decomp::run_with_large_stack` for closures that
// borrow the loaded file. Tokio worker threads have a ~2 MB stack; structure
// recovery and codegen on a real bundle overflow it on the calling thread.
fn run_scoped_with_large_stack<T, F>(f: F) -> T
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("hbc-mcp-tool".into())
            .stack_size(hbc_decomp::LARGE_STACK_SIZE)
            .spawn_scoped(scope, f)
            .expect("failed to spawn large-stack tool thread");
        match handle.join() {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

impl LoadedFile {
    fn ensure_pipeline(&mut self, deep: bool) -> Result<(), McpError> {
        if self.pipeline_ctx.is_none() || self.pipeline_deep != deep {
            // Reuse an on-disk analysis cache (`<file>.hdcache`) keyed by the
            // bytecode and options (deep is part of the key), so repeated sessions on
            // the same file and mode don't re-analyze.
            let cache_path = hbc_decomp::default_cache_path(std::path::Path::new(&self.path));
            let options = DecompileOptionsV2 {
                deep,
                ..DecompileOptionsV2::optimized()
            };
            let ctx = PipelineContext::build_cached(
                &self.file,
                &self.format,
                &options,
                &self.bytes,
                &cache_path,
            )
            .map_err(|e| McpError::internal_error(format!("Pipeline build error: {e}"), None))?;
            self.pipeline_ctx = Some(ctx);
            self.pipeline_deep = deep;
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

#[cfg(test)]
mod tests {
    use super::run_scoped_with_large_stack;

    // Recursion deep enough to blow a 2 MB stack (the frame is padded so the
    // optimizer cannot shrink it) but comfortably inside 64 MB.
    fn deep(n: u32) -> u32 {
        let pad = [n; 32];
        if n == 0 {
            return std::hint::black_box(pad)[0];
        }
        std::hint::black_box(pad);
        1 + deep(n - 1)
    }

    #[test]
    fn scoped_helper_runs_deep_recursion_and_borrows() {
        let data = [1u32, 2, 3];
        let sum = run_scoped_with_large_stack(|| data.iter().sum::<u32>() + deep(50_000));
        assert_eq!(sum, 6 + 50_000);
    }

    #[test]
    fn lib_helper_runs_deep_recursion() {
        assert_eq!(hbc_decomp::run_with_large_stack(|| deep(50_000)), 50_000);
    }

    #[test]
    fn panics_are_resumed_on_the_caller() {
        let r =
            std::panic::catch_unwind(|| run_scoped_with_large_stack(|| -> u32 { panic!("boom") }));
        assert!(r.is_err());
        let r = std::panic::catch_unwind(|| {
            hbc_decomp::run_with_large_stack(|| -> u32 { panic!("boom") })
        });
        assert!(r.is_err());
    }
}
