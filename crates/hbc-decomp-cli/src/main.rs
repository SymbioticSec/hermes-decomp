use clap::Parser;
use hbc_decomp::{DecompileOptionsV2, DisasmOptions};
use std::time::Instant;

mod cli_args;
mod commands;
mod helpers;
mod tui;

use cli_args::{Cli, Command};
use helpers::{load_file, load_format, parse_globs, parse_id_ranges, write_output};

fn init_logging(spec: Option<&str>) {
    let mut builder = env_logger::Builder::new();
    // `--log <spec>` wins over RUST_LOG; otherwise fall back to the environment.
    match spec {
        Some(s) => {
            builder.parse_filters(s);
        }
        None => {
            builder.parse_env("RUST_LOG");
        }
    }
    // Compact, target-tagged format so `--log modname=trace` output is easy to grep.
    builder.format(|buf, record| {
        use std::io::Write;
        writeln!(
            buf,
            "[{:<5} {}] {}",
            record.level(),
            record.target(),
            record.args()
        )
    });
    // Already-initialized is fine (e.g. tests); ignore the error.
    let _ = builder.try_init();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    init_logging(cli.log.as_deref());
    // Give Rayon workers a large stack up front: decompilation recurses deeply
    // and the default stack overflows on big bundles (e.g. `decompile
    // --resolve-closures` on a multi-MB Metro bundle).
    hbc_decomp::configure_thread_pool();

    commands::update_cmd::auto_check_on_startup();

    match cli.command {
        Command::Info { input, format } => {
            let file = load_file(&input, &format)?;
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
            format: format_args,
            diff_code,
        } => {
            tui::debug_log(&format!(
                "[TUI] Loading primary bundle: {}",
                input.display()
            ));
            let primary_load_start = Instant::now();
            let file = load_file(&input, &format_args)?;
            tui::debug_log(&format!(
                "[TUI] Loaded primary bundle in {:.2?} (functions: {})",
                primary_load_start.elapsed(),
                file.header.function_count
            ));

            let primary_format_start = Instant::now();
            let format = load_format(&file, format_args.format_version)?;
            tui::debug_log(&format!(
                "[TUI] Resolved primary format in {:.2?}",
                primary_format_start.elapsed()
            ));
            let path = input.display().to_string();

            let diff_target = if let Some(path2) = input2 {
                tui::debug_log(&format!(
                    "[TUI] Loading secondary bundle: {}",
                    path2.display()
                ));
                let secondary_load_start = Instant::now();
                let file2 = load_file(&path2, &format_args)?;
                tui::debug_log(&format!(
                    "[TUI] Loaded secondary bundle in {:.2?} (functions: {})",
                    secondary_load_start.elapsed(),
                    file2.header.function_count
                ));

                let secondary_format_start = Instant::now();
                let format2 = load_format(&file2, format_args.format_version)?;
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
            format: format_args,
            show_offsets,
            no_labels,
            no_strings,
            info,
        } => {
            let file = load_file(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            let options = DisasmOptions {
                show_offsets,
                show_labels: !no_labels,
                resolve_strings: !no_strings,
                enable_color: output.is_none(),
            };
            let content = if info {
                // --info: prepend a one-line metadata banner before each function.
                let ids: Vec<u32> = match function {
                    Some(id) => vec![id],
                    None => (0..file.header.function_count).collect(),
                };
                let mut out = String::new();
                for id in ids {
                    if let Some(banner) = hbc_decomp::function_info_banner(&file, id) {
                        out.push_str(&format!("; {banner}\n"));
                    }
                    out.push_str(&hbc_decomp::disassemble_function(
                        &file, &format, id, &options,
                    )?);
                    out.push('\n');
                }
                out
            } else if let Some(function_id) = function {
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
            format: format_args,
            show_offsets,
            no_strings,
            no_propagate,
            no_simplify,
            no_structure,
            expand,
            expand_depth,
            resolve_closures,
            deep,
            stable,
            json,
            check_dead_code,
            assembly,
            modules,
            module_name,
            exclude_module_name,
            from_module,
            module_depth,
            no_cache,
            cascade,
        } => {
            // Progress on stderr so long full-bundle runs are not silent.
            // Still quiet for tiny single-function dumps unless writing to a file.
            let want_progress = output.is_some() || function.is_none();
            hbc_decomp::set_progress_enabled(want_progress);

            let decomp_start = std::time::Instant::now();
            if want_progress {
                eprintln!("hermes-decomp: decompiling {} …", input.display());
            }

            let (file, file_bytes) = helpers::load_file_with_bytes(&input, &format_args)?;
            if want_progress {
                let mb = file_bytes.len() as f64 / (1024.0 * 1024.0);
                eprintln!(
                    "  • parsed: HBC v{}, {} functions, {:.2} MiB",
                    file.header.version, file.header.function_count, mb
                );
            }
            let cache_path = hbc_decomp::default_cache_path(&input);
            let format = load_format(&file, format_args.format_version)?;
            let options = DecompileOptionsV2 {
                resolve_strings: !no_strings,
                include_offsets: show_offsets || assembly,
                propagate: !no_propagate,
                simplify: !no_simplify,
                recover_structures: !no_structure,
                assembly_mode: assembly,
                deep,
                stable,
                cascade: cascade.clone(),
            };
            // The analysis cache does not key on the artifact, so a cached context
            // would be served with none of the confirmed names applied.
            let no_cache = no_cache || cascade.is_some();

            if check_dead_code {
                commands::decompile_cmd::print_dead_code_report(&file, &format)?;
                return Ok(());
            }

            let filter = hbc_decomp::ModuleFilter {
                id_ranges: parse_id_ranges(modules.as_deref()),
                name_globs: parse_globs(module_name.as_deref()),
                exclude_globs: parse_globs(exclude_module_name.as_deref()),
                from: from_module,
                depth: module_depth,
            };

            if json {
                // The same pipeline (and cache) as the JavaScript path, so the IR
                // carries the IPA, closure and module naming the text output gets.
                let ctx = if no_cache {
                    hbc_decomp::PipelineContext::build_with_options(&file, &format, &options)?
                } else {
                    hbc_decomp::PipelineContext::build_cached(
                        &file,
                        &format,
                        &options,
                        &file_bytes,
                        &cache_path,
                    )?
                };
                let selected =
                    commands::decompile_cmd::select_json_functions(&ctx, function, &filter)?;
                match &output {
                    Some(path) => {
                        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
                        commands::decompile_cmd::write_ir_json(&mut w, &file, &ctx, &selected)?;
                        eprintln!("Wrote {} ({} functions)", path.display(), selected.len());
                    }
                    None => {
                        let mut w = std::io::BufWriter::new(std::io::stdout().lock());
                        commands::decompile_cmd::write_ir_json(&mut w, &file, &ctx, &selected)?;
                    }
                }
                if want_progress {
                    eprintln!(
                        "hermes-decomp: finished in {:.1}s",
                        decomp_start.elapsed().as_secs_f64()
                    );
                }
                return Ok(());
            }

            let content = if expand {
                if let Some(function_id) = function {
                    commands::decompile_cmd::decompile_with_expansion(
                        &file,
                        &format,
                        function_id,
                        &options,
                        expand_depth,
                    )?
                } else if no_cache {
                    hbc_decomp::decompile_all_v2_with_closures(&file, &format, &options)?
                } else {
                    hbc_decomp::decompile_all_v2_with_closures_cached(
                        &file,
                        &format,
                        &options,
                        &file_bytes,
                        &cache_path,
                    )?
                }
            } else if let Some(function_id) = function {
                if resolve_closures {
                    let ctx = hbc_decomp::build_closure_context(&file, &format)?;
                    hbc_decomp::decompile_function_v2_with_context(
                        &file,
                        &format,
                        function_id,
                        &options,
                        Some(&ctx),
                    )?
                } else {
                    hbc_decomp::decompile_function_v2(&file, &format, function_id, &options)?
                }
            } else {
                match (filter.is_empty(), no_cache) {
                    (true, true) => {
                        hbc_decomp::decompile_all_v2_with_closures(&file, &format, &options)?
                    }
                    (true, false) => hbc_decomp::decompile_all_v2_with_closures_cached(
                        &file,
                        &format,
                        &options,
                        &file_bytes,
                        &cache_path,
                    )?,
                    (false, true) => {
                        hbc_decomp::decompile_filtered_v2(&file, &format, &options, Some(&filter))?
                    }
                    (false, false) => hbc_decomp::decompile_filtered_v2_cached(
                        &file,
                        &format,
                        &options,
                        Some(&filter),
                        &file_bytes,
                        &cache_path,
                    )?,
                }
            };

            let content = if assembly {
                let file_path = input.display().to_string();
                commands::decompile_cmd::format_assembly_output(
                    &content,
                    &file,
                    &file_path,
                    file_bytes.len(),
                )
            } else {
                content
            };
            // Stable mode drops the volatile `/* <id> */` module annotations. The
            // Metro id shifts whenever a module is added or removed, so it makes every
            // import line differ between two builds even when nothing there changed.
            // The readable module name stays, which is what matters for a build diff.
            let content = if stable {
                helpers::strip_module_id_comments(&content)
            } else {
                content
            };
            write_output(output, &content)?;
            if want_progress {
                eprintln!(
                    "hermes-decomp: finished in {:.1}s",
                    decomp_start.elapsed().as_secs_f64()
                );
            }
        }
        Command::Closures {
            input,
            function,
            format: format_args,
            json,
        } => {
            let file = load_file(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            commands::decompile_cmd::print_closure_info(&file, &format, function, json)?;
        }
        Command::Deps {
            input,
            module,
            format: format_args,
            depth,
            json,
        } => {
            let (file, bytes) = helpers::load_file_with_bytes(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            let cache_path = hbc_decomp::default_cache_path(&input);
            commands::extract_cmd::print_module_deps(
                &file,
                &format,
                &bytes,
                &cache_path,
                module,
                depth,
                json,
            )?;
        }
        Command::Modules {
            input,
            format: format_args,
            limit,
            json,
        } => {
            let (file, bytes) = helpers::load_file_with_bytes(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            let cache_path = hbc_decomp::default_cache_path(&input);
            commands::extract_cmd::print_modules(&file, &format, &bytes, &cache_path, limit, json)?;
        }
        Command::Debug {
            input,
            format,
            scopes,
            callees,
            vars,
        } => {
            let (file, bytes) = helpers::load_file_with_bytes(&input, &format)?;
            commands::debug_cmd::print_debug_info(&file, &bytes, scopes, callees, vars)?;
        }
        Command::Extract {
            input,
            output,
            format: format_args,
            no_strings,
        } => {
            let (file, bytes) = helpers::load_file_with_bytes(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            let cache_path = hbc_decomp::default_cache_path(&input);
            commands::extract_cmd::run_extract(
                &file,
                &format,
                &output,
                &bytes,
                &cache_path,
                !no_strings,
            )?;
        }
        Command::Graphviz {
            input,
            function,
            output,
            format: format_args,
            open,
        } => {
            let file = load_file(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            commands::graphviz_cmd::run_graphviz(&file, &format, function, output, open)?;
        }
        Command::Xref {
            input,
            query,
            kind,
            format: format_args,
            json,
        } => {
            let file = load_file(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            commands::xref_cmd::run_xref(&file, &format, &query, kind, json)?;
        }
        Command::BinDiff {
            input1,
            input2,
            format,
            diff_code,
            json,
        } => {
            commands::bindiff_cmd::run_bindiff(&input1, &input2, &format, diff_code, json)?;
        }
        Command::Dump {
            input,
            kind,
            json,
            format,
        } => {
            let file = load_file(&input, &format)?;
            commands::dump_cmd::run_dump(&file, kind, json);
        }
        Command::Cascade { action } => {
            use cli_args::CascadeAction;
            let (input, format_version) = match &action {
                CascadeAction::Extract {
                    input,
                    format_version,
                    ..
                }
                | CascadeAction::Verify {
                    input,
                    format_version,
                    ..
                } => (input.clone(), *format_version),
            };
            let format_args = cli_args::FormatArgs {
                format_version,
                layout: cli_args::LayoutArg::Auto,
                function_layout: cli_args::FunctionLayoutArg::Auto,
            };
            let (file, bytes) = helpers::load_file_with_bytes(&input, &format_args)?;
            let format = load_format(&file, format_version)?;
            let cache_path = hbc_decomp::default_cache_path(&input);
            match action {
                CascadeAction::Extract { output, .. } => commands::cascade_cmd::run_extract(
                    &file,
                    &format,
                    &bytes,
                    &cache_path,
                    output.as_deref(),
                )?,
                CascadeAction::Verify { artifact, .. } => commands::cascade_cmd::run_verify(
                    &file,
                    &format,
                    &bytes,
                    &cache_path,
                    &artifact,
                )?,
            }
        }
        Command::Callgraph {
            input,
            function,
            dot,
            depth,
            format: format_args,
            json,
        } => {
            let file = load_file(&input, &format_args)?;
            let format = load_format(&file, format_args.format_version)?;
            commands::callgraph_cmd::run_callgraph(&file, &format, function, depth, dot, json)?;
        }
        Command::Update {
            check,
            install,
            version,
        } => {
            commands::update_cmd::run(check, install, version)?;
        }
        Command::Secrets {
            input,
            format,
            json,
            show_full,
        } => {
            commands::write_cmd::run_secrets(&input, &format, json, show_full)?;
        }
        Command::FridaHooks {
            input,
            module,
            export,
            output,
            format,
        } => {
            commands::write_cmd::run_frida_hooks(&input, &format, module, export, output)?;
        }
        Command::EmitHasm {
            input,
            function,
            output,
            format,
        } => {
            commands::write_cmd::run_emit_hasm(&input, function, output, &format)?;
        }
        Command::Asm {
            input,
            hasm,
            function,
            output,
            format,
        } => {
            commands::write_cmd::run_asm(&input, &hasm, function, &output, &format)?;
        }
        Command::PatchString {
            input,
            output,
            id,
            old,
            new,
            format,
        } => {
            commands::write_cmd::run_patch_string(&input, &output, id, old, new, &format)?;
        }
        Command::PatchFunction {
            input,
            output,
            function,
            hasm,
            format,
        } => {
            commands::write_cmd::run_patch_function(&input, &output, function, &hasm, &format)?;
        }
        Command::InjectStub {
            input,
            output,
            function,
            kind,
            format,
        } => {
            commands::write_cmd::run_inject_stub(&input, &output, function, &kind, &format)?;
        }
        Command::Create {
            version,
            output,
            string,
        } => {
            commands::write_cmd::run_create(version, &output, string)?;
        }
        Command::AsmCheck { input, function } => {
            commands::write_cmd::run_roundtrip_check(&input, function)?;
        }
    }

    Ok(())
}
