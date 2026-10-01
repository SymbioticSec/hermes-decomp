use hbc_decomp::{BytecodeFile, BytecodeFormat};
use std::error::Error;
use std::path::PathBuf;

// Emit the control-flow graph of one function as Graphviz DOT.
pub fn run_graphviz(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function: u32,
    output: Option<PathBuf>,
    open: bool,
) -> Result<(), Box<dyn Error>> {
    let builder_options = hbc_decomp::IRBuilderOptions {
        resolve_strings: true,
        include_offsets: true,
        absolute_offsets: false,
    };
    let mut builder = hbc_decomp::IRBuilder::new(file, format, builder_options);
    let mut cfg = builder.build_function(function)?;

    hbc_decomp::propagate(&mut cfg, &hbc_decomp::PropagationConfig::default());

    let name = file
        .string_at(file.function_headers[function as usize].function_name())
        .map(|e| e.value.as_str())
        .unwrap_or("");
    let label = if name.is_empty() {
        format!("f{function}")
    } else {
        name.to_string()
    };

    let dot_content = hbc_decomp::ir::generate_dot(&cfg, &label);

    if let Some(path) = output {
        std::fs::write(&path, &dot_content)?;
        if open {
            std::process::Command::new("open").arg(&path).status()?;
        }
    } else {
        println!("{dot_content}");
    }
    Ok(())
}
