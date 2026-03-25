use super::closure_inference::infer_name_from_closure_usage;
use super::closure_usage::{
    collect_closure_usage_in_stmt, is_closure_name, ClosureUsageInfo,
};
use crate::analysis::ClosureContext;
use crate::ir::{AssignTarget, Expression, Statement, Value};
use std::collections::BTreeMap;

// Cross-function closure naming: aggregates usage of `closure_N` across sibling functions
// (children of the same parent) to infer one consistent name per parent slot.
//
// After closure resolution, `closure_N` in function X refers to slot N of X's parent.
// If sibling functions A, B, C all reference `closure_3.setToken()`, we infer "authStore"
// once and apply it everywhere — no `authStore`/`authStore2` inconsistency.
//
// Returns the number of closure variables renamed.
pub fn rename_closure_variables_cross_function(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &ClosureContext,
) -> usize {
    use std::collections::HashSet;

    // Phase 1: Collect usage per (parent_id, slot) across ALL functions
    // Key = (parent_function_id, slot_number)
    let mut slot_usage: BTreeMap<(u32, u32), ClosureUsageInfo> = BTreeMap::new();
    // Track which (func_id, closure_name) maps to which (parent_id, slot)
    let mut func_closure_to_slot: Vec<(u32, String, u32, u32)> = Vec::new(); // (func_id, closure_name, parent_id, slot)

    let mut all_ir_keys: Vec<_> = all_ir.keys().copied().collect();
    all_ir_keys.sort();
    for func_id in all_ir_keys {
        let stmts = &all_ir[&func_id];
        // Collect usage in this function
        let mut usage_map: BTreeMap<String, ClosureUsageInfo> = BTreeMap::new();
        for stmt in stmts.iter() {
            collect_closure_usage_in_stmt(stmt, &mut usage_map);
        }

        // Map each closure_N to its parent slot
        let parent_id = closure_ctx.parent_function.get(&func_id).copied();

        let mut usage_items: Vec<_> = usage_map.into_iter().collect();
        usage_items.sort_by(|a, b| a.0.cmp(&b.0));
        for (closure_name, info) in usage_items {
            if let Some(slot) = parse_closure_slot(&closure_name) {
                // Determine the parent: for level-0 slots, it's the direct parent
                let pid = if slot < (1 << 24) {
                    // Simple slot (level 0) → parent
                    parent_id
                } else {
                    // Multi-level slot — walk up
                    let level = (slot >> 24) as usize;
                    let mut current = func_id;
                    let mut found = None;
                    for _ in 0..=level {
                        if let Some(&p) = closure_ctx.parent_function.get(&current) {
                            current = p;
                            found = Some(current);
                        } else {
                            break;
                        }
                    }
                    found
                };

                if let Some(pid) = pid {
                    let actual_slot = slot & 0xFFFFFF; // Extract raw slot for multi-level
                    let key = (pid, actual_slot);
                    func_closure_to_slot.push((func_id, closure_name.clone(), pid, actual_slot));

                    // Merge usage into the aggregated slot info
                    let agg = slot_usage.entry(key).or_default();
                    agg.properties.extend(info.properties);
                    agg.methods.extend(info.methods);
                    if info.called_as_function {
                        agg.called_as_function = true;
                    }
                }
            }
        }
    }

    if slot_usage.is_empty() {
        return 0;
    }

    // Phase 2: Infer one name per (parent_id, slot) from aggregated usage
    let mut slot_names: BTreeMap<(u32, u32), String> = BTreeMap::new();
    // Track used names PER PARENT to avoid collisions only among siblings
    let mut per_parent_used: BTreeMap<u32, HashSet<String>> = BTreeMap::new();

    let mut slot_usage_keys: Vec<_> = slot_usage.keys().copied().collect();
    slot_usage_keys.sort();
    for key in &slot_usage_keys {
        let info = &slot_usage[key];
        let (parent_id, slot) = *key;
        // Try to get a name hint from ClosureContext (what value was stored in this slot)
        let slot_hint = closure_ctx.function_closures.get(&parent_id)
            .and_then(|ci| ci.slots.get(&slot))
            .map(|sv| match sv {
                crate::analysis::ClosureSlotValue::Variable(v) => v.as_str(),
                crate::analysis::ClosureSlotValue::Function { name: Some(n), .. } => n.as_str(),
                crate::analysis::ClosureSlotValue::Constant(c) => c.as_str(),
                _ => "",
            })
            .filter(|s| !s.is_empty());
        if let Some(inferred) = infer_name_from_closure_usage(info, slot_hint) {
            let used = per_parent_used.entry(parent_id).or_default();
            let unique = make_unique_name(&inferred, used);
            slot_names.insert(*key, unique);
        }
    }

    if slot_names.is_empty() {
        return 0;
    }

    // Phase 3: Build per-function rename maps and apply
    // Group the func_closure_to_slot entries by func_id
    let mut per_func_renames: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for (func_id, closure_name, parent_id, slot) in &func_closure_to_slot {
        if let Some(new_name) = slot_names.get(&(*parent_id, *slot)) {
            per_func_renames
                .entry(*func_id)
                .or_default()
                .insert(closure_name.clone(), new_name.clone());
        }
    }

    let mut total_renamed = 0;
    let mut rename_keys: Vec<_> = per_func_renames.keys().copied().collect();
    rename_keys.sort();
    for func_id in &rename_keys {
        let renames = &per_func_renames[func_id];
        if let Some(stmts) = all_ir.get_mut(func_id) {
            total_renamed += renames.len();
            crate::analysis::naming::rename_variables_in_stmts(stmts, renames);
        }
    }

    total_renamed
}

// Parse a closure variable name into its slot number.
// "closure_5" → Some(5), "closure_16777221" → Some(16777221)
fn parse_closure_slot(name: &str) -> Option<u32> {
    name.strip_prefix("closure_")?.parse::<u32>().ok()
}

// Single-function fallback (for functions without a known parent in ClosureContext).
pub fn rename_closure_variables(stmts: &mut [Statement]) -> usize {
    let mut usage_map: BTreeMap<String, ClosureUsageInfo> = BTreeMap::new();
    for stmt in stmts.iter() {
        collect_closure_usage_in_stmt(stmt, &mut usage_map);
    }

    if usage_map.is_empty() {
        return 0;
    }

    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_existing_names(stmts, &mut used_names);

    for (closure_name, info) in &usage_map {
        if let Some(inferred) = infer_name_from_closure_usage(info, None) {
            let unique = make_unique_name(&inferred, &mut used_names);
            renames.insert(closure_name.clone(), unique);
        }
    }

    if renames.is_empty() {
        return 0;
    }

    let count = renames.len();
    crate::analysis::naming::rename_variables_in_stmts(stmts, &renames);
    count
}

// Collect all non-closure variable names already in use in the statement tree.
pub(super) fn collect_existing_names(stmts: &[Statement], names: &mut std::collections::HashSet<String>) {
    for stmt in stmts {
        collect_names_in_stmt(stmt, names);
    }
}

fn collect_names_in_stmt(stmt: &Statement, names: &mut std::collections::HashSet<String>) {
    match stmt {
        Statement::Assign { target, value } => {
            if let AssignTarget::Variable(v) = target {
                if !is_closure_name(v) {
                    names.insert(v.clone());
                }
            }
            collect_names_in_expr(value, names);
        }
        Statement::Let { name, value, .. } => {
            if !is_closure_name(name) {
                names.insert(name.clone());
            }
            collect_names_in_expr(value, names);
        }
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_names_in_expr(e, names);
        }
        Statement::If { condition, then_body, else_body } => {
            collect_names_in_expr(condition, names);
            for s in then_body { collect_names_in_stmt(s, names); }
            for s in else_body { collect_names_in_stmt(s, names); }
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            collect_names_in_expr(condition, names);
            for s in body { collect_names_in_stmt(s, names); }
        }
        Statement::For { init, condition, update, body } => {
            if let Some(s) = init { collect_names_in_stmt(s, names); }
            if let Some(e) = condition { collect_names_in_expr(e, names); }
            if let Some(s) = update { collect_names_in_stmt(s, names); }
            for s in body { collect_names_in_stmt(s, names); }
        }
        Statement::ForOf { variable, iterable, body } => {
            names.insert(variable.clone());
            collect_names_in_expr(iterable, names);
            for s in body { collect_names_in_stmt(s, names); }
        }
        Statement::ForIn { variable, object, body } => {
            names.insert(variable.clone());
            collect_names_in_expr(object, names);
            for s in body { collect_names_in_stmt(s, names); }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body { collect_names_in_stmt(s, names); }
            for s in catch_body { collect_names_in_stmt(s, names); }
            for s in finally_body { collect_names_in_stmt(s, names); }
        }
        Statement::Switch { discriminant, cases, default } => {
            collect_names_in_expr(discriminant, names);
            for (e, stmts) in cases {
                collect_names_in_expr(e, names);
                for s in stmts { collect_names_in_stmt(s, names); }
            }
            if let Some(stmts) = default {
                for s in stmts { collect_names_in_stmt(s, names); }
            }
        }
        Statement::Block(stmts) => {
            for s in stmts { collect_names_in_stmt(s, names); }
        }
        _ => {}
    }
}

fn collect_names_in_expr(expr: &Expression, names: &mut std::collections::HashSet<String>) {
    match expr {
        Expression::Value(Value::Variable(v)) => {
            if !is_closure_name(v) {
                names.insert(v.clone());
            }
        }
        Expression::Call { callee, arguments } => {
            collect_names_in_expr(callee, names);
            for a in arguments { collect_names_in_expr(a, names); }
        }
        Expression::Member { object, .. } => {
            collect_names_in_expr(object, names);
        }
        Expression::Binary { left, right, .. } => {
            collect_names_in_expr(left, names);
            collect_names_in_expr(right, names);
        }
        Expression::Unary { operand, .. } => {
            collect_names_in_expr(operand, names);
        }
        Expression::Assignment { target, value } => {
            collect_names_in_expr(target, names);
            collect_names_in_expr(value, names);
        }
        _ => {}
    }
}

// Generate a unique name that doesn't collide with existing names.
pub(super) fn make_unique_name(base: &str, used: &mut std::collections::HashSet<String>) -> String {
    let sanitized = super::suggestions::sanitize_name(base);
    if used.insert(sanitized.clone()) {
        return sanitized;
    }
    let mut counter = 2u32;
    loop {
        let candidate = format!("{sanitized}{counter}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        counter += 1;
    }
}

