use hbc_decomp::error::Result;
use hbc_decomp::opcode::BytecodeFormat;
use hbc_decomp::BytecodeFile;

pub fn run_callgraph(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function: Option<u32>,
    depth: usize,
    dot: bool,
) -> Result<()> {
    let output = hbc_decomp::render_call_graph(file, format, function, depth, dot)?;
    print!("{output}");
    Ok(())
}
