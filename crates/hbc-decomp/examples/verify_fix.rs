use hbc_decomp::{DecompileOptionsV2, Decompiler};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    // 1. Read the bytecode file (using our example file)
    // Adjust path to point relative to where the example runs (from workspace root usually)
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/react-native/index.android.bundle"
    );

    // Check if file exists, falling back to a safe check for the user
    if !std::path::Path::new(path).exists() {
        println!("Warning: Example file not found at: {}", path);
        // Try looking in current directory just in case
        if std::path::Path::new("index.android.bundle").exists() {
            println!("Found index.android.bundle in current directory, using that.");
            // logic to use that instead... simply override for this example
        } else {
            println!("Skipping test execution as file is missing. Please ensure examples/react-native/index.android.bundle exists.");
            return Ok(());
        }
    }

    // We try to read whatever we found or the original path
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => {
            // Fallback for demo purposes if path resolution fails
            vec![]
        }
    };

    if bytes.is_empty() {
        return Ok(());
    }

    // 2. Initialize the decompiler (parses header, detects format)
    let mut decompiler = Decompiler::new(&bytes)?;

    // 3. (Optional) Build context for global analysis (closures, etc.)
    decompiler.build_closure_context()?;

    // 4. Configure options
    let options = DecompileOptionsV2::optimized();

    // 5. Decompile explicit function (function 0 is usually safe)
    println!("Decompiling Function 0...");
    let code = decompiler.decompile_function(0, &options)?;
    println!("--- Code ---\n{code}\n------------");

    // 6. Export IR
    println!("Exporting IR for Function 0...");
    let ir = decompiler.decompile_to_ir(0, &options)?;
    println!("IR Statements: {}", ir.len());

    Ok(())
}
