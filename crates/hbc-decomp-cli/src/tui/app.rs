use ratatui::text::Text;
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc::Receiver, Arc};
use std::time::Instant;

use hbc_decomp::{BytecodeFile, BytecodeFormat, ClosureContext, PipelineContext};

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
    pub(crate) known_names: HashSet<String>,

    // Mode used for diff calculation (Assembly or Code)
    pub diff_calc_mode: DiffMode,

    // Toggle for content diff highlighting
    pub show_diff_colors: bool,

    // Toggle for the unified (git-style, single column) diff view.
    pub diff_unified: bool,

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
            diff_unified: false,
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

    pub(crate) fn add_discovered_name(&mut self, name: String) {
        if self.known_names.insert(name.clone()) {
            self.all_function_names.push(name);
            self.all_function_names.sort();
            self.update_search();
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

    /// File-2 id for the selected function, resolving renames. Lets us show the
    /// code of functions that exist only in file 2 ("added"), which have no
    /// file-1 id.
    pub fn selected_function_id2(&self) -> Option<u32> {
        let name = self.selected_function_name()?;
        if let Some(id) = self.map2.get(name) {
            return Some(*id);
        }
        if let Some(DiffStatus::Renamed(new_name)) = self.diff_status.get(name) {
            return self.map2.get(new_name).copied();
        }
        None
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
