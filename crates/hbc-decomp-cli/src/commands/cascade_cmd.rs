// The offline half of the proposal chain. Extraction writes out what a reader has
// to judge, verification reads a proposal back and says what the bytecode confirms.
// Neither talks to a network, and neither is on the decompile path.

use hbc_decomp::cascade::{self, Artifact, BundleFingerprint};
use hbc_decomp::{BytecodeFile, BytecodeFormat, DecompileOptionsV2, PipelineContext};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

fn pipeline(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    bytes: &[u8],
    cache_path: &Path,
) -> Result<PipelineContext, Box<dyn Error>> {
    Ok(PipelineContext::build_cached(
        file,
        format,
        &DecompileOptionsV2::optimized(),
        bytes,
        cache_path,
    )?)
}

// Names the bytecode already established, which a proposal must not overwrite.
fn recovered_names(ctx: &PipelineContext) -> BTreeMap<u32, String> {
    ctx.closure_ctx
        .as_ref()
        .map(|c| c.function_names.clone())
        .unwrap_or_default()
}

pub fn run_extract(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    bytes: &[u8],
    cache_path: &Path,
    output: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let ctx = pipeline(file, format, bytes, cache_path)?;
    let found = cascade::candidates::extract(&ctx.all_ir, &recovered_names(&ctx));
    let document = serde_json::json!({
        "version": cascade::ARTIFACT_VERSION,
        "bundle": BundleFingerprint::of(file),
        "candidates": found,
    });
    let text = serde_json::to_string_pretty(&document)?;
    match output {
        Some(path) => {
            std::fs::write(path, text)?;
            println!("{} candidates written to {}", found.len(), path.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

pub fn run_verify(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    bytes: &[u8],
    cache_path: &Path,
    artifact_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let text = std::fs::read_to_string(artifact_path)?;
    let artifact: Artifact = serde_json::from_str(&text)?;
    let ctx = pipeline(file, format, bytes, cache_path)?;
    let verified = cascade::verify::verify_against_file(&artifact, file, &ctx.all_ir);

    println!(
        "{} of {} proposals confirmed ({:.1}%)",
        verified.confirmed(),
        verified.total(),
        verified.confirmation_rate() * 100.0
    );
    if !verified.rejected.is_empty() {
        println!("\nrefused:");
        for (proposal, reason) in &verified.rejected {
            println!(
                "  fn{} as {} ({}): {reason}",
                proposal.function_id,
                proposal.name,
                proposal.role.as_str()
            );
        }
    }
    Ok(())
}
