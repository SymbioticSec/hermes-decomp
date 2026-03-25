use crate::cli_args::DumpKind;
use hbc_decomp::BytecodeFile;

pub fn run_dump(file: &BytecodeFile, kind: DumpKind) {
    match kind {
        DumpKind::Strings => dump_strings(file),
        DumpKind::Functions => dump_functions(file),
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
