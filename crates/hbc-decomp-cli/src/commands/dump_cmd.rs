use crate::cli_args::DumpKind;
use hbc_decomp::{BytecodeFile, TableKind};

fn table_kind(kind: DumpKind) -> Option<TableKind> {
    match kind {
        DumpKind::Strings | DumpKind::Functions => None,
        DumpKind::CjsModules => Some(TableKind::CjsModules),
        DumpKind::Regexp => Some(TableKind::RegExp),
        DumpKind::ObjShapes => Some(TableKind::ObjShapes),
        DumpKind::FunctionSources => Some(TableKind::FunctionSources),
        DumpKind::StringKinds => Some(TableKind::StringKinds),
        DumpKind::Sections => Some(TableKind::Sections),
        DumpKind::BigInt => Some(TableKind::BigInt),
        DumpKind::ArrayBuffer => Some(TableKind::ArrayBuffer),
    }
}

pub fn run_dump(file: &BytecodeFile, kind: DumpKind, json: bool) {
    if let Some(tk) = table_kind(kind) {
        if json {
            let value = hbc_decomp::dump_table_json(file, tk);
            match serde_json::to_string_pretty(&value) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("Failed to serialize table: {e}"),
            }
        } else {
            print!("{}", hbc_decomp::dump_table(file, tk));
        }
        return;
    }

    // strings / functions retain their original behavior
    if json {
        let value = match kind {
            DumpKind::Strings => strings_json(file),
            DumpKind::Functions => functions_json(file),
            _ => unreachable!(),
        };
        match serde_json::to_string_pretty(&value) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("Failed to serialize table: {e}"),
        }
        return;
    }

    match kind {
        DumpKind::Strings => dump_strings(file),
        DumpKind::Functions => dump_functions(file),
        _ => unreachable!(),
    }
}

fn dump_strings(file: &BytecodeFile) {
    println!("String Table ({} entries):", file.header.string_count);
    println!("----------------------------------------");
    for i in 0..file.header.string_count {
        if let Some(entry) = file.string_at(i) {
            println!("[{}] {}", i, hbc_decomp::escape_js_string(&entry.value));
        } else {
            println!("[{i}] <error>");
        }
    }
}

fn dump_functions(file: &BytecodeFile) {
    println!("Function Table ({} entries):", file.header.function_count);
    println!("----------------------------------------");
    println!("{:<5} {:<30} {:<10} {:<10}", "ID", "Name", "Offset", "Size");

    for (i, header) in file.function_headers.iter().enumerate() {
        let name = file
            .string_at(header.function_name())
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("f{i}"));

        println!(
            "{:<5} {:<30} {:<10x} {:<10}",
            i,
            name,
            header.offset(),
            header.bytecode_size_in_bytes()
        );
    }
}

fn strings_json(file: &BytecodeFile) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = (0..file.header.string_count)
        .map(|i| {
            serde_json::json!({
                "index": i,
                "value": file.string_at(i).map(|e| e.value.clone()),
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}

fn functions_json(file: &BytecodeFile) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = file
        .function_headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            serde_json::json!({
                "id": i,
                "name": file.string_at(h.function_name()).map(|e| e.value.clone()),
                "offset": h.offset(),
                "size": h.bytecode_size_in_bytes(),
                "params": h.param_count(),
                "frame_size": h.frame_size(),
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}
