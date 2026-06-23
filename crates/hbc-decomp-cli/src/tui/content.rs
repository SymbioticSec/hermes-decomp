//! Content generation for the TUI: per-function disassembly / decompilation for
//! both files, plus the split/diff rendering driven by `App::content`.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use similar::{DiffTag, TextDiff};

use hbc_decomp::DecompileOptionsV2;

use super::app::{App, ViewMode};
use super::diff::DiffStatus;
use super::formatting::{format_disasm_colored, format_info, highlight_code};
use super::{decompile_or_log, disasm_or_log};

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
        match id_opt {
            Some(id) => disasm_or_log(&self.file, &self.format, id),
            None => String::new(),
        }
    }

    fn get_disasm_string_remote(&self, id: u32) -> String {
        match (self.file2.as_ref(), self.format2.as_ref()) {
            (Some(file2), Some(format2)) => disasm_or_log(file2, format2, id),
            _ => String::new(),
        }
    }

    // --- Content Generators for File 1 ---

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

        // Decompiled output was requested: make sure the (lazy) pipeline is
        // building so quality upgrades once it's ready.
        if self.pipeline_ctx.is_none() {
            self.ensure_pipeline_building();
        }

        // Use full pipeline context if available (IPA, Metro, naming). Otherwise
        // fall back to a fast single-function decompile — NEVER build a
        // whole-file closure context here: that ran on the UI thread inside
        // terminal.draw() and froze the TUI for seconds on large bundles. The
        // background pipeline upgrades quality once it's ready.
        let content = if let Some(ctx) = &self.pipeline_ctx {
            ctx.generate_function_code(&self.file, function_id)
        } else {
            let options = DecompileOptionsV2::optimized();
            decompile_or_log(&self.file, &self.format, function_id, &options)
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
            // Fast fallback while the pipeline builds — no whole-file work on
            // the UI thread (see decompile_content for why).
            let file2 = self.file2.as_ref().unwrap();
            let format2 = self.format2.as_ref().unwrap();
            let options = DecompileOptionsV2::optimized();
            decompile_or_log(file2, format2, function_id, &options)
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

}
