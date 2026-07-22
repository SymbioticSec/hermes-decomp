// Write path and RE MCP tools (secrets, frida, patch, inject, create).

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router, ErrorData as McpError};

use hbc_decomp::{
    create_minimal, emit_hasm_function, format_secrets_report, generate_frida_for_file,
    inject_stub, parse_hasm_with_context, patch_function_body, patch_string_by_id,
    patch_string_replace, scan_secrets, CreateOptions, FridaHookOptions, InjectStubKind,
    PatchOptions,
};

use super::params::*;
use super::HermesService;

#[tool_router(router = write_router, vis = "pub(crate)")]
impl HermesService {

    #[tool(
        description = "Scan the string table for likely secrets (AWS keys, JWTs, tokens, URLs, private keys)."
    )]
    fn secrets(
        &self,
        Parameters(params): Parameters<SecretsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let hits = scan_secrets(&loaded.file, &[]);
            let report = format_secrets_report(&hits, !params.show_full);
            Ok(CallToolResult::success(vec![ContentBlock::text(report)]))
        })
    }

    #[tool(
        description = "Emit HASM (our Hermes assembly dialect) for one function. It round trips with the assemble and patch function path."
    )]
    fn emit_hasm(
        &self,
        Parameters(params): Parameters<FunctionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let text = emit_hasm_function(&loaded.file, &loaded.format, params.function_id)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        })
    }

    #[tool(
        description = "Patch a string table entry and write a new .hbc. A same length value patches in place. A different length rebuilds the string table. Works on legacy and modern (v97 plus) files, including identifiers and UTF-16 strings. Provide id or old_value."
    )]
    fn patch_string(
        &self,
        Parameters(params): Parameters<PatchStringParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let mut file = loaded.file.clone();
            let opts = PatchOptions::default();
            let out = if let Some(id) = params.id {
                patch_string_by_id(&mut file, &loaded.format, id, &params.new_value, &opts)
            } else if let Some(old) = &params.old_value {
                patch_string_replace(&mut file, &loaded.format, old, &params.new_value, &opts)
            } else {
                return Err(McpError::invalid_params("provide id or old_value", None));
            }
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            std::fs::write(&params.output_path, &out)
                .map_err(|e| McpError::internal_error(format!("write: {e}"), None))?;
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Patched string → {} ({} bytes)",
                params.output_path,
                out.len()
            ))]))
        })
    }

    #[tool(
        description = "Inject a stub at a function entry and write a new .hbc. kind='nop' is a runtime no op, kind='log' prints the function name on entry."
    )]
    fn inject_stub(
        &self,
        Parameters(params): Parameters<InjectStubParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let kind = match params.kind.as_str() {
                "nop" => InjectStubKind::NopPad,
                "log" => InjectStubKind::LogEntry,
                other => {
                    return Err(McpError::invalid_params(
                        format!("unknown stub kind '{other}' (use 'nop' or 'log')"),
                        None,
                    ))
                }
            };
            let mut file = loaded.file.clone();
            let out = inject_stub(
                &mut file,
                &loaded.format,
                params.function_id,
                kind,
                &PatchOptions::default(),
            )
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            std::fs::write(&params.output_path, &out)
                .map_err(|e| McpError::internal_error(format!("write: {e}"), None))?;
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Injected {} stub into function {} → {}",
                params.kind, params.function_id, params.output_path
            ))]))
        })
    }

    #[tool(
        description = "Assemble a function body from HASM text and write a new .hbc. HASM is our disasm dialect (see emit_hasm). A same size body patches in place. Both legacy and modern (v97 plus) files also support growing or shrinking the body."
    )]
    fn patch_function(
        &self,
        Parameters(params): Parameters<PatchFunctionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let mut file = loaded.file.clone();
            let insns = parse_hasm_with_context(&params.hasm, &loaded.format, &file)
                .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            let out = patch_function_body(
                &mut file,
                &loaded.format,
                params.function_id,
                &insns,
                &PatchOptions::default(),
            )
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            std::fs::write(&params.output_path, &out)
                .map_err(|e| McpError::internal_error(format!("write: {e}"), None))?;
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Assembled function {} → {} ({} bytes)",
                params.function_id,
                params.output_path,
                out.len()
            ))]))
        })
    }

    #[tool(
        description = "Create a minimal valid .hbc from scratch and write it. Legacy layout for version 96 and lower, modern layout for version 97 and newer."
    )]
    fn create_hbc(
        &self,
        Parameters(params): Parameters<CreateHbcParams>,
    ) -> Result<CallToolResult, McpError> {
        let out = create_minimal(&CreateOptions {
            version: params.version,
            global_body: Vec::new(),
            strings: Vec::new(),
        })
        .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        std::fs::write(&params.output_path, &out)
            .map_err(|e| McpError::internal_error(format!("write: {e}"), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Created minimal HBC v{} → {} ({} bytes)",
            params.version,
            params.output_path,
            out.len()
        ))]))
    }

    #[tool(
        description = "Generate Frida hooks for a Metro module's exports. Writes before.js / after.js / agent.js / run.sh to output_dir."
    )]
    fn frida_hooks(
        &self,
        Parameters(params): Parameters<FridaHooksParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let mut opts = FridaHookOptions {
                module_id: params.module_id,
                ..Default::default()
            };
            if let Some(e) = &params.exports {
                opts.exports = e
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            let bundle = generate_frida_for_file(&loaded.file, &loaded.format, opts)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            let dir = std::path::Path::new(&params.output_dir);
            std::fs::create_dir_all(dir)
                .map_err(|e| McpError::internal_error(format!("mkdir: {e}"), None))?;
            for (name, body) in [
                ("before.js", &bundle.before_js),
                ("after.js", &bundle.after_js),
                ("agent.js", &bundle.agent_js),
                ("run.sh", &bundle.run_sh),
            ] {
                std::fs::write(dir.join(name), body)
                    .map_err(|e| McpError::internal_error(format!("write {name}: {e}"), None))?;
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Wrote Frida hooks for module {} ({} exports: {}) → {}",
                bundle.module_id,
                bundle.exports.len(),
                bundle.exports.join(", "),
                params.output_dir
            ))]))
        })
    }
}
