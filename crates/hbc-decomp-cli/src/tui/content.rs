//! Content generation for the TUI: per-function disassembly / decompilation for
//! both files, plus the split/diff rendering driven by `App::content`.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use similar::{ChangeTag, DiffTag, TextDiff};

use hbc_decomp::{
    build_closure_context, decompile_function_v2, decompile_function_v2_with_context,
    DecompileOptionsV2,
};

use super::app::{App, ViewMode};
use super::diff::DiffStatus;
use super::formatting::{format_disasm_colored, format_info, highlight_code};

impl App {
    pub fn content(&mut self) -> (Text<'static>, Option<Text<'static>>) {
        if self.function_names.is_empty() {
            if self.diff_analyzing {
                return (Text::raw("Analyzing functions..."), None);
            }
            return (Text::raw("No functions found matching query."), None);
        }

        // A function that exists only in file 2 ("added") has no file-1 id, so
        // the normal single-pane views would just say "not present". Fall back
        // to its file-2 code so it is actually viewable. (The split diff view
        // below keeps the left pane on file 1 on purpose.)
        let only_in_file2 =
            self.selected_function_id().is_none() && self.selected_function_id2().is_some();

        match self.view {
            ViewMode::Info => (Text::raw(self.format_info_wrapper()), None),
            ViewMode::Disasm => {
                if only_in_file2 {
                    let id2 = self.selected_function_id2().unwrap();
                    return (self.disasm_content2(id2), None);
                }
                (self.disasm_content(), None)
            }
            ViewMode::Decompile => {
                if only_in_file2 {
                    let id2 = self.selected_function_id2().unwrap();
                    return (Text::from(highlight_code(&self.decompile_content2(id2))), None);
                }
                (Text::from(highlight_code(&self.decompile_content())), None)
            }
            ViewMode::Diff => {
                // Unified (git-style) single-column view, if toggled with `u`.
                if self.diff_unified && self.file2.is_some() {
                    return (self.unified_diff_text(), None);
                }
                // If showing diff colors, we need to compute the diff and styling manually
                if self.show_diff_colors && self.file2.is_some() {
                    let left_str = match self.diff_kind {
                        ViewMode::Disasm => {
                            self.get_disasm_string_local(self.selected_function_id())
                        }
                        _ => self.decompile_content(), // Returns String
                    };

                    let right_str = {
                        let name_opt = self.selected_function_name();
                        if let Some(name) = name_opt {
                            let name = name.to_string();
                            let id2 = if let Some(id) = self.map2.get(&name) {
                                Some(*id)
                            } else if let Some(DiffStatus::Renamed(new_name)) =
                                self.diff_status.get(&name)
                            {
                                self.map2.get(new_name).copied()
                            } else {
                                None
                            };

                            if let Some(id2) = id2 {
                                match self.diff_kind {
                                    ViewMode::Disasm => self.get_disasm_string_remote(id2),
                                    _ => self.decompile_content2(id2), // Returns String
                                }
                            } else {
                                String::new() // Function doesn't exist in right
                            }
                        } else {
                            String::new()
                        }
                    };

                    if right_str.is_empty() {
                        let left = match self.diff_kind {
                            ViewMode::Disasm => self.disasm_content(),
                            _ => Text::raw(left_str),
                        };
                        return (
                            left,
                            Some(Text::raw("Function removed or renamed in file 2.")),
                        );
                    }

                    // Compute Diff
                    let diff = TextDiff::from_lines(&left_str, &right_str);

                    // Optimization: Collect lines into Vec for O(1) access
                    let left_lines_vec: Vec<&str> = left_str.lines().collect();
                    let right_lines_vec: Vec<&str> = right_str.lines().collect();

                    let mut l_lines = Vec::new();
                    let mut r_lines = Vec::new();

                    for op in diff.ops() {
                        match op.tag() {
                            DiffTag::Delete => {
                                // Lines only in Left
                                for i in op.old_range() {
                                    let content = left_lines_vec.get(i).unwrap_or(&"").to_string();
                                    l_lines.push(Line::from(Span::styled(
                                        content,
                                        Style::default().bg(Color::Red).fg(Color::White),
                                    )));
                                    // Padding in Right
                                    r_lines.push(Line::from(""));
                                }
                            }
                            DiffTag::Insert => {
                                // Lines only in Right
                                for i in op.new_range() {
                                    let content = right_lines_vec.get(i).unwrap_or(&"").to_string();
                                    r_lines.push(Line::from(Span::styled(
                                        content,
                                        Style::default().bg(Color::Green).fg(Color::Black),
                                    )));
                                    // Padding in Left
                                    l_lines.push(Line::from(""));
                                }
                            }
                            DiffTag::Replace => {
                                // Left has lines, Right has lines.
                                let old_len = op.old_range().len();
                                let new_len = op.new_range().len();
                                let max_len = std::cmp::max(old_len, new_len);

                                for i in 0..max_len {
                                    if i < old_len {
                                        let idx = op.old_range().start + i;
                                        let content =
                                            left_lines_vec.get(idx).unwrap_or(&"").to_string();
                                        l_lines.push(Line::from(Span::styled(
                                            content,
                                            Style::default().bg(Color::Red).fg(Color::White),
                                        )));
                                    } else {
                                        l_lines.push(Line::from(""));
                                    }

                                    if i < new_len {
                                        let idx = op.new_range().start + i;
                                        let content =
                                            right_lines_vec.get(idx).unwrap_or(&"").to_string();
                                        r_lines.push(Line::from(Span::styled(
                                            content,
                                            Style::default().bg(Color::Green).fg(Color::Black),
                                        )));
                                    } else {
                                        r_lines.push(Line::from(""));
                                    }
                                }
                            }
                            DiffTag::Equal => {
                                for i in op.old_range() {
                                    let content = left_lines_vec.get(i).unwrap_or(&"").to_string();
                                    l_lines.push(Line::from(content.clone()));
                                    r_lines.push(Line::from(content)); // Identical
                                }
                            }
                        }
                    }

                    (Text::from(l_lines), Some(Text::from(r_lines)))
                } else {
                    // Standard non-colored view (or if file2 missing)
                    let left = match self.diff_kind {
                        ViewMode::Disasm => self.disasm_content(),
                        _ => Text::raw(self.decompile_content()),
                    };

                    let right = if self.file2.is_some() {
                        let name_opt = self.selected_function_name();
                        if let Some(name) = name_opt {
                            let name = name.to_string();
                            let id2 = if let Some(id) = self.map2.get(&name) {
                                Some(*id)
                            } else if let Some(DiffStatus::Renamed(new_name)) =
                                self.diff_status.get(&name)
                            {
                                self.map2.get(new_name).copied()
                            } else {
                                None
                            };

                            if let Some(id2) = id2 {
                                match self.diff_kind {
                                    ViewMode::Disasm => Some(self.disasm_content2(id2)),
                                    _ => Some(Text::raw(self.decompile_content2(id2))),
                                }
                            } else {
                                match self.diff_status.get(&name) {
                                    Some(DiffStatus::Renamed(new_name)) => {
                                        Some(Text::raw(format!("Renamed to {new_name}")))
                                    }
                                    Some(DiffStatus::Removed) => {
                                        Some(Text::raw("Function removed in file 2."))
                                    }
                                    _ => Some(Text::raw("Function removed or renamed in file 2.")),
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        Some(Text::raw("No second file loaded."))
                    };

                    (left, right)
                }
            }
        }
    }

    // get_line_from_str removed (optimized out)

    // Helper to get raw string for disasm
    fn get_disasm_string_local(&self, id_opt: Option<u32>) -> String {
        if let Some(id) = id_opt {
            hbc_decomp::disassemble_function(
                &self.file,
                &self.format,
                id,
                &hbc_decomp::DisasmOptions::default(),
            )
            .unwrap_or_default()
        } else {
            String::new()
        }
    }

    fn get_disasm_string_remote(&self, id: u32) -> String {
        let (file2, format2) = match (self.file2.as_ref(), self.format2.as_ref()) {
            (Some(f), Some(fmt)) => (f, fmt),
            _ => return String::new(),
        };
        hbc_decomp::disassemble_function(file2, format2, id, &hbc_decomp::DisasmOptions::default())
            .unwrap_or_default()
    }

    // --- Content Generators for File 1 ---

    fn ensure_closure_ctx_local(&mut self) {
        if self.closure_ctx_attempted {
            return;
        }
        self.closure_ctx_attempted = true;
        self.closure_ctx = build_closure_context(&self.file, &self.format).ok();
    }

    fn ensure_closure_ctx_remote(&mut self) {
        if self.closure_ctx2_attempted {
            return;
        }
        self.closure_ctx2_attempted = true;
        if let (Some(file2), Some(format2)) = (self.file2.as_ref(), self.format2.as_ref()) {
            self.closure_ctx2 = build_closure_context(file2, format2).ok();
        }
    }

    pub fn disasm_content(&mut self) -> Text<'static> {
        let function_id = match self.selected_function_id() {
            Some(id) => id,
            None => return Text::raw("Function not present in this file (Added in v2)"),
        };

        if let Some(content) = self.disasm_cache.get(&(function_id as usize)) {
            return content.clone();
        }

        let content = match self
            .file
            .decode_function_instructions(&self.format, function_id)
        {
            Ok(instructions) => format_disasm_colored(&instructions, &self.format, &self.file),
            Err(e) => Text::raw(format!("Error: {e}")),
        };

        self.disasm_cache
            .insert(function_id as usize, content.clone());
        content
    }

    pub fn decompile_content(&mut self) -> String {
        let function_id = match self.selected_function_id() {
            Some(id) => id,
            None => return "Function not present in this file (Added in v2)".to_string(),
        };

        if let Some(content) = self.decompile_cache.get(&(function_id as usize)) {
            return content.clone();
        }

        // Use full pipeline context if available (IPA, Metro, naming)
        let content = if let Some(ctx) = &self.pipeline_ctx {
            ctx.generate_function_code(&self.file, function_id)
        } else {
            // Fallback: basic single-function decompilation while pipeline builds
            let options = DecompileOptionsV2::optimized();
            self.ensure_closure_ctx_local();

            if let Some(closure) = self.closure_ctx.as_ref() {
                decompile_function_v2_with_context(
                    &self.file,
                    &self.format,
                    function_id,
                    &options,
                    Some(closure),
                )
            } else {
                decompile_function_v2(&self.file, &self.format, function_id, &options)
            }
            .unwrap_or_else(|err| format!("error: {err}"))
        };

        self.decompile_cache
            .insert(function_id as usize, content.clone());
        content
    }

    // --- Content Generators for File 2 ---

    pub fn disasm_content2(&mut self, function_id: u32) -> Text<'static> {
        if let Some(content) = self.disasm_cache2.get(&(function_id as usize)) {
            return content.clone();
        }

        let (file2, format2) = match (self.file2.as_ref(), self.format2.as_ref()) {
            (Some(f), Some(fmt)) => (f, fmt),
            _ => return Text::raw("No second file loaded"),
        };

        let content = match file2.decode_function_instructions(format2, function_id) {
            Ok(instructions) => format_disasm_colored(&instructions, format2, file2),
            Err(e) => Text::raw(format!("Error: {e}")),
        };

        self.disasm_cache2
            .insert(function_id as usize, content.clone());
        content
    }

    pub fn decompile_content2(&mut self, function_id: u32) -> String {
        if let Some(content) = self.decompile_cache2.get(&(function_id as usize)) {
            return content.clone();
        }

        // Guard: both file2 and format2 must be present for diff mode
        if self.file2.is_none() || self.format2.is_none() {
            return "No second file loaded".to_string();
        }

        let content = if self.pipeline_ctx2.is_some() {
            // Use full pipeline context if available (IPA, Metro, naming)
            let file2 = self.file2.as_ref().unwrap();
            self.pipeline_ctx2
                .as_ref()
                .unwrap()
                .generate_function_code(file2, function_id)
        } else {
            // Fallback: basic single-function decompilation while pipeline builds
            self.ensure_closure_ctx_remote();
            let file2 = self.file2.as_ref().unwrap();
            let format2 = self.format2.as_ref().unwrap();
            let options = DecompileOptionsV2::optimized();
            if let Some(ctx) = self.closure_ctx2.as_ref() {
                decompile_function_v2_with_context(file2, format2, function_id, &options, Some(ctx))
            } else {
                decompile_function_v2(file2, format2, function_id, &options)
            }
            .unwrap_or_else(|err| format!("error: {err}"))
        };

        self.decompile_cache2
            .insert(function_id as usize, content.clone());
        content
    }

    pub fn format_info_wrapper(&self) -> String {
        let name_opt = self.selected_function_name();
        let name = match name_opt {
            Some(n) => n,
            None => return "No function selected.".to_string(),
        };
        let status = self.diff_status.get(name);

        format_info(
            &self.file,
            &self.path,
            &self.file2,
            &self.path2,
            self.selected,
            &self.function_names,
            &self.map1,
            &self.map2,
            status,
        )
    }

    /// Left (file 1) and right (file 2) source for the selected function's
    /// diff. A side is empty when the function exists only in the other file
    /// (added / removed).
    fn diff_left_right(&mut self) -> (String, String) {
        let left = if self.selected_function_id().is_some() {
            match self.diff_kind {
                ViewMode::Disasm => self.get_disasm_string_local(self.selected_function_id()),
                _ => self.decompile_content(),
            }
        } else {
            String::new()
        };
        let right = if let Some(id2) = self.selected_function_id2() {
            match self.diff_kind {
                ViewMode::Disasm => self.get_disasm_string_remote(id2),
                _ => self.decompile_content2(id2),
            }
        } else {
            String::new()
        };
        (left, right)
    }

    /// Git-style unified diff of the selected function: a single column showing
    /// the full before/after with `-`/`+` markers and old/new line numbers.
    fn unified_diff_text(&mut self) -> Text<'static> {
        let (left, right) = self.diff_left_right();
        let kind = match self.diff_kind {
            ViewMode::Disasm => "disassembly",
            _ => "decompiled code",
        };
        let name = self.selected_function_name().unwrap_or("?").to_string();

        let mut lines: Vec<Line<'static>> = vec![
            Line::from(Span::styled(
                format!("--- file 1: {name} ({kind})"),
                Style::default().fg(Color::Red),
            )),
            Line::from(Span::styled(
                format!("+++ file 2: {name} ({kind})"),
                Style::default().fg(Color::Green),
            )),
            Line::from(Span::styled(
                "  old   new",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let diff = TextDiff::from_lines(&left, &right);
        for change in diff.iter_all_changes() {
            let old_n = change
                .old_index()
                .map(|i| format!("{:>5}", i + 1))
                .unwrap_or_else(|| "     ".to_string());
            let new_n = change
                .new_index()
                .map(|i| format!("{:>5}", i + 1))
                .unwrap_or_else(|| "     ".to_string());
            let (sign, color) = match change.tag() {
                ChangeTag::Delete => ('-', Some(Color::Red)),
                ChangeTag::Insert => ('+', Some(Color::Green)),
                ChangeTag::Equal => (' ', None),
            };
            let text = change.value().trim_end_matches('\n').to_string();
            let content_style = color.map_or_else(Style::default, |c| Style::default().fg(c));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{old_n} {new_n} {sign} "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(text, content_style),
            ]));
        }

        Text::from(lines)
    }
}
