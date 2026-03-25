use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hermes-dec")]
#[command(about = "Hermes bytecode disassembler/decompiler", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Info {
        input: PathBuf,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    Versions,
    Tui {
        input: PathBuf,
        #[arg(long)]
        input2: Option<PathBuf>,
        #[arg(long)]
        format_version: Option<u32>,
        // Compare decompiled code for diff (slower)
        #[arg(long)]
        diff_code: bool,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    Disasm {
        input: PathBuf,
        #[arg(long)]
        function: Option<u32>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        #[arg(long)]
        show_offsets: bool,
        #[arg(long)]
        no_labels: bool,
        #[arg(long)]
        no_strings: bool,
    },
    Decompile {
        input: PathBuf,
        #[arg(long)]
        function: Option<u32>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        // Include bytecode offsets as comments
        #[arg(long)]
        show_offsets: bool,
        // Don't resolve string table indices
        #[arg(long)]
        no_strings: bool,
        // Disable constant/copy propagation
        #[arg(long)]
        no_propagate: bool,
        // Disable expression simplification
        #[arg(long)]
        no_simplify: bool,
        // Disable control flow structure recovery
        #[arg(long)]
        no_structure: bool,
        // Expand referenced functions inline (recursively decompile closures)
        #[arg(long)]
        expand: bool,
        // Maximum depth for function expansion (default: 2)
        #[arg(long, default_value = "2")]
        expand_depth: usize,
        // Resolve closure variables across functions (slower but more readable)
        #[arg(long)]
        resolve_closures: bool,
        // Output as JSON (IR)
        #[arg(long)]
        json: bool,
        // Check for unreachable functions (Dead Code)
        #[arg(long)]
        check_dead_code: bool,
        // Assembly mode: show absolute binary offsets on each line (Binary Ninja style)
        #[arg(long)]
        assembly: bool,
    },
    // Show closure mappings for a function (what each closure_X refers to)
    Closures {
        input: PathBuf,
        #[arg(long)]
        function: u32,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    // Show Metro module dependencies
    Deps {
        input: PathBuf,
        // Module ID (not function ID)
        #[arg(long)]
        module: u32,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        // Show dependency tree with this depth
        #[arg(long, default_value = "2")]
        depth: usize,
    },
    // List all Metro modules in the bundle
    Modules {
        input: PathBuf,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        // Show only first N modules
        #[arg(long)]
        limit: Option<usize>,
    },
    // Show debug info (variable names, scope descriptors, textified callees)
    Debug {
        input: PathBuf,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        // Show only scopes
        #[arg(long)]
        scopes: bool,
        // Show only callees
        #[arg(long)]
        callees: bool,
        // Show only variable names
        #[arg(long)]
        vars: bool,
    },
    // Extract all Metro modules to separate files
    Extract {
        input: PathBuf,
        // Output directory
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        #[arg(long)]
        no_strings: bool,
    },
    // Generate a Graphviz DOT file for the Control Flow Graph
    Graphviz {
        input: PathBuf,
        #[arg(long)]
        function: u32,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        // Open the generated graph immediately (requires xdot or open)
        #[arg(long)]
        open: bool,
    },
    // Find cross-references (xrefs) to a string or function
    Xref {
        input: PathBuf,
        // String or Function ID to search for
        #[arg(long)]
        query: String,
        // Type of query: string | function
        #[arg(long, value_enum, default_value = "string")]
        kind: XrefKind,
        #[arg(long)]
        format_version: Option<u32>,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
    BinDiff {
        input1: PathBuf,
        input2: PathBuf,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
        #[arg(long)]
        format_version: Option<u32>,
        // Compare decompiled code for modified functions
        #[arg(long)]
        diff_code: bool,
    },
    // Dump data from the HBC file (strings, etc.)
    Dump {
        input: PathBuf,
        // What to dump: 'strings', 'functions'
        #[arg(long, value_enum, default_value = "strings")]
        kind: DumpKind,
        #[arg(long, value_enum, default_value = "auto")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "auto")]
        function_layout: FunctionLayoutArg,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum DumpKind {
    Strings,
    Functions,
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
