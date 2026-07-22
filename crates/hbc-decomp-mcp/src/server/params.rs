// Tool parameter types (JSON schema derived) for the MCP server.

use rmcp::schemars;
use serde::Deserialize;

// --- Parameter types ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoadFileParams {
    #[schemars(description = "Absolute path to the .hbc file")]
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FunctionIdParams {
    #[schemars(description = "Function ID (0-based index)")]
    pub function_id: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DecompileFunctionParams {
    #[schemars(description = "Function ID (0-based index)")]
    pub function_id: u32,
    #[schemars(description = "Include bytecode offsets as comments")]
    #[serde(default)]
    pub show_offsets: bool,
    #[schemars(description = "Assembly mode: emit absolute file offsets (Binary Ninja style)")]
    #[serde(default)]
    pub assembly: bool,
    #[schemars(description = "Apply constant/copy propagation (default: true)")]
    #[serde(default = "default_true")]
    pub propagate: bool,
    #[schemars(description = "Apply expression simplification (default: true)")]
    #[serde(default = "default_true")]
    pub simplify: bool,
    #[schemars(description = "Recover control flow structures: if/while/for (default: true)")]
    #[serde(default = "default_true")]
    pub recover_structures: bool,
    #[schemars(description = "Use closure context for cross function variable resolution")]
    #[serde(default)]
    pub resolve_closures: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct XrefParams {
    #[schemars(description = "String to search for in the bytecode")]
    pub query: String,
    #[schemars(description = "Type of query: 'string' or 'function' (default: 'string')")]
    #[serde(default = "default_string")]
    pub kind: String,
}

fn default_string() -> String {
    "string".to_string()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ModuleDepsParams {
    #[schemars(description = "Metro module ID")]
    pub module_id: u32,
    #[schemars(description = "Dependency tree depth (default: 2)")]
    #[serde(default = "default_depth")]
    pub depth: usize,
}

fn default_depth() -> usize {
    2
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListModulesParams {
    #[schemars(description = "Maximum number of modules to return")]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DumpParams {
    #[schemars(description = "What to dump: 'strings', 'functions', 'identifiers', or 'all'")]
    #[serde(default = "default_strings")]
    pub kind: String,
}

fn default_strings() -> String {
    "strings".to_string()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DisasmParams {
    #[schemars(description = "Function ID (0-based index)")]
    pub function_id: u32,
    #[schemars(description = "Show bytecode offsets")]
    #[serde(default)]
    pub show_offsets: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SecretsParams {
    #[schemars(description = "Show full secret values instead of redacting the middle (default: false)")]
    #[serde(default)]
    pub show_full: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PatchStringParams {
    #[schemars(description = "String table id to patch (use this or old_value)")]
    pub id: Option<u32>,
    #[schemars(description = "Existing string value to replace (use this or id)")]
    pub old_value: Option<String>,
    #[schemars(description = "New string value. A same length value patches in place. A different length rebuilds the string table on legacy and modern files")]
    pub new_value: String,
    #[schemars(description = "Path to write the patched .hbc")]
    pub output_path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InjectStubParams {
    #[schemars(description = "Function id to inject into")]
    pub function_id: u32,
    #[schemars(description = "Stub kind. 'nop' is a runtime no op, 'log' prints the function name on entry")]
    pub kind: String,
    #[schemars(description = "Path to write the patched .hbc")]
    pub output_path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PatchFunctionParams {
    #[schemars(description = "Function id whose body to replace")]
    pub function_id: u32,
    #[schemars(description = "HASM assembly text (our disasm dialect, as produced by emit_hasm)")]
    pub hasm: String,
    #[schemars(description = "Path to write the patched .hbc")]
    pub output_path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateHbcParams {
    #[schemars(description = "HBC bytecode version (legacy layout, <= 96)")]
    pub version: u32,
    #[schemars(description = "Path to write the new minimal .hbc")]
    pub output_path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FridaHooksParams {
    #[schemars(description = "Metro module id whose exports to hook")]
    pub module_id: u32,
    #[schemars(description = "Comma separated export names to hook. Default is all known exports")]
    pub exports: Option<String>,
    #[schemars(description = "Output directory for before.js / after.js / agent.js / run.sh")]
    pub output_dir: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DumpTableParams {
    #[schemars(
        description = "Table to dump: cjs-modules, regexp, obj-shapes, function-sources, string-kinds, sections, big-int, array-buffer"
    )]
    pub kind: String,
    #[schemars(description = "Return the table as JSON instead of text (default: false)")]
    #[serde(default)]
    pub json: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CallgraphParams {
    #[schemars(description = "Restrict to the subgraph reachable from this function ID")]
    pub function_id: Option<u32>,
    #[schemars(description = "Max hops from function_id (default: 3)")]
    #[serde(default = "default_depth")]
    pub depth: usize,
    #[schemars(description = "Emit Graphviz DOT instead of a text edge listing (default: false)")]
    #[serde(default)]
    pub dot: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DecompileModuleParams {
    #[schemars(description = "Metro module ID")]
    pub module_id: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ModuleExportsParams {
    #[schemars(description = "Metro module ID")]
    pub module_id: u32,
}

