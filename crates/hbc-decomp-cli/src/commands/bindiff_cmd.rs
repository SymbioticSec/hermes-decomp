use crate::cli_args::FormatArgs;
use crate::tui::diff::{compare_functions, DiffMode, DiffStatus};
use hbc_decomp::{decompile_function_v2, BytecodeFile, BytecodeFormat, DecompileOptionsV2};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Debug, Clone)]
pub struct ModifiedFunction {
    pub name: String,
    pub base_id: u32,
    pub new_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_code: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct DiffReport {
    pub base: String,
    pub new: String,
    pub identical: usize,
    pub modified: Vec<ModifiedFunction>,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

impl DiffReport {
    // The report with every list in name order, so two runs over the same
    // bundles produce the same document.
    pub fn sorted(mut self) -> Self {
        self.modified.sort_by(|a, b| a.name.cmp(&b.name));
        self.removed.sort();
        self.added.sort();
        self
    }
}

pub fn run_bindiff(
    path1: &PathBuf,
    path2: &PathBuf,
    args: &FormatArgs,
    diff_code: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !json {
        println!("Loading {}...", path1.display());
    }
    let file1 = crate::helpers::load_file(path1, args)?;
    let format1 = crate::helpers::load_format(&file1, args.format_version)?;

    if !json {
        println!("Loading {}...", path2.display());
    }
    let file2 = crate::helpers::load_file(path2, args)?;
    let format2 = crate::helpers::load_format(&file2, args.format_version)?;

    if !json {
        println!("Comparing functions...");
    }

    let report = compare_bundles(
        &file1,
        &format1,
        &file2,
        &format2,
        diff_code,
        path1.display().to_string(),
        path2.display().to_string(),
    );

    if json {
        println!("{}", serde_json::to_string_pretty(&report.sorted())?);
        return Ok(());
    }
    print_report(&report, diff_code);
    Ok(())
}

fn compare_bundles(
    file1: &BytecodeFile,
    format1: &BytecodeFormat,
    file2: &BytecodeFile,
    format2: &BytecodeFormat,
    diff_code: bool,
    base: String,
    new: String,
) -> DiffReport {
    // Name -> FunctionID
    let map1 = build_function_map(file1);
    let map2 = build_function_map(file2);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    let mut identical = 0;

    // Comparaison
    for (name, id1) in &map1 {
        if let Some(id2) = map2.get(name) {
            let mode = if diff_code {
                DiffMode::Code
            } else {
                DiffMode::Assembly
            };
            let status = compare_functions(file1, format1, *id1, file2, format2, *id2, mode);

            if status != DiffStatus::Identical {
                let (base_code, new_code) = if diff_code {
                    (
                        Some(decompile_or_error(file1, format1, *id1)),
                        Some(decompile_or_error(file2, format2, *id2)),
                    )
                } else {
                    (None, None)
                };
                modified.push(ModifiedFunction {
                    name: name.clone(),
                    base_id: *id1,
                    new_id: *id2,
                    base_code,
                    new_code,
                });
            } else {
                identical += 1;
            }
        } else {
            removed.push(name.clone());
        }
    }

    for name in map2.keys() {
        if !map1.contains_key(name) {
            added.push(name.clone());
        }
    }

    DiffReport {
        base,
        new,
        identical,
        modified,
        removed,
        added,
    }
}

fn decompile_or_error(file: &BytecodeFile, format: &BytecodeFormat, id: u32) -> String {
    decompile_function_v2(file, format, id, &DecompileOptionsV2::default())
        .unwrap_or_else(|e| format!("Error: {e}"))
}

fn print_report(report: &DiffReport, diff_code: bool) {
    println!("\n--- BinDiff Result ---");
    println!("Identical: {}", report.identical);
    println!("Modified:  {}", report.modified.len());
    println!("Removed:   {}", report.removed.len());
    println!("Added:     {}", report.added.len());

    if !report.modified.is_empty() {
        println!("\nModified Functions:");
        for m in &report.modified {
            println!("  - {} (ID: {} -> {})", m.name, m.base_id, m.new_id);

            if diff_code {
                println!("\n    --- LEFT (v1) ---");
                for line in m.base_code.as_deref().unwrap_or_default().lines() {
                    println!("    {line}");
                }

                println!("\n    --- RIGHT (v2) ---");
                for line in m.new_code.as_deref().unwrap_or_default().lines() {
                    println!("    {line}");
                }
                println!("\n    ------------------");
            }
        }
    }
}

fn build_function_map(file: &BytecodeFile) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    for (i, header) in file.function_headers.iter().enumerate() {
        let name = file
            .string_at(header.function_name())
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("f{i}"));
        map.insert(name, i as u32);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn diff_report_json_is_sorted_and_omits_absent_code() {
        let report = DiffReport {
            base: "a.hbc".into(),
            new: "b.hbc".into(),
            identical: 3,
            modified: vec![
                ModifiedFunction {
                    name: "zeta".into(),
                    base_id: 5,
                    new_id: 6,
                    base_code: None,
                    new_code: None,
                },
                ModifiedFunction {
                    name: "alpha".into(),
                    base_id: 1,
                    new_id: 2,
                    base_code: Some("function alpha() {}".into()),
                    new_code: Some("function alpha(x) {}".into()),
                },
            ],
            removed: vec!["old2".into(), "old1".into()],
            added: vec!["new2".into(), "new1".into()],
        }
        .sorted();
        let back: Value = serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(back["identical"], 3);
        assert_eq!(back["modified"][0]["name"], "alpha");
        assert_eq!(back["modified"][0]["new_code"], "function alpha(x) {}");
        assert!(back["modified"][1].get("base_code").is_none());
        assert_eq!(back["removed"], serde_json::json!(["old1", "old2"]));
        assert_eq!(back["added"], serde_json::json!(["new1", "new2"]));
    }
}
