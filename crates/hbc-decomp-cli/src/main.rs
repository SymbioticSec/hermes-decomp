use clap::Parser;
use hbc_decomp::{DecompileOptionsV2, DisasmOptions};
use std::time::Instant;

mod cli_args;
mod commands;
mod helpers;
mod tui;

use cli_args::{Cli, Command};
use helpers::{load_file, load_format, write_output};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Command::Info {
            input,
            layout,
            function_layout,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            commands::debug_cmd::print_info(&file);
        }
        Command::Versions => {
            let versions = hbc_decomp::opcode::available_versions();
            let list = versions
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!("Available opcode versions: {list}");
        }
        Command::Tui {
            input,
            input2,
            format_version,
            layout,
            function_layout,
            diff_code,
        } => {
            tui::debug_log(&format!("[TUI] Loading primary bundle: {}", input.display()));
            let primary_load_start = Instant::now();
            let file = load_file(&input, layout, function_layout)?;
            tui::debug_log(&format!(
                "[TUI] Loaded primary bundle in {:.2?} (functions: {})",
                primary_load_start.elapsed(),
                file.header.function_count
            ));

            let primary_format_start = Instant::now();
            let format = load_format(&file, format_version)?;
            tui::debug_log(&format!(
                "[TUI] Resolved primary format in {:.2?}",
                primary_format_start.elapsed()
            ));
            let path = input.display().to_string();

            let diff_target = if let Some(path2) = input2 {
                tui::debug_log(&format!("[TUI] Loading secondary bundle: {}", path2.display()));
                let secondary_load_start = Instant::now();
                let file2 = load_file(&path2, layout, function_layout)?;
                tui::debug_log(&format!(
                    "[TUI] Loaded secondary bundle in {:.2?} (functions: {})",
                    secondary_load_start.elapsed(),
                    file2.header.function_count
                ));

                let secondary_format_start = Instant::now();
                let format2 = load_format(&file2, format_version)?;
                tui::debug_log(&format!(
                    "[TUI] Resolved secondary format in {:.2?}",
                    secondary_format_start.elapsed()
                ));
                Some((file2, format2, path2.display().to_string()))
            } else {
                None
            };

            tui::run_tui(file, format, path, diff_target, diff_code)?;
        }
        Command::Disasm {
            input,
            function,
            output,
            format_version,
            layout,
            function_layout,
            show_offsets,
            no_labels,
            no_strings,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;
            let options = DisasmOptions {
                show_offsets,
                show_labels: !no_labels,
                resolve_strings: !no_strings,
                enable_color: output.is_none(),
            };
            let content = if let Some(function_id) = function {
                hbc_decomp::disassemble_function(&file, &format, function_id, &options)?
            } else {
                hbc_decomp::disassemble_all(&file, &format, &options)?
            };
            write_output(output, &content)?;
        }
        Command::Decompile {
            input,
            function,
            output,
            format_version,
            layout,
            function_layout,
            show_offsets,
            no_strings,
            no_propagate,
            no_simplify,
            no_structure,
            expand,
            expand_depth,
            resolve_closures,
            json,
            check_dead_code,
            assembly,
        } => {
            let (file, file_bytes) = helpers::load_file_with_bytes(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;
            let options = DecompileOptionsV2 {
                resolve_strings: !no_strings,
                include_offsets: show_offsets || assembly,
                propagate: !no_propagate,
                simplify: !no_simplify,
                recover_structures: !no_structure,
                assembly_mode: assembly,
            };

            if check_dead_code {
                let analysis = hbc_decomp::analyze_module(&file, &format)?;
                println!("Dead Code Analysis:");
                println!("-------------------");
                if analysis.dead_code.is_empty() {
                    println!("No unreachable functions detected.");
                } else {
                    let mut dead: Vec<u32> = analysis.dead_code.into_iter().collect();
                    dead.sort();
                    println!("Found {} unreachable functions:", dead.len());
                    for id in dead {
                        let name = file.string_at(file.function_headers[id as usize].function_name())
                            .map(|e| e.value.as_str()).unwrap_or("");
                        println!("  Function {id} ({name})");
                    }
                }
                return Ok(());
            }

            let content = if json {
                 if let Some(function_id) = function {
                     commands::decompile_cmd::expand_json(&file, &format, function_id, &options)?
                 } else {
                     let mut results = Vec::new();
                     let decomp = hbc_decomp::Decompiler::from_parts(file.clone(), format.clone());
                     for i in 0..file.header.function_count {
                         if let Ok(ir) = decomp.decompile_to_ir(i, &options) {
                             results.push(serde_json::json!({
                                 "functionId": i,
                                 "ir": ir
                             }));
                         }
                     }
                     serde_json::to_string_pretty(&results)?
                 }
            } else if expand {
                if let Some(function_id) = function {
                    commands::decompile_cmd::decompile_with_expansion(&file, &format, function_id, &options, expand_depth)?
                } else {
                    hbc_decomp::decompile_all_v2_with_closures(&file, &format, &options)?
                }
            } else if let Some(function_id) = function {
                if resolve_closures {
                    let ctx = hbc_decomp::build_closure_context(&file, &format)?;
                    hbc_decomp::decompile_function_v2_with_context(&file, &format, function_id, &options, Some(&ctx))?
                } else {
                    hbc_decomp::decompile_function_v2(&file, &format, function_id, &options)?
                }
            } else {
                hbc_decomp::decompile_all_v2_with_closures(&file, &format, &options)?
            };

            let content = if assembly {
                let file_path = input.display().to_string();
                commands::decompile_cmd::format_assembly_output(&content, &file, &file_path, file_bytes.len())
            } else {
                content
            };
            write_output(output, &content)?;
        }
        Command::Closures {
            input,
            function,
            format_version,
            layout,
            function_layout,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;
            commands::decompile_cmd::print_closure_info(&file, &format, function)?;
        }
        Command::Deps {
            input,
            module,
            format_version,
            layout,
            function_layout,
            depth,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;
            commands::extract_cmd::print_module_deps(&file, &format, module, depth)?;
        }
        Command::Modules {
            input,
            format_version,
            layout,
            function_layout,
            limit,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;
            commands::extract_cmd::print_modules(&file, &format, limit)?;
        }
        Command::Debug {
            input,
            layout,
            function_layout,
            scopes,
            callees,
            vars,
        } => {
            let (file, bytes) = helpers::load_file_with_bytes(&input, layout, function_layout)?;
            commands::debug_cmd::print_debug_info(&file, &bytes, scopes, callees, vars)?;
        }
        Command::Extract {
            input,
            output,
            format_version,
            layout,
            function_layout,
            no_strings,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;
            commands::extract_cmd::run_extract(&file, &format, &output, !no_strings)?;
        }
        Command::Graphviz {
            input,
            function,
            output,
            format_version,
            layout,
            function_layout,
            open,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;

            let builder_options = hbc_decomp::IRBuilderOptions {
                resolve_strings: true,
                include_offsets: true,
                absolute_offsets: false,
            };
            let mut builder = hbc_decomp::IRBuilder::new(&file, &format, builder_options);
            let mut cfg = builder.build_function(function)?;

            hbc_decomp::propagate(&mut cfg, &hbc_decomp::PropagationConfig::default());

            let name = file
                .string_at(file.function_headers[function as usize].function_name())
                .map(|e| e.value.as_str())
                .unwrap_or("");
            let label = if name.is_empty() { format!("f{function}") } else { name.to_string() };

            let dot_content = hbc_decomp::ir::generate_dot(&cfg, &label);

            if let Some(path) = output {
                std::fs::write(&path, &dot_content)?;
                if open {
                    std::process::Command::new("open").arg(&path).status()?;
                }
            } else {
                println!("{dot_content}");
            }
        }
        Command::Xref {
            input,
            query,
            kind,
            format_version,
            layout,
            function_layout,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            let format = load_format(&file, format_version)?;

            let results = match kind {
                cli_args::XrefKind::String => {
                    hbc_decomp::analysis::find_string_xrefs(&file, &format, &query)
                }
                cli_args::XrefKind::Function => {
                    let fid = query.parse::<u32>().map_err(|_| "Invalid function ID")?;
                    hbc_decomp::analysis::find_function_refs(&file, &format, fid)
                }
            };

            println!("Found {} cross-references for '{}':", results.len(), query);
            for xref in results {
                let name = file
                    .string_at(file.function_headers[xref.function_id as usize].function_name())
                    .map(|e| e.value.as_str())
                    .unwrap_or("<anonymous>");

                println!(
                    "  Function {} ({}) at offset {:04x}: {}", 
                    xref.function_id, 
                    name, 
                    xref.offset,
                    xref.opcode
                );
            }
        }
        Command::BinDiff {
            input1,
            input2,
            layout,
            function_layout,
            format_version,
            diff_code,
        } => {
            commands::bindiff_cmd::run_bindiff(&input1, &input2, layout, function_layout, format_version, diff_code)?;
        }
        Command::Dump {
            input,
            kind,
            layout,
            function_layout,
        } => {
            let file = load_file(&input, layout, function_layout)?;
            commands::dump_cmd::run_dump(&file, kind);
        }
    }

    Ok(())
}
