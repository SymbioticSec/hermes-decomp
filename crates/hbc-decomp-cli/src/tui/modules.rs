// Metro module list state for the TUI (ViewMode::Modules).

use hbc_decomp::PipelineContext;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ModuleRow {
    pub module_id: u32,
    pub function_id: u32,
    pub name: String,
    pub export_count: usize,
    pub dep_count: usize,
}

#[derive(Debug, Default)]
pub struct ModulesState {
    pub rows: Vec<ModuleRow>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
    pub query: String,
    /// Lazy cache of decompiled module source.
    pub cache: HashMap<u32, String>,
}

impl ModulesState {
    pub fn rebuild_from(&mut self, ctx: &PipelineContext) {
        let mut rows: Vec<ModuleRow> = ctx
            .registry
            .modules
            .values()
            .map(|m| ModuleRow {
                module_id: m.module_id,
                function_id: m.function_id,
                name: m
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("module_{}", m.module_id)),
                export_count: m.exports.len(),
                dep_count: m.dependencies.len(),
            })
            .collect();
        rows.sort_by_key(|r| r.module_id);
        self.rows = rows;
        self.query.clear();
        self.selected = 0;
        self.scroll = 0;
        self.refilter();
    }

    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                if q.is_empty() {
                    return true;
                }
                r.name.to_lowercase().contains(&q)
                    || r.module_id.to_string().contains(&q)
                    || r.function_id.to_string().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub fn selected_row(&self) -> Option<&ModuleRow> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.rows.get(i))
    }

    pub fn move_sel(&mut self, delta: isize, page: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as isize;
        let cur = self.selected as isize;
        let next = (cur + delta).clamp(0, len - 1) as usize;
        self.selected = next;
        // Keep selection in view
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + page {
            self.scroll = self.selected + 1 - page;
        }
    }

    pub fn label(&self, idx_in_filtered: usize) -> String {
        let Some(&row_i) = self.filtered.get(idx_in_filtered) else {
            return String::new();
        };
        let r = &self.rows[row_i];
        format!(
            "{:>5}  {:<40}  exp:{:<3} dep:{}",
            r.module_id,
            truncate(&r.name, 40),
            r.export_count,
            r.dep_count
        )
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        format!("{s:<max$}")
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
