// CLI handlers for write-path commands: asm, patch-*, inject-stub, create, secrets, frida-hooks.

use std::path::{Path, PathBuf};

use hbc_decomp::{
    create_minimal, emit_hasm_function, generate_frida_for_file, inject_stub, parse_hasm_with_context,
    patch_function_body, patch_string_by_id, patch_string_replace, scan_secrets,
    format_secrets_report, CreateOptions, FridaHookOptions, InjectStubKind, PatchOptions,
};

use crate::cli_args::{FunctionLayoutArg, LayoutArg};
use crate::helpers::{load_file, load_format};

type BoxErr = Box<dyn std::error::Error>;

// Warn when a write command targets a modern file (HBC version 97 or newer),
// where the write path is only partially supported.
fn warn_modern_write(file: &hbc_decomp::BytecodeFile) {
    let modern = matches!(
        file.header.function_header_layout,
        hbc_decomp::FunctionHeaderLayout::Modern12
    );
    if modern {
        eprintln!(
            "note: modern HBC v{} (version 97 or newer, 12 byte headers). String patches,\n  \
             function body resize and stub injection are all supported and verified on a real\n  \
             engine, including length changes, identifiers and UTF-16. Building a modern file\n  \
             from scratch with create is not supported yet and stays legacy only.\n  \
             To run modern output yourself, build the external verifier with\n  \
             scripts/build/build_hermes_v98_toolchain.sh on macOS.",
            file.header.version
        );
    }
}

pub fn run_secrets(
    input: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    json: bool,
    show_full: bool,
) -> Result<(), BoxErr> {
    let file = load_file(input, layout, function_layout)?;
    let hits = scan_secrets(&file, &[]);
    if json {
        let rows: Vec<_> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "string_id": h.string_id,
                    "category": h.category,
                    "pattern": h.pattern_name,
                    "value": if show_full { &h.value } else { &h.redacted },
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print!("{}", format_secrets_report(&hits, !show_full));
    }
    Ok(())
}

pub fn run_frida_hooks(
    input: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    format_version: Option<u32>,
    module_id: u32,
    export: Option<String>,
    out_dir: PathBuf,
) -> Result<(), BoxErr> {
    let file = load_file(input, layout, function_layout)?;
    let format = load_format(&file, format_version)?;
    let mut opts = FridaHookOptions {
        module_id,
        ..Default::default()
    };
    if let Some(e) = export {
        opts.exports = e.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    let bundle = generate_frida_for_file(&file, &format, opts)?;
    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(out_dir.join("before.js"), &bundle.before_js)?;
    std::fs::write(out_dir.join("after.js"), &bundle.after_js)?;
    std::fs::write(out_dir.join("agent.js"), &bundle.agent_js)?;
    std::fs::write(out_dir.join("run.sh"), &bundle.run_sh)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(out_dir.join("run.sh"))?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(out_dir.join("run.sh"), perms)?;
    }
    eprintln!(
        "Wrote Frida hooks for module {} ({} exports) → {}",
        bundle.module_id,
        bundle.exports.len(),
        out_dir.display()
    );
    for e in &bundle.exports {
        eprintln!("  - {e}");
    }
    Ok(())
}

pub fn run_asm(
    input: &PathBuf,
    hasm: &PathBuf,
    function: u32,
    output: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    format_version: Option<u32>,
) -> Result<(), BoxErr> {
    let mut file = load_file(input, layout, function_layout)?;
    let format = load_format(&file, format_version)?;
    warn_modern_write(&file);
    let text = std::fs::read_to_string(hasm)?;
    let insns = parse_hasm_with_context(&text, &format, &file)?;
    let out = patch_function_body(&mut file, &format, function, &insns, &PatchOptions::default())?;
    std::fs::write(output, out)?;
    eprintln!(
        "Assembled function {function} from {} → {}",
        hasm.display(),
        output.display()
    );
    Ok(())
}

pub fn run_emit_hasm(
    input: &PathBuf,
    function: u32,
    output: Option<PathBuf>,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    format_version: Option<u32>,
) -> Result<(), BoxErr> {
    let file = load_file(input, layout, function_layout)?;
    let format = load_format(&file, format_version)?;
    let text = emit_hasm_function(&file, &format, function)?;
    if let Some(path) = output {
        std::fs::write(path, text)?;
    } else {
        print!("{text}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_patch_string(
    input: &PathBuf,
    output: &PathBuf,
    id: Option<u32>,
    old: Option<String>,
    new: String,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    format_version: Option<u32>,
) -> Result<(), BoxErr> {
    let mut file = load_file(input, layout, function_layout)?;
    let format = load_format(&file, format_version)?;
    warn_modern_write(&file);
    let opts = PatchOptions::default();
    let out = if let Some(id) = id {
        patch_string_by_id(&mut file, &format, id, &new, &opts)?
    } else if let Some(old) = old {
        patch_string_replace(&mut file, &format, &old, &new, &opts)?
    } else {
        return Err("provide --id or --old".into());
    };
    std::fs::write(output, out)?;
    eprintln!("Patched string → {}", output.display());
    Ok(())
}

pub fn run_patch_function(
    input: &PathBuf,
    output: &PathBuf,
    function: u32,
    hasm: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    format_version: Option<u32>,
) -> Result<(), BoxErr> {
    run_asm(
        input,
        hasm,
        function,
        output,
        layout,
        function_layout,
        format_version,
    )
}

pub fn run_inject_stub(
    input: &PathBuf,
    output: &PathBuf,
    function: u32,
    kind: &str,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    format_version: Option<u32>,
) -> Result<(), BoxErr> {
    let mut file = load_file(input, layout, function_layout)?;
    let format = load_format(&file, format_version)?;
    warn_modern_write(&file);
    let kind = match kind {
        "nop" | "NopPad" => InjectStubKind::NopPad,
        "log" | "LogEntry" => InjectStubKind::LogEntry,
        other => return Err(format!("unknown stub kind: {other} (use nop|log)").into()),
    };
    let out = inject_stub(
        &mut file,
        &format,
        function,
        kind,
        &PatchOptions::default(),
    )?;
    std::fs::write(output, out)?;
    eprintln!("Injected stub into function {function} → {}", output.display());
    Ok(())
}

pub fn run_create(version: u32, output: &PathBuf, strings: Vec<String>) -> Result<(), BoxErr> {
    let opts = CreateOptions {
        version,
        strings: if strings.is_empty() {
            vec!["global".into()]
        } else {
            strings
        },
        ..Default::default()
    };
    let bytes = create_minimal(&opts)?;
    std::fs::write(output, bytes)?;
    eprintln!("Created minimal HBC v{version} → {}", output.display());
    Ok(())
}

pub fn run_roundtrip_check(input: &Path, function: u32) -> Result<(), BoxErr> {
    use hbc_decomp::{encode_function_body, verify_footer, BytecodeFile, BytecodeFormat};

    let bytes = std::fs::read(input)?;
    if !verify_footer(&bytes) {
        return Err("input footer SHA-1 mismatch".into());
    }
    let file = BytecodeFile::parse_auto(&bytes)?;
    let format = BytecodeFormat::for_version(file.header.version)?;
    let text = emit_hasm_function(&file, &format, function)?;
    let parsed = parse_hasm_with_context(&text, &format, &file)?;
    let original = file.decode_function_instructions(&format, function)?;
    let a = encode_function_body(&format, &original)?;
    let b = encode_function_body(&format, &parsed)?;
    if a != b {
        return Err("HASM round-trip byte mismatch".into());
    }
    eprintln!("OK: hasm round-trip function {function} on {}", input.display());
    Ok(())
}
