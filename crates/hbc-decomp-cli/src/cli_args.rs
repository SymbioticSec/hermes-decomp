use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hermes-dec")]
#[command(about = "Hermes bytecode disassembler/decompiler (HBC versions 40-99)", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print bytecode header info (version, function/string counts, sections).
    Info {
        /// Path to the .hbc file or React Native .bundle.
        input: PathBuf,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// List all supported HBC opcode-table versions (40-99).
    Versions,
    /// Launch the interactive terminal UI (browse, search, decompile, diff).
    Tui {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Second bundle to diff against (enables diff mode).
        #[arg(long)]
        input2: Option<PathBuf>,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// In diff mode, compare decompiled code per function (slower).
        #[arg(long)]
        diff_code: bool,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Disassemble functions to Hermes assembly.
    Disasm {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Restrict to a single function by its numeric ID.
        #[arg(long)]
        function: Option<u32>,
        /// Write output to this file instead of stdout.
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Annotate each instruction with its bytecode offset.
        #[arg(long)]
        show_offsets: bool,
        /// Don't emit jump labels (raw offsets only).
        #[arg(long)]
        no_labels: bool,
        /// Don't resolve string-table indices to literals.
        #[arg(long)]
        no_strings: bool,
        /// Print a per-function metadata banner (params, frame, regs, flags, handlers).
        #[arg(long)]
        info: bool,
    },
    /// Decompile bytecode to readable JavaScript (ESM/Metro-aware).
    Decompile {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Restrict to a single function by its numeric ID.
        #[arg(long)]
        function: Option<u32>,
        /// Write output to this file instead of stdout.
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Include bytecode offsets as comments.
        #[arg(long)]
        show_offsets: bool,
        /// Don't resolve string-table indices to literals.
        #[arg(long)]
        no_strings: bool,
        /// Disable constant/copy propagation.
        #[arg(long)]
        no_propagate: bool,
        /// Disable expression simplification.
        #[arg(long)]
        no_simplify: bool,
        /// Disable control-flow structure recovery (emit flat goto form).
        #[arg(long)]
        no_structure: bool,
        /// Expand referenced functions inline (recursively decompile closures).
        #[arg(long)]
        expand: bool,
        /// Maximum depth for function expansion.
        #[arg(long, default_value = "2")]
        expand_depth: usize,
        /// Resolve closure variables across functions (slower, more readable).
        #[arg(long)]
        resolve_closures: bool,
        /// Output the IR as JSON instead of JavaScript.
        #[arg(long)]
        json: bool,
        /// Report unreachable (dead-code) functions.
        #[arg(long)]
        check_dead_code: bool,
        /// Assembly mode: show absolute binary offsets on each line.
        #[arg(long)]
        assembly: bool,
        /// Emit only these Metro module IDs (ranges/list, e.g. "100-150,200,5").
        #[arg(long)]
        modules: Option<String>,
        /// Emit only modules whose name matches a glob (comma-separated, e.g. "Login*,Auth*").
        #[arg(long)]
        module_name: Option<String>,
        /// Exclude modules whose name matches a glob (comma-separated, e.g. "react*,lodash*").
        #[arg(long)]
        exclude_module_name: Option<String>,
        /// Emit a module and its dependency subtree (use with --module-depth).
        #[arg(long)]
        from_module: Option<u32>,
        /// Max dependency depth for --from-module.
        #[arg(long, default_value = "3")]
        module_depth: usize,
        /// Disable the on-disk analysis cache (`<input>.hdcache`); always re-analyze.
        #[arg(long)]
        no_cache: bool,
    },
    /// Show closure mappings for a function (what each closure_X refers to).
    Closures {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Function ID to inspect.
        #[arg(long)]
        function: u32,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Show a Metro module's dependency tree.
    Deps {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Metro module ID (not the function ID).
        #[arg(long)]
        module: u32,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Dependency-tree depth to display.
        #[arg(long, default_value = "2")]
        depth: usize,
    },
    /// List all Metro modules in the bundle.
    Modules {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Show only the first N modules.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show embedded debug info (variable names, scope descriptors, callees).
    Debug {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Show only scope descriptors.
        #[arg(long)]
        scopes: bool,
        /// Show only textified callees.
        #[arg(long)]
        callees: bool,
        /// Show only variable names.
        #[arg(long)]
        vars: bool,
    },
    /// Extract each Metro module to its own file.
    Extract {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Output directory for the extracted modules.
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Don't resolve string-table indices to literals.
        #[arg(long)]
        no_strings: bool,
    },
    /// Emit a Graphviz DOT control-flow graph for a function.
    Graphviz {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Function ID to graph.
        #[arg(long)]
        function: u32,
        /// Write the DOT to this file instead of stdout.
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Open the generated graph immediately (requires xdot or `open`).
        #[arg(long)]
        open: bool,
    },
    /// Find cross-references (xrefs) to a string or function.
    Xref {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// String literal or function ID to search for.
        #[arg(long)]
        query: String,
        /// What `query` refers to: string | function.
        #[arg(long, value_enum, default_value = "string")]
        kind: XrefKind,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Diff two bundles (added/removed/modified functions).
    BinDiff {
        /// First (base) bundle.
        input1: PathBuf,
        /// Second (new) bundle.
        input2: PathBuf,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// Compare decompiled code for modified functions (slower).
        #[arg(long)]
        diff_code: bool,
    },
    /// Dump raw HBC tables (strings, functions).
    Dump {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// What to dump: strings | functions | cjs-modules | regexp | obj-shapes | function-sources | string-kinds | sections | big-int | array-buffer.
        #[arg(long, value_enum, default_value = "strings")]
        kind: DumpKind,
        /// Emit the selected table as JSON.
        #[arg(long)]
        json: bool,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Print the bundle call graph (caller → callee edges).
    Callgraph {
        /// Path to the .hbc file or .bundle.
        input: PathBuf,
        /// Restrict to the subgraph reachable from this function ID.
        #[arg(long)]
        function: Option<u32>,
        /// Emit Graphviz DOT instead of a text edge listing.
        #[arg(long)]
        dot: bool,
        /// Max hops from --function.
        #[arg(long, default_value = "3")]
        depth: usize,
        /// Override the detected HBC bytecode version.
        #[arg(long)]
        format_version: Option<u32>,
        /// File header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        /// Per-function header layout (auto-detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Check for and install updates from GitHub releases.
    Update {
        /// Just print the latest version + release notes, do not download.
        #[arg(long)]
        check: bool,
        /// Install the update in place (replaces the current binary).
        #[arg(long)]
        install: bool,
        /// Update to a specific version (e.g. "v0.1.7" or "0.1.7").
        #[arg(long)]
        version: Option<String>,
    },
    /// Scan string table for likely secrets / credentials.
    Secrets {
        input: PathBuf,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        /// Emit JSON instead of a text report.
        #[arg(long)]
        json: bool,
        /// Show full secret values (default redacts middle).
        #[arg(long)]
        show_full: bool,
    },
    /// Generate Frida hooks for a Metro module's exports.
    FridaHooks {
        input: PathBuf,
        /// Metro module id to hook.
        #[arg(long)]
        module: u32,
        /// Comma-separated export names (default: all known exports).
        #[arg(long)]
        export: Option<String>,
        /// Output directory for before.js / after.js / agent.js / run.sh.
        #[arg(short = 'o', long, default_value = "./frida_hooks_out")]
        output: PathBuf,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Emit HASM (disasm) for one function to a file.
    EmitHasm {
        input: PathBuf,
        #[arg(long)]
        function: u32,
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Assemble HASM text into a function body and write a new .hbc.
    Asm {
        /// Base .hbc / bundle to patch.
        input: PathBuf,
        /// HASM text file (our disasm dialect).
        hasm: PathBuf,
        #[arg(long)]
        function: u32,
        #[arg(short = 'o', long)]
        output: PathBuf,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Patch a string table entry (same-length only for now).
    PatchString {
        input: PathBuf,
        #[arg(short = 'o', long)]
        output: PathBuf,
        #[arg(long)]
        id: Option<u32>,
        #[arg(long)]
        old: Option<String>,
        #[arg(long)]
        new: String,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Patch a function body from a HASM file (alias of `asm`).
    PatchFunction {
        input: PathBuf,
        #[arg(short = 'o', long)]
        output: PathBuf,
        #[arg(long)]
        function: u32,
        #[arg(long)]
        hasm: PathBuf,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Inject a small bytecode stub into a function.
    InjectStub {
        input: PathBuf,
        #[arg(short = 'o', long)]
        output: PathBuf,
        #[arg(long)]
        function: u32,
        /// Stub kind: `nop` or `log`.
        #[arg(long, default_value = "log")]
        kind: String,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    /// Create a minimal valid .hbc from scratch. Legacy headers for v96 and lower, modern headers for v97 and newer.
    Create {
        #[arg(long, default_value = "96")]
        version: u32,
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// Optional strings to include (default: global).
        #[arg(long)]
        string: Vec<String>,
    },
    /// Verify HASM encode round-trip for one function (disasm→parse→encode).
    AsmCheck {
        input: PathBuf,
        #[arg(long, default_value = "0")]
        function: u32,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum DumpKind {
    Strings,
    Functions,
    CjsModules,
    Regexp,
    ObjShapes,
    FunctionSources,
    StringKinds,
    Sections,
    BigInt,
    ArrayBuffer,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum XrefKind {
    String,
    Function,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum LayoutArg {
    Auto,
    Legacy,
    Modern,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum FunctionLayoutArg {
    Auto,
    Legacy16,
    Modern12,
}
