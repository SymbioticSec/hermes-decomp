mod body_hints;
mod graph;
mod hints_tables;
mod inference;
mod property_accesses;
pub(crate) mod resolution;
mod structs;
pub(crate) mod traversal;

use std::collections::HashSet;
use crate::ir::Statement;
use graph::CallGraph;
use inference::{is_generic_name, vote_on_names};
use std::collections::BTreeMap;

/// Upper bound on parameter-slot vectors. Parameter indices come from parsed
/// IR; a corrupt index could otherwise drive `vec![None; idx + 1]` / `resize`
/// to allocate gigabytes and abort the process. No real function approaches
/// this many parameters.
pub(crate) const MAX_PARAM_SLOTS: usize = 1 << 16;
pub use structs::GlobalAnalysis;
pub use resolution::FunctionNameIndex;

use super::metro::registry::MetroRegistry;

// Maximum iterations for fixed-point propagation via param_links.
// Convergence typically occurs in 2-5 iterations; 20 guarantees termination.
const MAX_PARAM_LINK_ITERATIONS: usize = 20;

pub fn run_ipa(
    functions: &BTreeMap<u32, Vec<Statement>>,
    metro_registry: &MetroRegistry,
    func_name_index: &FunctionNameIndex,
) -> GlobalAnalysis {
    let mut analysis = GlobalAnalysis {
        param_names: BTreeMap::new(),
        param_links: Vec::new(),
        graph: CallGraph::new(),
        dead_code: HashSet::new(),
    };
    let mut call_sites: BTreeMap<u32, Vec<Vec<Option<String>>>> = BTreeMap::new();
    let mut self_param_names: BTreeMap<u32, Vec<Vec<Option<String>>>> = BTreeMap::new();

    // Pass 1: Collect initial structural names and links
    let mut collect_ctx = traversal::CollectContext {
        graph: &mut analysis.graph,
        call_sites: &mut call_sites,
        self_param_names: &mut self_param_names,
        param_links: &mut analysis.param_links,
        metro_registry,
        func_name_index,
    };
    for (&func_id, stmts) in functions {
        traversal::collect_info(func_id, stmts, &mut collect_ctx);
    }
    // Explicitly end borrow of `analysis` fields through `collect_ctx`
    let _ = collect_ctx;

    // Pass 1b: Infer parameter names from body usage patterns
    for (&func_id, stmts) in functions {
        let body_hints = body_hints::infer_param_names_from_body(stmts);
        if !body_hints.is_empty() {
            let max_idx =
                (body_hints.iter().map(|(idx, _)| *idx).max().unwrap_or(0) as usize)
                    .min(MAX_PARAM_SLOTS);
            let mut site = vec![None; max_idx + 1];
            for (idx, name) in body_hints {
                if site.get(idx as usize).is_none_or(|s| s.is_none())
                    && idx < site.len() as u32 {
                        site[idx as usize] = Some(name);
                    }
            }
            self_param_names.entry(func_id).or_default().push(site);
        }
    }

    // Initial vote based on bodies and call names
    for (&func_id, sites) in &call_sites {
        analysis
            .param_names
            .insert(func_id, vote_on_names(sites.clone()));
    }
    for (func_id, sites) in self_param_names {
        let structural = vote_on_names(sites);
        let existing = analysis
            .param_names
            .entry(func_id)
            .or_insert_with(|| vec![None; structural.len()]);
        for (i, name) in structural.into_iter().enumerate() {
            if i < existing.len() && existing[i].is_none() {
                existing[i] = name;
            }
        }
    }

    // Index links by src and dst for faster lookup
    let mut links_by_src: BTreeMap<u32, Vec<structs::ParamLink>> = BTreeMap::new();
    let mut links_by_dst: BTreeMap<u32, Vec<structs::ParamLink>> = BTreeMap::new();

    for link in &analysis.param_links {
        links_by_src.entry(link.src_func).or_default().push(*link);
        links_by_dst.entry(link.dst_func).or_default().push(*link);
    }

    let post_order = analysis.graph.compute_post_order();
    let mut top_order = post_order.clone();
    top_order.reverse();

    // Pass 2: Propagate names across links using Topological Order
    // Iteration 1: Top-Down (Caller -> Callee)
    for &func_id in &top_order {
        let src_names = analysis
            .param_names
            .get(&func_id)
            .cloned()
            .unwrap_or_default();

        if let Some(links) = links_by_src.get(&func_id) {
            for link in links {
                if link.dst_param as usize >= MAX_PARAM_SLOTS {
                    continue;
                }
                if let Some(Some(name)) = src_names.get(link.src_param as usize) {
                    let entry = analysis.param_names.entry(link.dst_func).or_default();
                    if entry.len() <= link.dst_param as usize {
                        entry.resize(link.dst_param as usize + 1, None);
                    }

                    if entry[link.dst_param as usize].is_none() && !is_generic_name(name) {
                        entry[link.dst_param as usize] = Some(name.clone());
                    }
                }
            }
        }
    }

    // Iteration 2: Bottom-Up (Callee -> Caller)
    for &func_id in &post_order {
        let dst_names = analysis
            .param_names
            .get(&func_id)
            .cloned()
            .unwrap_or_default();

        if let Some(links) = links_by_dst.get(&func_id) {
            for link in links {
                if link.src_param as usize >= MAX_PARAM_SLOTS {
                    continue;
                }
                if let Some(Some(name)) = dst_names.get(link.dst_param as usize) {
                    let entry = analysis.param_names.entry(link.src_func).or_default();
                    if entry.len() <= link.src_param as usize {
                        entry.resize(link.src_param as usize + 1, None);
                    }

                    if entry[link.src_param as usize].is_none() && !is_generic_name(name) {
                        entry[link.src_param as usize] = Some(name.clone());
                    }
                }
            }
        }
    }

    // Iteration 3: Final Sweep (Fixed-point propagation via param_links)
    // Propagates names across function boundaries via param_links until no changes occur.
    for _ in 0..MAX_PARAM_LINK_ITERATIONS {
        let mut changes = false;
        let mut updates = Vec::new();

        for link in &analysis.param_links {
            if let Some(src_names) = analysis.param_names.get(&link.src_func) {
                if let Some(Some(name)) = src_names.get(link.src_param as usize) {
                    if !is_generic_name(name) {
                        updates.push((link.dst_func, link.dst_param as usize, name.clone()));
                    }
                }
            }

            if let Some(dst_names) = analysis.param_names.get(&link.dst_func) {
                if let Some(Some(name)) = dst_names.get(link.dst_param as usize) {
                    if !is_generic_name(name) {
                        updates.push((link.src_func, link.src_param as usize, name.clone()));
                    }
                }
            }
        }

        for (id, idx, name) in updates {
            let entry = analysis.param_names.entry(id).or_default();
            if entry.len() <= idx {
                entry.resize(idx + 1, None);
            }

            let current_is_none = entry[idx].is_none();
            if current_is_none {
                entry[idx] = Some(name);
                changes = true;
            }
        }

        if !changes {
            break;
        }
    }

    // Detect Dead Code
    let mut roots = Vec::new();
    for module in metro_registry.modules.values() {
        roots.push(module.function_id);
    }
    if !roots.contains(&0) && functions.contains_key(&0) {
        roots.push(0);
    }

    let reachable = analysis.graph.get_reachable_functions(&roots);
    let all_funcs: HashSet<u32> = functions.keys().cloned().collect();
    analysis.dead_code = all_funcs.difference(&reachable).cloned().collect();

    analysis
}
