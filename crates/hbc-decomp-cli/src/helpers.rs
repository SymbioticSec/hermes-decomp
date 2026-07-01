use crate::cli_args::{FunctionLayoutArg, LayoutArg};
use hbc_decomp::{BytecodeFile, BytecodeFormat, FunctionHeaderLayout, HeaderLayout};
use std::fs;
use std::path::PathBuf;

pub fn load_file(
    input: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
) -> Result<BytecodeFile, Box<dyn std::error::Error>> {
    let (file, _) = load_file_with_bytes(input, layout, function_layout)?;
    Ok(file)
}

pub fn load_file_with_bytes(
    input: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
) -> Result<(BytecodeFile, Vec<u8>), Box<dyn std::error::Error>> {
    let bytes = fs::read(input)?;
    let file = match layout {
        LayoutArg::Auto => BytecodeFile::parse_auto(&bytes)?,
        LayoutArg::Legacy => {
            let function_layout = resolve_function_layout(layout, function_layout);
            BytecodeFile::parse_with_layout(&bytes, HeaderLayout::Legacy, function_layout)?
        }
        LayoutArg::Modern => {
            let function_layout = resolve_function_layout(layout, function_layout);
            BytecodeFile::parse_with_layout(&bytes, HeaderLayout::Modern, function_layout)?
        }
    };
    Ok((file, bytes))
}

pub fn resolve_function_layout(
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
) -> FunctionHeaderLayout {
    match function_layout {
        FunctionLayoutArg::Legacy16 => FunctionHeaderLayout::Legacy16,
        FunctionLayoutArg::Modern12 => FunctionHeaderLayout::Modern12,
        FunctionLayoutArg::Auto => match layout {
            LayoutArg::Legacy => FunctionHeaderLayout::Legacy16,
            LayoutArg::Modern => FunctionHeaderLayout::Modern12,
            LayoutArg::Auto => FunctionHeaderLayout::Legacy16,
        },
    }
}

pub fn load_format(
    file: &BytecodeFile,
    format_version: Option<u32>,
) -> Result<BytecodeFormat, Box<dyn std::error::Error>> {
    let version = format_version.unwrap_or(file.header.version);
    let (format, used_version) = BytecodeFormat::for_version_or_latest(version)?;
    if used_version != version {
        eprintln!(
            "warning: using opcode format version {used_version} for bytecode version {version}"
        );
    }
    Ok(format)
}

pub fn write_output(
    output: Option<PathBuf>,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = output {
        fs::write(path, content)?;
    } else {
        print!("{content}");
    }
    Ok(())
}

// Parse a `--modules` spec like "100-150,200,5-9" into inclusive id ranges.
// A bare id `N` becomes `(N, N)`. Malformed entries are skipped.
pub fn parse_id_ranges(spec: Option<&str>) -> Vec<(u32, u32)> {
    let spec = match spec {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut ranges = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<u32>(), hi.trim().parse::<u32>()) {
                ranges.push((lo.min(hi), lo.max(hi)));
            }
        } else if let Ok(n) = part.parse::<u32>() {
            ranges.push((n, n));
        }
    }
    ranges
}

// Parse a comma-separated glob list (e.g. "react*,lodash*") into trimmed,
// non-empty patterns.
pub fn parse_globs(spec: Option<&str>) -> Vec<String> {
    spec.map(|s| {
        s.split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect()
    })
    .unwrap_or_default()
}
