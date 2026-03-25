use std::collections::HashMap;
use std::sync::Arc;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use hbc_decomp::{
    collect_label_offsets, escape_js_string, BytecodeFile, BytecodeFormat, Instruction, Operand,
    OperandType, OperandValue,
};

pub fn format_disasm_colored(
    instructions: &[Instruction],
    format: &BytecodeFormat,
    file: &BytecodeFile,
) -> Text<'static> {
    let mut lines = Vec::new();
    let label_offsets = collect_label_offsets(instructions, format);

    for insn in instructions {
        if label_offsets.contains(&insn.offset) {
            lines.push(Line::from(vec![Span::styled(
                format!("L{}:", insn.offset),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]));
        }

        let mut spans = Vec::new();

        // Offset
        spans.push(Span::styled(
            format!("{:04x}  ", insn.offset),
            Style::default().fg(Color::DarkGray),
        ));

        // Opcode
        let def = match format.definitions.get(insn.opcode as usize) {
            Some(d) => d,
            None => {
                lines.push(Line::from(vec![Span::raw(format!(
                    "<unknown opcode {}>",
                    insn.opcode
                ))]));
                continue;
            }
        };
        spans.push(Span::styled(
            def.name.clone(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::raw(" "));

        // Operands
        for (i, operand) in insn.operands.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(", "));
            }
            spans.push(format_operand(insn, operand, file));
        }

        lines.push(Line::from(spans));
    }

    Text::from(lines)
}

fn format_operand(insn: &Instruction, operand: &Operand, file: &BytecodeFile) -> Span<'static> {
    match operand.ty {
        OperandType::Reg8 | OperandType::Reg32 => {
            let s = match operand.value {
                OperandValue::U8(v) => format!("r{v}"),
                OperandValue::U16(v) => format!("r{v}"),
                OperandValue::U32(v) => format!("r{v}"),
                _ => "r?".to_string(),
            };
            Span::styled(s, Style::default().fg(Color::Red))
        }
        OperandType::Addr8 | OperandType::Addr32 => {
            let s = match operand.value.as_i32() {
                Some(rel) => {
                    let target = insn.offset as i32 + rel;
                    format!("L{target}")
                }
                None => "L?".to_string(),
            };
            Span::styled(s, Style::default().fg(Color::Yellow))
        }
        OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S => {
            if let Some(id) = operand.value.as_u32() {
                if let Some(entry) = file.string_at(id) {
                    let s = escape_js_string(&entry.value);
                    return Span::styled(format!("\"{s}\""), Style::default().fg(Color::Green));
                }
            }
            let s = format_operand_value(&operand.value);
            Span::styled(s, Style::default().fg(Color::Magenta))
        }
        _ => {
            let s = format_operand_value(&operand.value);
            Span::styled(s, Style::default().fg(Color::Magenta))
        }
    }
}

fn format_operand_value(value: &OperandValue) -> String {
    match value {
        OperandValue::U8(v) => v.to_string(),
        OperandValue::U16(v) => v.to_string(),
        OperandValue::U32(v) => v.to_string(),
        OperandValue::I8(v) => v.to_string(),
        OperandValue::I32(v) => v.to_string(),
        OperandValue::F64(v) => format!("{v}"),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn format_info(
    file: &BytecodeFile,
    path: &String,
    file2: &Option<Arc<BytecodeFile>>,
    path2: &Option<String>,
    selected: usize,
    function_names: &[String],
    map1: &HashMap<String, u32>,
    map2: &HashMap<String, u32>,
    status: Option<&crate::tui::diff::DiffStatus>,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("File: {path}"));
    lines.push(format!("Version: {}", file.header.version));
    lines.push(format!("Header layout: {:?}", file.header.layout));
    lines.push(format!("Functions: {}", file.header.function_count));
    lines.push(format!("Strings: {}", file.header.string_count));

    if let Some(p2) = path2 {
        lines.push(format!("\nFile 2: {p2}"));
        if let Some(f2) = file2 {
            lines.push(format!("Version: {}", f2.header.version));
            lines.push(format!("Functions: {}", f2.header.function_count));
        }
    }
    lines.push("".to_string());

    if selected < function_names.len() {
        let name = &function_names[selected];
        // Resolve function ID from map1 (correct even in diff mode)
        let func_id = map1.get(name).copied();

        lines.push(format!("Current Function: {name}"));
        if let Some(id) = func_id {
            if let Some(header) = file.function_headers.get(id as usize) {
                lines.push(format!("ID: {}", header.function_id()));
                lines.push(format!(
                    "Bytecode size: {}",
                    header.bytecode_size_in_bytes()
                ));
                lines.push(format!("Frame size: {}", header.frame_size()));
                lines.push(format!("Flags: 0x{:02x}", header.flags()));
            }
        } else {
            lines.push("(Not present in file 1)".to_string());
        }

        if file2.is_some() {
            if let Some(id2) = map2.get(name) {
                lines.push(format!("\nMatches in File 2: ID {id2}"));
                if let Some(f2) = file2 {
                    if let Some(h2) = f2.function_headers.get(*id2 as usize) {
                        lines.push(format!("Bytecode size: {}", h2.bytecode_size_in_bytes()));
                        if let Some(fid) = func_id {
                            if let Some(h1) = file.function_headers.get(fid as usize) {
                                if h2.bytecode_size_in_bytes() != h1.bytecode_size_in_bytes() {
                                    lines.push("Status: MODIFIED (Size mismatch)".to_string());
                                } else {
                                    lines.push(
                                        "Status: POTENTIALLY IDENTICAL (Size match)".to_string(),
                                    );
                                }
                            }
                        }
                    }
                }
            } else {
                match status {
                    Some(crate::tui::diff::DiffStatus::Renamed(new_name)) => {
                        lines.push(format!("\nStatus: RENAMED to {new_name}"));
                    }
                    Some(crate::tui::diff::DiffStatus::Removed) => {
                        lines.push("\nStatus: REMOVED in File 2".to_string());
                    }
                    _ => {
                        if file2.is_some() {
                            lines.push("\nStatus: REMOVED or RENAMED (No match found)".to_string());
                        }
                    }
                }
            }
        }
    }

    lines.join("\n")
}

pub fn highlight_code(code: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let keywords = [
        "var",
        "let",
        "const",
        "function",
        "if",
        "else",
        "return",
        "this",
        "new",
        "throw",
        "try",
        "catch",
        "while",
        "for",
        "break",
        "continue",
        "await",
        "async",
        "import",
        "export",
        "from",
        "switch",
        "case",
        "default",
        "typeof",
        "void",
        "delete",
        "in",
        "of",
        "instanceof",
        "Symbol",
        "Promise",
        "Object",
        "Array",
        "String",
        "Number",
        "Boolean",
        "JSON",
        "Math",
        "console",
    ];

    for line_str in code.lines() {
        let mut spans = Vec::new();
        let mut current_word = String::new();
        let mut in_string = false;
        let mut string_char = '\0';
        let mut in_comment = false;
        let mut chars = line_str.chars().peekable();

        while let Some(c) = chars.next() {
            if in_comment {
                current_word.push(c);
                continue;
            }

            if in_string {
                current_word.push(c);
                if c == string_char {
                    // Count trailing backslashes before this quote (excluding the quote itself)
                    let before_quote = &current_word[..current_word.len() - 1];
                    let num_backslashes = before_quote.chars().rev().take_while(|&ch| ch == '\\').count();
                    // Quote is escaped only if preceded by an odd number of backslashes
                    if num_backslashes % 2 == 0 {
                        spans.push(Span::styled(
                            current_word.clone(),
                            Style::default().fg(Color::Green),
                        ));
                        current_word.clear();
                        in_string = false;
                    }
                }
                continue;
            }

            // Start of comment
            if c == '/' && chars.peek() == Some(&'/') {
                if !current_word.is_empty() {
                    spans.push(Span::raw(current_word.clone()));
                    current_word.clear();
                }
                in_comment = true;
                current_word.push(c);
                continue;
            }

            // Start of string
            if c == '"' || c == '\'' || c == '`' {
                if !current_word.is_empty() {
                    if keywords.contains(&current_word.as_str()) {
                        spans.push(Span::styled(
                            current_word.clone(),
                            Style::default().fg(Color::Magenta),
                        ));
                    } else if current_word == "undefined"
                        || current_word == "null"
                        || current_word == "true"
                        || current_word == "false"
                    {
                        spans.push(Span::styled(
                            current_word.clone(),
                            Style::default().fg(Color::Yellow),
                        ));
                    } else {
                        spans.push(Span::raw(current_word.clone()));
                    }
                    current_word.clear();
                }
                in_string = true;
                string_char = c;
                current_word.push(c);
                continue;
            }

            if c.is_alphanumeric() || c == '_' || c == '$' {
                current_word.push(c);
            } else {
                if !current_word.is_empty() {
                    if keywords.contains(&current_word.as_str()) {
                        spans.push(Span::styled(
                            current_word.clone(),
                            Style::default().fg(Color::Magenta),
                        ));
                    } else if current_word == "undefined"
                        || current_word == "null"
                        || current_word == "true"
                        || current_word == "false"
                    {
                        spans.push(Span::styled(
                            current_word.clone(),
                            Style::default().fg(Color::Yellow),
                        ));
                    } else {
                        spans.push(Span::raw(current_word.clone()));
                    }
                    current_word.clear();
                }

                spans.push(Span::raw(c.to_string()));
            }
        }

        if !current_word.is_empty() {
            if in_comment {
                spans.push(Span::styled(
                    current_word.clone(),
                    Style::default().fg(Color::DarkGray),
                ));
            } else if in_string {
                // Unterminated string
                spans.push(Span::styled(
                    current_word.clone(),
                    Style::default().fg(Color::Green),
                ));
            } else if keywords.contains(&current_word.as_str()) {
                spans.push(Span::styled(
                    current_word.clone(),
                    Style::default().fg(Color::Magenta),
                ));
            } else if current_word == "undefined"
                || current_word == "null"
                || current_word == "true"
                || current_word == "false"
            {
                spans.push(Span::styled(
                    current_word.clone(),
                    Style::default().fg(Color::Yellow),
                ));
            } else {
                spans.push(Span::raw(current_word.clone()));
            }
        }

        lines.push(Line::from(spans));
    }
    lines
}
