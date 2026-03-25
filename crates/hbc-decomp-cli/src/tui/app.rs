use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::ListState;
use similar::{DiffTag, TextDiff};
use std::collections::{HashMap, HashSet};
use std::sync::{
    mpsc::{Receiver, TryRecvError},
    Arc,
};
use std::time::Instant;

use hbc_decomp::{
    build_closure_context, decompile_function_v2, decompile_function_v2_with_context, BytecodeFile,
    BytecodeFormat, ClosureContext, DecompileOptionsV2, PipelineContext,
};

use super::formatting::{format_disasm_colored, format_info, highlight_code};
use super::debug_log;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Disasm,
    Decompile,
    Info,
    Diff,
}

use super::diff::{spawn_diff_status_worker, DiffMode, DiffProgressMsg, DiffStatus};

impl ViewMode {
    pub fn next(self, has_diff: bool) -> Self {
        match self {
            ViewMode::Disasm => ViewMode::Decompile,
            ViewMode::Decompile => ViewMode::Info,
            ViewMode::Info => {
                if has_diff {
                    ViewMode::Diff
                } else {
                    ViewMode::Disasm
                }
            }
            ViewMode::Diff => ViewMode::Disasm,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ViewMode::Disasm => "Disasm",
            ViewMode::Decompile => "Decompile",
            ViewMode::Info => "Info",
            ViewMode::Diff => "BinDiff (Split View)",
        }
    }
}

pub struct App {
    pub file: Arc<BytecodeFile>,
    pub format: Arc<BytecodeFormat>,
    pub path: String,

    // Second file for diffing
    pub file2: Option<Arc<BytecodeFile>>,
    pub format2: Option<Arc<BytecodeFormat>>,
    pub path2: Option<String>,
    pub map1: HashMap<String, u32>,
    pub map2: HashMap<String, u32>,

    // Store all names and filtered names for search
    pub all_function_names: Vec<String>,
    pub function_names: Vec<String>,

    // Search state
    pub search_query: String,
    pub is_searching: bool,
    pub selected: usize,
    pub scroll: u32,
    pub view: ViewMode,

    // Track what kind of diff to show (based on previous view)
    pub diff_kind: ViewMode, // Disasm or Decompile

    pub disasm_cache: HashMap<usize, Text<'static>>,
    pub decompile_cache: HashMap<usize, String>,

    // Caches for file 2
    pub disasm_cache2: HashMap<usize, Text<'static>>,
    pub decompile_cache2: HashMap<usize, String>,

    // Optional closure contexts for higher-quality v2 pseudocode.
    pub closure_ctx: Option<ClosureContext>,
    pub closure_ctx2: Option<ClosureContext>,
    pub closure_ctx_attempted: bool,
    pub closure_ctx2_attempted: bool,

    // Diff status for each function (by name)
    pub diff_status: HashMap<String, DiffStatus>,
    pub diff_rx: Option<Receiver<DiffProgressMsg>>,
    pub diff_analyzing: bool,
    pub diff_progress_done: usize,
    pub diff_progress_total: usize,
    known_names: HashSet<String>,

    // Mode used for diff calculation (Assembly or Code)
    pub diff_calc_mode: DiffMode,

    // Toggle for content diff highlighting
    pub show_diff_colors: bool,

    // Persistent list state for function list scrolling
    pub list_state: ListState,

    // Full pipeline context (IPA, Metro, naming) — built in background
    pub pipeline_ctx: Option<Arc<PipelineContext>>,
    pub pipeline_rx: Option<Receiver<PipelineContext>>,
    pub pipeline_building: bool,

    // Pipeline context for file 2 (diff mode)
    pub pipeline_ctx2: Option<Arc<PipelineContext>>,
    pub pipeline_rx2: Option<Receiver<PipelineContext>>,
    pub pipeline_building2: bool,
}

impl App {
    pub fn new(
        file: BytecodeFile,
        format: BytecodeFormat,
        path: String,
        diff_target: Option<(BytecodeFile, BytecodeFormat, String)>,
        diff_code: bool,
    ) -> Self {
        let total_start = Instant::now();
        let has_diff_target = diff_target.is_some();
        debug_log(&format!(
            "[TUI] App::new start (primary_funcs: {}, diff_target: {}, diff_mode: {})",
            file.header.function_count,
            has_diff_target,
            if diff_code { "code" } else { "assembly" }
        ));

        let primary_names_start = Instant::now();
        let mut function_names = Vec::new();
        for header in &file.function_headers {
            let name = file
                .string_at(header.function_name())
                .map(|entry| entry.value.clone())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("f{}", header.function_id()));
            function_names.push(name);
        }
        debug_log(&format!(
            "[TUI] Indexed primary function names: {} in {:.2?}",
            function_names.len(),
            primary_names_start.elapsed()
        ));

        let initial_names = if has_diff_target {
            Vec::new()
        } else {
            function_names.clone()
        };

        let (file2, format2, path2, map2) = if let Some((f2, fmt2, p2)) = diff_target {
            let secondary_map_start = Instant::now();
            let mut m = HashMap::new();
            for header in &f2.function_headers {
                let name = f2
                    .string_at(header.function_name())
                    .map(|entry| entry.value.clone())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| format!("f{}", header.function_id()));
                m.insert(name, header.function_id());
            }
            debug_log(&format!(
                "[TUI] Indexed secondary function names: {} in {:.2?}",
                m.len(),
                secondary_map_start.elapsed()
            ));
            (Some(Arc::new(f2)), Some(Arc::new(fmt2)), Some(p2), m)
        } else {
            (None, None, None, HashMap::new())
        };

        let file = Arc::new(file);
        let format = Arc::new(format);

        let mut app = Self {
            file,
            format,
            path,
            file2,
            format2,
            path2,
            map1: HashMap::new(),
            map2,
            all_function_names: initial_names.clone(),
            function_names: initial_names,
            search_query: String::new(),
            is_searching: false,
            selected: 0,
            scroll: 0,
            view: ViewMode::Disasm,
            diff_kind: ViewMode::Decompile, // Default diff mode
            disasm_cache: HashMap::new(),
            decompile_cache: HashMap::new(),
            disasm_cache2: HashMap::new(),
            decompile_cache2: HashMap::new(),
            closure_ctx: None,
            closure_ctx2: None,
            closure_ctx_attempted: false,
            closure_ctx2_attempted: false,
            diff_status: HashMap::new(),
            diff_rx: None,
            diff_analyzing: false,
            diff_progress_done: 0,
            diff_progress_total: 0,
            known_names: HashSet::new(),
            diff_calc_mode: if diff_code {
                DiffMode::Code
            } else {
                DiffMode::Assembly
            },
            show_diff_colors: true, // Default to true as user requested colors
            list_state: ListState::default().with_selected(Some(0)),
            pipeline_ctx: None,
            pipeline_rx: None,
            pipeline_building: false,
            pipeline_ctx2: None,
            pipeline_rx2: None,
            pipeline_building2: false,
        };

        let map1_start = Instant::now();
        app.map1 = app.build_function_map_local();
        debug_log(&format!(
            "[TUI] Built primary function map: {} entries in {:.2?}",
            app.map1.len(),
            map1_start.elapsed()
        ));

        if let (Some(file2), Some(format2)) = (&app.file2, &app.format2) {
            let mut all_names = HashSet::new();
            for name in app.map1.keys() {
                all_names.insert(name.clone());
            }
            for name in app.map2.keys() {
                all_names.insert(name.clone());
            }

            app.diff_progress_total = all_names.len();
            app.diff_analyzing = true;
            debug_log(&format!(
                "[TUI] Spawning diff worker for {} functions (mode: {:?})",
                app.diff_progress_total, app.diff_calc_mode
            ));
            app.diff_rx = Some(spawn_diff_status_worker(
                app.file.clone(),
                app.format.clone(),
                app.map1.clone(),
                file2.clone(),
                format2.clone(),
                app.map2.clone(),
                app.diff_calc_mode,
            ));
        } else {
            app.known_names = app.all_function_names.iter().cloned().collect();
        }

        // Spawn background pipeline build (IPA, Metro, naming) for file 1
        {
            let file = app.file.clone();
            let format = app.format.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            app.pipeline_rx = Some(rx);
            app.pipeline_building = true;
            std::thread::spawn(move || {
                debug_log("[TUI] Pipeline build (file 1) started...");
                let start = Instant::now();
                match PipelineContext::build(&file, &format) {
                    Ok(ctx) => {
                        debug_log(&format!(
                            "[TUI] Pipeline build (file 1) done in {:.2?}",
                            start.elapsed()
                        ));
                        let _ = tx.send(ctx);
                    }
                    Err(e) => {
                        debug_log(&format!("[TUI] Pipeline build (file 1) failed: {e}"));
                    }
                }
            });
        }

        // Spawn background pipeline build for file 2 (diff mode)
        if let (Some(file2), Some(format2)) = (&app.file2, &app.format2) {
            let file2 = file2.clone();
            let format2 = format2.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            app.pipeline_rx2 = Some(rx);
            app.pipeline_building2 = true;
            std::thread::spawn(move || {
                debug_log("[TUI] Pipeline build (file 2) started...");
                let start = Instant::now();
                match PipelineContext::build(&file2, &format2) {
                    Ok(ctx) => {
                        debug_log(&format!(
                            "[TUI] Pipeline build (file 2) done in {:.2?}",
                            start.elapsed()
                        ));
                        let _ = tx.send(ctx);
                    }
                    Err(e) => {
                        debug_log(&format!("[TUI] Pipeline build (file 2) failed: {e}"));
                    }
                }
            });
        }

        debug_log(&format!("[TUI] App::new done in {:.2?}", total_start.elapsed()));
        app
    }

    // calculate_diff_status method removed (moved to module)

    fn build_function_map_local(&self) -> HashMap<String, u32> {
        let mut map = HashMap::new();
        for (i, header) in self.file.function_headers.iter().enumerate() {
            let name = self
                .file
                .string_at(header.function_name())
                .map(|e| e.value.clone())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| format!("f{i}"));
            map.insert(name, i as u32);
        }
        map
    }

    // strip_offsets removed (moved to module)

    fn add_discovered_name(&mut self, name: String) {
        if self.known_names.insert(name.clone()) {
            self.all_function_names.push(name);
            self.all_function_names.sort();
            self.update_search();
        }
    }

    pub fn poll_background_tasks(&mut self) {
        let mut messages = Vec::new();
        let mut disconnected = false;
        const MAX_MESSAGES_PER_TICK: usize = 128;

        if let Some(rx) = self.diff_rx.as_ref() {
            for _ in 0..MAX_MESSAGES_PER_TICK {
                match rx.try_recv() {
                    Ok(msg) => messages.push(msg),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        for msg in messages {
            match msg {
                DiffProgressMsg::Item {
                    name,
                    status,
                    done,
                    total,
                } => {
                    self.diff_status.insert(name.clone(), status);
                    self.diff_progress_done = done;
                    self.diff_progress_total = total;
                    self.add_discovered_name(name);
                }
                DiffProgressMsg::Finished { final_status } => {
                    self.diff_status = final_status;
                    self.diff_analyzing = false;
                    self.diff_progress_done = self.diff_status.len();
                    if self.diff_progress_total == 0 {
                        self.diff_progress_total = self.diff_status.len();
                    }
                    self.all_function_names = self.diff_status.keys().cloned().collect();
                    self.all_function_names.sort();
                    self.known_names = self.all_function_names.iter().cloned().collect();
                    self.update_search();
                    self.diff_rx = None;
                }
            }
        }

        if disconnected {
            self.diff_analyzing = false;
            self.diff_rx = None;
        }

        // Poll pipeline context (file 1)
        if let Some(rx) = self.pipeline_rx.as_ref() {
            match rx.try_recv() {
                Ok(ctx) => {
                    debug_log("[TUI] Pipeline context (file 1) received");
                    self.pipeline_ctx = Some(Arc::new(ctx));
                    self.pipeline_building = false;
                    self.pipeline_rx = None;
                    self.decompile_cache.clear();
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.pipeline_building = false;
                    self.pipeline_rx = None;
                }
            }
        }

        // Poll pipeline context (file 2)
        if let Some(rx) = self.pipeline_rx2.as_ref() {
            match rx.try_recv() {
                Ok(ctx) => {
                    debug_log("[TUI] Pipeline context (file 2) received");
                    self.pipeline_ctx2 = Some(Arc::new(ctx));
                    self.pipeline_building2 = false;
                    self.pipeline_rx2 = None;
                    self.decompile_cache2.clear();
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.pipeline_building2 = false;
                    self.pipeline_rx2 = None;
                }
            }
        }
    }

    pub fn function_list_title(&self) -> String {
        if self.diff_analyzing {
            format!(
                "Functions (analyzing {}/{})",
                self.diff_progress_done, self.diff_progress_total
            )
        } else {
            "Functions".to_string()
        }
    }

    pub fn selected_function_id(&self) -> Option<u32> {
        if self.selected < self.function_names.len() {
            let name = &self.function_names[self.selected];
            self.map1.get(name).copied()
        } else {
            None
        }
    }

    pub fn selected_function_name(&self) -> Option<&str> {
        if self.selected < self.function_names.len() {
            Some(&self.function_names[self.selected])
        } else {
            None
        }
    }

    pub fn set_selected(&mut self, index: usize) {
        if self.selected != index {
            self.selected = index;
            self.list_state.select(Some(index));
            self.scroll = 0;
        }
    }

    pub fn set_view(&mut self, view: ViewMode) {
        if self.view != view {
            // If switching TO Diff, check what we are coming from to set diff_kind
            if view == ViewMode::Diff {
                if self.view == ViewMode::Disasm {
                    self.diff_kind = ViewMode::Disasm;
                } else {
                    self.diff_kind = ViewMode::Decompile;
                }
            }
            self.view = view;
            self.scroll = 0;
        }
    }

    pub fn toggle_diff_kind(&mut self) {
        if self.view == ViewMode::Diff {
            self.diff_kind = match self.diff_kind {
                ViewMode::Disasm => ViewMode::Decompile,
                _ => ViewMode::Disasm,
            };
            self.scroll = 0;
        }
    }

    pub fn content(&mut self) -> (Text<'static>, Option<Text<'static>>) {
        if self.function_names.is_empty() {
            if self.diff_analyzing {
                return (Text::raw("Analyzing functions..."), None);
            }
            return (Text::raw("No functions found matching query."), None);
        }

        match self.view {
            ViewMode::Info => (Text::raw(self.format_info_wrapper()), None),
            ViewMode::Disasm => (self.disasm_content(), None),
            ViewMode::Decompile => (
                Text::from(highlight_code(&self.decompile_content())),
                None,
            ),
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

    // Fast search: only matches function names (called on every keystroke).
    pub fn update_search(&mut self) {
        if self.search_query.is_empty() {
            self.function_names = self.all_function_names.clone();
        } else {
            let query = self.search_query.to_lowercase();
            self.function_names = self
                .all_function_names
                .iter()
                .filter(|name| name.to_lowercase().contains(&query))
                .cloned()
                .collect();
        }
        if self.selected >= self.function_names.len() {
            self.selected = 0;
            self.list_state.select(Some(0));
        }
        self.scroll = 0;
    }

    // Deep search: also scans string literals in bytecode (called on Enter).
    pub fn deep_search(&mut self) {
        if self.search_query.is_empty() {
            return;
        }

        let string_xrefs = hbc_decomp::analysis::find_string_xrefs(
            &self.file,
            &self.format,
            &self.search_query,
        );

        if !string_xrefs.is_empty() {
            let id_to_name: HashMap<u32, &String> =
                self.map1.iter().map(|(name, &id)| (id, name)).collect();
            let existing: HashSet<String> = self.function_names.iter().cloned().collect();

            for xref in string_xrefs {
                if let Some(name) = id_to_name.get(&xref.function_id) {
                    if self.known_names.contains(*name) && !existing.contains(*name) {
                        self.function_names.push((*name).clone());
                    }
                }
            }

            self.function_names.sort();
            self.function_names.dedup();
        }

        if self.selected >= self.function_names.len() {
            self.selected = 0;
            self.list_state.select(Some(0));
        }
    }
}
