use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc::Receiver, Arc};
use std::time::Instant;

use hbc_decomp::{BytecodeFile, BytecodeFormat, PipelineContext};

use super::debug_log;
use super::gitdiff::{self, GitDiffJob, GitMsg, GitRow};
use super::disasm_or_log;

// Build the analysis pipeline, reusing the on-disk cache (`<path>.hdcache`) when
// possible. The cache key needs the raw file bytes, which the App doesn't keep,
// so we re-read them from `path` (the same bytes the other commands hash, so the
// cache is shared). An empty or unreadable path falls back to an uncached build.
fn build_pipeline_cached(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    path: &str,
) -> hbc_decomp::error::Result<PipelineContext> {
    if !path.is_empty() {
        if let Ok(bytes) = std::fs::read(path) {
            let cache_path = hbc_decomp::default_cache_path(std::path::Path::new(path));
            return PipelineContext::build_cached(
                file,
                format,
                &hbc_decomp::DecompileOptionsV2::optimized(),
                &bytes,
                &cache_path,
            );
        }
    }
    PipelineContext::build(file, format)
}

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

    // Full-program "git" diff mode: hides the function list and shows all code
    // of file 1 (left) vs file 2 (right), aligned. Computed off-thread.
    pub git_diff: bool,
    pub git_rows: Arc<Vec<GitRow>>,
    pub git_rx: Option<Receiver<GitMsg>>,
    pub git_computing: bool,
    pub git_progress: (usize, usize),
    pub git_built_kind: Option<ViewMode>,
    // Git-diff content kind. Defaults to Disasm: it needs no decompiler pipeline
    // so it shows instantly; `v` switches to Decompile (which waits for the
    // pipeline). Independent from the split-view `diff_kind`.
    pub git_kind: ViewMode,
    // Ignore volatile Metro ids (module_955 vs module_769) in the git diff.
    pub git_normalize: bool,
    // In-view search for the git diff.
    pub git_search: String,
    pub git_searching: bool,
    pub git_match_count: usize,
    // 1-based index of the match the view is currently on (0 = none).
    pub git_match_index: usize,
    // Syntax-highlight the code in the git diff (vs. plain diff tint).
    pub git_syntax: bool,
    // Collapsed functions (by their header index in git_rows).
    pub git_folded: std::collections::HashSet<usize>,
    // Display order: indices into git_rows that are currently visible (respects
    // folds). `scroll` indexes into this, not git_rows directly.
    pub git_visible: Vec<usize>,
    // Inner area of the git columns (set during draw) for click-to-fold.
    pub git_view_top: u16,
    pub git_view_height: u16,
    // Frame counter, advanced each loop, for animating the loading spinner.
    pub tick: usize,

    // Persistent list state for function list scrolling
    pub list_state: ListState,
    // Inner area of the function list (set during draw), for click-to-select.
    pub list_inner: Rect,

    // Content search state (search within selected function's content)
    pub content_search: String,
    pub is_content_searching: bool,
    pub content_search_matches: Vec<(usize, usize)>, // (line_idx, char_idx) pairs
    pub content_search_index: usize, // Current match index (0-based)

    // Full pipeline context (IPA, Metro, naming) — built in background
    pub pipeline_ctx: Option<Arc<PipelineContext>>,
    pub pipeline_rx: Option<Receiver<PipelineContext>>,
    pub pipeline_building: bool,

    // Pipeline context for file 2 (diff mode)
    pub pipeline_ctx2: Option<Arc<PipelineContext>>,
    pub pipeline_rx2: Option<Receiver<PipelineContext>>,
    pub pipeline_building2: bool,

    // Text selection (terminal cell coordinates, content pane only)
    pub selection_anchor: Option<(u16, u16)>,
    pub selection_target: Option<(u16, u16)>,
    pub selecting: bool,
    pub content_inner: Rect,
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
            git_diff: false,
            git_rows: Arc::new(Vec::new()),
            git_rx: None,
            git_computing: false,
            git_progress: (0, 0),
            git_built_kind: None,
            // Decompiled by default — it streams per function now, so there's
            // no opaque wait; `v` toggles to disassembly.
            git_kind: ViewMode::Decompile,
            git_normalize: true,
            git_search: String::new(),
            git_searching: false,
            git_match_count: 0,
            git_match_index: 0,
            git_syntax: false,
            git_folded: std::collections::HashSet::new(),
            git_visible: Vec::new(),
            git_view_top: 0,
            git_view_height: 0,
            tick: 0,
            list_state: ListState::default().with_selected(Some(0)),
            list_inner: Rect::default(),
            content_search: String::new(),
            is_content_searching: false,
            content_search_matches: Vec::new(),
            content_search_index: 0,
            pipeline_ctx: None,
            pipeline_rx: None,
            pipeline_building: false,
            pipeline_ctx2: None,
            pipeline_rx2: None,
            pipeline_building2: false,
            selection_anchor: None,
            selection_target: None,
            selecting: false,
            content_inner: Rect::default(),
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

        // The decompiler pipeline (IPA, Metro, naming) is built lazily — it
        // takes several seconds per file and would otherwise saturate all CPU
        // cores at startup, starving the (instant) disassembly views and making
        // the UI lag. It's kicked off the first time decompiled output is asked
        // for (Decompile view, or the git diff's `v`).
        debug_log(&format!("[TUI] App::new done in {:.2?}", total_start.elapsed()));
        app
    }

    // Start building the decompiler pipeline context(s) in the background, if
    // not already built or in progress. Idempotent.
    pub fn ensure_pipeline_building(&mut self) {
        if self.pipeline_ctx.is_none() && !self.pipeline_building {
            let file = self.file.clone();
            let format = self.format.clone();
            let path = self.path.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.pipeline_rx = Some(rx);
            self.pipeline_building = true;
            std::thread::spawn(move || match build_pipeline_cached(&file, &format, &path) {
                Ok(ctx) => {
                    let _ = tx.send(ctx);
                }
                Err(e) => debug_log(&format!("[pipeline] file 1 build failed: {e}")),
            });
        }
        if let (Some(file2), Some(format2)) = (self.file2.clone(), self.format2.clone()) {
            if self.pipeline_ctx2.is_none() && !self.pipeline_building2 {
                let path2 = self.path2.clone().unwrap_or_default();
                let (tx, rx) = std::sync::mpsc::channel();
                self.pipeline_rx2 = Some(rx);
                self.pipeline_building2 = true;
                std::thread::spawn(move || match build_pipeline_cached(&file2, &format2, &path2) {
                    Ok(ctx) => {
                        let _ = tx.send(ctx);
                    }
                    Err(e) => debug_log(&format!("[pipeline] file 2 build failed: {e}")),
                });
            }
        }
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

    // File-2 id for the selected function, resolving renames. Lets us show the
    // code of functions that exist only in file 2 ("added"), which have no
    // file-1 id.
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

    // Select the function clicked at the given terminal cell, if it falls
    // inside the function list.
    pub fn select_at_row(&mut self, col: u16, row: u16) {
        let a = self.list_inner;
        let inside = col >= a.x && col < a.x + a.width && row >= a.y && row < a.y + a.height;
        if !inside {
            return;
        }
        let idx = self.list_state.offset() + (row - a.y) as usize;
        if idx < self.function_names.len() {
            self.set_selected(idx);
        }
    }

    pub fn is_inside_content(&self, col: u16, row: u16) -> bool {
        let r = self.content_inner;
        col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
    }

    pub fn normalized_selection(&self) -> Option<(u16, u16, u16, u16)> {
        let (ac, ar) = self.selection_anchor?;
        let (tc, tr) = self.selection_target?;
        let (sc, sr, ec, er) = if (ar, ac) <= (tr, tc) {
            (ac, ar, tc, tr)
        } else {
            (tc, tr, ac, ar)
        };
        Some((sc, sr, ec, er))
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_target = None;
        self.selecting = false;
    }

    /// Push the current selection to the terminal clipboard via OSC 52 so
    /// that Cmd+C / Ctrl+Shift+C copies it.  No-op when there is no
    /// selection or the extracted text is empty.
    pub fn copy_selection_to_clipboard(&mut self) {
        use crossterm::clipboard::CopyToClipboard;
        use crossterm::execute;

        let Some((sel_sc, sel_sr, sel_ec, sel_er)) = self.normalized_selection() else {
            return;
        };

        let (content, _) = self.content();
        let inner = self.content_inner;
        let scroll = self.scroll as u16;

        let mut result = String::new();
        for vi in 0..inner.height {
            let term_row = inner.y + vi;
            let line_idx = vi + scroll;
            if term_row < sel_sr || term_row > sel_er || line_idx as usize >= content.lines.len() {
                if term_row > sel_er {
                    break;
                }
                continue;
            }

            let line = &content.lines[line_idx as usize];
            let line_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

            let col_start = if term_row == sel_sr {
                sel_sc.saturating_sub(inner.x) as usize
            } else {
                0
            };
            let col_end = if term_row == sel_er {
                ((sel_ec + 1).min(inner.x + inner.width) - inner.x) as usize
            } else {
                line_text.len()
            };
            let col_start = col_start.min(line_text.len());
            let col_end = col_end.min(line_text.len());
            if col_start < col_end {
                result.push_str(&line_text[col_start..col_end]);
            }
            if term_row < sel_er {
                result.push('\n');
            }
        }

        if result.is_empty() {
            return;
        }

        let _ = execute!(
            std::io::stdout(),
            CopyToClipboard::to_clipboard_from(result)
        );
    }

    pub fn set_selected(&mut self, index: usize) {
        if self.selected != index {
            self.selected = index;
            self.list_state.select(Some(index));
            self.scroll = 0;
            self.clear_selection();
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
            self.clear_selection();
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

    // Drop the cached git-diff rows (e.g. after switching disasm/decompile),
    // so the next `request_git_diff` recomputes.
    pub fn invalidate_git_diff(&mut self) {
        self.git_rows = Arc::new(Vec::new());
        self.git_built_kind = None;
        self.git_rx = None;
        self.git_computing = false;
        self.git_progress = (0, 0);
        self.git_folded.clear();
        self.git_visible.clear();
    }

    // Recompute which git_rows are visible given the current fold state. Folded
    // functions show only their header; their body rows are hidden.
    pub fn rebuild_git_visible(&mut self) {
        let mut visible = Vec::with_capacity(self.git_rows.len());
        let mut hiding = false;
        for (i, row) in self.git_rows.iter().enumerate() {
            match row {
                GitRow::Header(_) => {
                    hiding = self.git_folded.contains(&i);
                    visible.push(i);
                }
                _ => {
                    if !hiding {
                        visible.push(i);
                    }
                }
            }
        }
        self.git_visible = visible;
    }

    // Toggle the fold of the function whose header is at the given display row
    // (terminal cell). No-op if the click isn't on a header.
    pub fn git_toggle_fold_at(&mut self, row: u16) {
        if row < self.git_view_top {
            return;
        }
        let pos = self.scroll as usize + (row - self.git_view_top) as usize;
        let Some(&ri) = self.git_visible.get(pos) else {
            return;
        };
        if matches!(self.git_rows.get(ri), Some(GitRow::Header(_))) {
            if !self.git_folded.remove(&ri) {
                self.git_folded.insert(ri);
            }
            self.rebuild_git_visible();
        }
    }

    // Kick off the background build of the full-program git diff if it is
    // enabled and not already built/building for the current kind. Both disasm
    // and decompiled modes stream per function off-thread, so neither waits for
    // the full PipelineContext to build.
    // Safe to call every tick — it early-returns when there's nothing to do.
    pub fn request_git_diff(&mut self) {
        if !self.git_diff || self.git_computing || self.git_built_kind == Some(self.git_kind) {
            return;
        }
        let (Some(file2), Some(format2)) = (self.file2.clone(), self.format2.clone()) else {
            return;
        };

        // Build the full A->Z list directly from both files' function maps
        // (always complete from startup), not from the diff worker's
        // incrementally-populated list. file 1 functions first, then file-2-only
        // (added) ones, both sorted by name.
        let mut names: Vec<String> = self.map1.keys().cloned().collect();
        names.sort();
        let mut added: Vec<String> = self
            .map2
            .keys()
            .filter(|n| !self.map1.contains_key(*n))
            .cloned()
            .collect();
        added.sort();
        names.extend(added);

        let total = names.len();
        let job = GitDiffJob {
            names,
            map1: self.map1.clone(),
            map2: self.map2.clone(),
            diff_status: self.diff_status.clone(),
            file1: Arc::clone(&self.file),
            file2,
            format1: Arc::clone(&self.format),
            format2,
            kind: self.git_kind,
            normalize: self.git_normalize,
        };
        self.git_rows = Arc::new(Vec::new());
        self.git_visible.clear();
        self.git_folded.clear();
        self.git_progress = (0, total);
        self.git_rx = Some(gitdiff::spawn(job));
        self.git_computing = true;
    }

    // Scroll to the next git-diff row matching `git_search` (case-insensitive,
    // wrapping). No-op if the query is empty or there are no rows.
    // Incremental search (used while typing): jump to the first match at or
    // after the current position.
    pub fn git_search_live(&mut self) {
        self.git_search_jump(true, true);
    }

    pub fn git_search_next(&mut self) {
        self.git_search_jump(true, false);
    }

    pub fn git_search_prev(&mut self) {
        self.git_search_jump(false, false);
    }

    // Find the next/previous visible row matching `git_search` (wrapping),
    // update scroll, the total match count and the 1-based current index.
    // `inclusive` lets incremental typing match the current row in place.
    fn git_search_jump(&mut self, forward: bool, inclusive: bool) {
        let query = self.git_search.to_lowercase();
        let n = self.git_visible.len();
        self.git_match_count = if query.is_empty() {
            0
        } else {
            self.git_visible
                .iter()
                .filter(|&&ri| gitdiff::row_contains(&self.git_rows[ri], &query))
                .count()
        };
        if self.git_match_count == 0 {
            self.git_match_index = 0;
            return;
        }
        let cur = (self.scroll as usize).min(n - 1);
        let first_off = usize::from(!inclusive);
        for off in first_off..=n {
            let pos = if forward {
                (cur + off) % n
            } else {
                (cur + n - off) % n
            };
            if gitdiff::row_contains(&self.git_rows[self.git_visible[pos]], &query) {
                self.scroll = pos as u32;
                self.git_match_index = self.git_visible[..=pos]
                    .iter()
                    .filter(|&&ri| gitdiff::row_contains(&self.git_rows[ri], &query))
                    .count();
                return;
            }
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

    pub fn update_content_search(&mut self) {
        self.content_search_matches.clear();
        self.content_search_index = 0;

        if self.content_search.is_empty() {
            return;
        }

        let content_text = self.get_content_text();
        let query = self.content_search.to_lowercase();

        for (line_idx, line) in content_text.lines().enumerate() {
            let line_lower = line.to_lowercase();
            let mut char_idx = 0;
            while let Some(pos) = line_lower[char_idx..].find(&query) {
                self.content_search_matches
                    .push((line_idx, char_idx + pos));
                char_idx += pos + 1;
            }
        }

        if !self.content_search_matches.is_empty() {
            self.content_search_jump_to(0);
        }
    }

    pub fn content_search_next(&mut self) {
        if self.content_search_matches.is_empty() {
            return;
        }
        let next = (self.content_search_index + 1) % self.content_search_matches.len();
        self.content_search_jump_to(next);
    }

    pub fn content_search_prev(&mut self) {
        if self.content_search_matches.is_empty() {
            return;
        }
        let prev = if self.content_search_index == 0 {
            self.content_search_matches.len() - 1
        } else {
            self.content_search_index - 1
        };
        self.content_search_jump_to(prev);
    }

    fn content_search_jump_to(&mut self, match_idx: usize) {
        self.content_search_index = match_idx;
        if let Some(&(line_idx, _)) = self.content_search_matches.get(match_idx) {
            self.scroll = line_idx as u32;
        }
    }

    fn get_content_text(&mut self) -> String {
        match self.view {
            ViewMode::Disasm => {
                if let Some(id) = self.selected_function_id() {
                    disasm_or_log(&self.file, &self.format, id)
                } else {
                    String::new()
                }
            }
            ViewMode::Decompile => self.decompile_content(),
            ViewMode::Info => self.format_info_wrapper(),
            ViewMode::Diff => {
                let (left, _) = self.content();
                left.lines
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
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
