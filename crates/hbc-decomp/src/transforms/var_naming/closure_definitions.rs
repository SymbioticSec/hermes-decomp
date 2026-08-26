use super::closure_inference::infer_name_from_closure_usage;
use super::closure_usage::{
    collect_closure_usage_in_stmt, ident_from_property, is_closure_name, is_usable_object_key,
    unique_object_key, ClosureUsageInfo,
};
use crate::analysis::metro::FactoryRoles;
use crate::analysis::{ClosureContext, ClosureSlotValue};
use crate::ir::{AssignTarget, Expression, Statement, Value, Visitor};
use std::collections::BTreeMap;

// Cross-function closure naming: aggregates usage of `closure_N` across sibling functions
// (children of the same parent) to infer one consistent name per parent slot.
//
// After closure resolution, `closure_N` in function X refers to slot N of X's parent.
// If sibling functions A, B, C all reference `closure_3.setToken()`, we infer "authStore"
// once and apply it everywhere, no `authStore`/`authStore2` inconsistency.
//
// Object-key ground truth (`{ login: closure_1_0 }`) also fills the parent
// parameter that was stored into that slot, so `forgotPassword(arg0)` becomes
// `forgotPassword(login)`.
//
// Returns the number of closure variables renamed.
pub fn rename_closure_variables_cross_function(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &mut ClosureContext,
    param_names: &mut BTreeMap<u32, Vec<Option<String>>>,
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
            if let Some((level, slot)) = parse_closure_capture(&closure_name) {
                // Single-number `closure_N` is a parent-env capture (W10 convention).
                // Two-number `closure_{level}_{slot}` walks `level` hops (1 = parent).
                let pid = if level == 0 {
                    parent_id
                } else {
                    closure_ctx.ancestor_at(func_id, level)
                };

                if let Some(pid) = pid {
                    let key = (pid, slot);
                    func_closure_to_slot.push((func_id, closure_name.clone(), pid, slot));

                    // Merge usage into the aggregated slot info
                    let agg = slot_usage.entry(key).or_default();
                    agg.properties.extend(info.properties);
                    agg.methods.extend(info.methods);
                    agg.object_keys.extend(info.object_keys);
                    if info.called_as_function {
                        agg.called_as_function = true;
                    }
                    agg.indexed_accesses += info.indexed_accesses;
                    if info.spread {
                        agg.spread = true;
                    }
                }
            }
        }
    }

    if slot_usage.is_empty() {
        return apply_object_key_literals(all_ir, closure_ctx, param_names);
    }

    // Phase 2: Infer one name per (parent_id, slot) from aggregated usage
    let mut slot_names: BTreeMap<(u32, u32), String> = BTreeMap::new();
    // Slots whose name came from an object/member key (ground truth) — these
    // also name the parameter stored into the slot.
    let mut object_key_slots: HashSet<(u32, u32)> = HashSet::new();
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
                ClosureSlotValue::Variable(v) => v.as_str(),
                ClosureSlotValue::Function { name: Some(n), .. } => n.as_str(),
                ClosureSlotValue::Constant(c) => c.as_str(),
                _ => "",
            })
            .filter(|s| !s.is_empty());
        let from_key = unique_object_key(info);
        if let Some(inferred) = infer_name_from_closure_usage(info, slot_hint) {
            // `lib`/`mod`/`obj` are type fallbacks. Do not paint an ancestor
            // capture with them: they stick and hide a later object-key name
            // (`{ login: closure_1_0 }` only appears after reconstruct).
            if from_key.is_none() && is_weak_usage_name(&inferred) {
                continue;
            }
            let used = per_parent_used.entry(parent_id).or_default();
            let unique = make_unique_name(&inferred, used);
            if from_key.as_deref() == Some(inferred.as_str()) {
                object_key_slots.insert(*key);
            }
            slot_names.insert(*key, unique);
        }
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

    // The slot owner writes `closure_{slot} = param`. Rename that binding too,
    // and fill the parameter from an object-key name. Look up the param index
    // before rewriting the slot value (`arg0` → `login`).
    for ((parent_id, slot), new_name) in &slot_names {
        let owner_var = format!("closure_{slot}");
        per_func_renames
            .entry(*parent_id)
            .or_default()
            .entry(owner_var)
            .or_insert_with(|| new_name.clone());
        if object_key_slots.contains(&(*parent_id, *slot)) {
            if let Some(idx) = param_index_stored_in_slot(
                *parent_id,
                *slot,
                closure_ctx,
                all_ir,
                param_names,
            ) {
                fill_param_name(param_names, *parent_id, idx, new_name);
            }
        }
        closure_ctx.update_slot_variable(*parent_id, *slot, new_name.clone());
    }

    let mut total_renamed = 0;
    if !slot_names.is_empty() {
        let mut rename_keys: Vec<_> = per_func_renames.keys().copied().collect();
        rename_keys.sort();
        for func_id in &rename_keys {
            let renames = &per_func_renames[func_id];
            if let Some(stmts) = all_ir.get_mut(func_id) {
                total_renamed += renames.len();
                crate::analysis::naming::rename_variables_in_stmts(stmts, renames);
            }
        }
    }

    // After folding / reconstruct, `{ login: closure_1_0 }` (or `{ login: lib }`
    // if a weak name already stuck) is visible. Apply the key even when usage
    // collection saw no object literal.
    total_renamed += apply_object_key_literals(all_ir, closure_ctx, param_names);
    total_renamed
}

// Parse a closure capture into (level, slot).
// `closure_5` / `c5` → (0, 5)  (single-number: parent env, W10 convention)
// `closure_1_5` → (1, 5)        (ancestor at level 1, slot 5)
fn parse_closure_capture(name: &str) -> Option<(u32, u32)> {
    if let Some(rest) = name.strip_prefix("closure_") {
        if let Some((level, slot)) = rest.split_once('_') {
            if !level.is_empty()
                && level.chars().all(|c| c.is_ascii_digit())
                && !slot.is_empty()
                && slot.chars().all(|c| c.is_ascii_digit())
            {
                let l: u32 = level.parse().ok()?;
                let s: u32 = slot.parse().ok()?;
                if l == 0 {
                    return None;
                }
                return Some((l, s));
            }
        }
        let s: u32 = rest.parse().ok()?;
        return Some((0, s));
    }
    if let Some(n) = name.strip_prefix('c') {
        if !n.is_empty() && n.len() <= 6 && n.chars().all(|c| c.is_ascii_digit()) {
            return Some((0, n.parse().ok()?));
        }
    }
    None
}

fn param_index_stored_in_slot(
    parent_id: u32,
    slot: u32,
    closure_ctx: &ClosureContext,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    param_names: &BTreeMap<u32, Vec<Option<String>>>,
) -> Option<u32> {
    let mut slot_var: Option<&str> = None;
    if let Some(info) = closure_ctx.function_closures.get(&parent_id) {
        if let Some(ClosureSlotValue::Variable(v)) = info.slots.get(&slot) {
            slot_var = Some(v.as_str());
            if let Some(idx) = FactoryRoles::extract_param_index(v) {
                return Some(idx);
            }
            if let Some(names) = param_names.get(&parent_id) {
                for (i, n) in names.iter().enumerate() {
                    if n.as_deref() == Some(v.as_str()) {
                        return Some(i as u32);
                    }
                }
            }
        }
    }
    let stmts = all_ir.get(&parent_id)?;
    param_index_from_slot_store(
        stmts,
        slot,
        slot_var,
        param_names.get(&parent_id).map(|v| v.as_slice()),
    )
}

fn param_index_from_slot_store(
    stmts: &[Statement],
    slot: u32,
    slot_var: Option<&str>,
    param_names: Option<&[Option<String>]>,
) -> Option<u32> {
    let owner = format!("closure_{slot}");
    for stmt in stmts {
        if let Some(idx) =
            param_index_from_slot_store_stmt(stmt, slot, &owner, slot_var, param_names)
        {
            return Some(idx);
        }
    }
    None
}

fn param_index_from_slot_store_stmt(
    stmt: &Statement,
    slot: u32,
    owner: &str,
    slot_var: Option<&str>,
    param_names: Option<&[Option<String>]>,
) -> Option<u32> {
    match stmt {
        Statement::Assign { target, value } => {
            let writes_slot = match target {
                AssignTarget::Variable(n)
                    if n == owner || slot_var.is_some_and(|s| n == s) =>
                {
                    true
                }
                AssignTarget::ClosureVar { level: 0, slot: s } if *s == slot => true,
                _ => false,
            };
            if writes_slot {
                if let Some(idx) = param_index_of_expr(value, param_names) {
                    return Some(idx);
                }
            }
            None
        }
        Statement::Let { name, value, .. }
            if name == owner || slot_var.is_some_and(|s| name == s) =>
        {
            param_index_of_expr(value, param_names)
        }
        Statement::If { then_body, else_body, .. } => {
            param_index_from_slot_store(then_body, slot, slot_var, param_names)
                .or_else(|| param_index_from_slot_store(else_body, slot, slot_var, param_names))
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. }
        | Statement::Block(body) => param_index_from_slot_store(body, slot, slot_var, param_names),
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => param_index_from_slot_store(try_body, slot, slot_var, param_names)
            .or_else(|| param_index_from_slot_store(catch_body, slot, slot_var, param_names))
            .or_else(|| param_index_from_slot_store(finally_body, slot, slot_var, param_names)),
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases {
                if let Some(idx) = param_index_from_slot_store(body, slot, slot_var, param_names) {
                    return Some(idx);
                }
            }
            default
                .as_ref()
                .and_then(|d| param_index_from_slot_store(d, slot, slot_var, param_names))
        }
        _ => None,
    }
}

fn param_index_of_expr(value: &Expression, param_names: Option<&[Option<String>]>) -> Option<u32> {
    match value {
        Expression::Value(Value::Parameter(idx)) => Some(*idx),
        Expression::Value(Value::Variable(n)) => {
            if let Some(idx) = FactoryRoles::extract_param_index(n) {
                return Some(idx);
            }
            if let Some(names) = param_names {
                for (i, name) in names.iter().enumerate() {
                    if name.as_deref() == Some(n.as_str()) {
                        return Some(i as u32);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn fill_param_name(
    param_names: &mut BTreeMap<u32, Vec<Option<String>>>,
    func_id: u32,
    idx: u32,
    name: &str,
) {
    let idx = idx as usize;
    if idx >= crate::analysis::ipa::MAX_PARAM_SLOTS {
        return;
    }
    let entry = param_names.entry(func_id).or_default();
    if entry.len() <= idx {
        entry.resize(idx + 1, None);
    }
    // Never collide with another parameter in this function.
    if entry
        .iter()
        .enumerate()
        .any(|(i, n)| i != idx && n.as_deref() == Some(name))
    {
        return;
    }
    match &entry[idx] {
        None => entry[idx] = Some(name.to_string()),
        Some(existing) if param_name_is_placeholder(existing) => {
            entry[idx] = Some(name.to_string());
        }
        Some(_) => {}
    }
}

fn param_name_is_placeholder(name: &str) -> bool {
    if name.starts_with("closure_") || name.starts_with("outer") {
        return true;
    }
    if let Some(rest) = name.strip_prefix("arg") {
        return rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit());
    }
    matches!(name, "first" | "second" | "third" | "fourth" | "last")
}

fn is_weak_usage_name(name: &str) -> bool {
    matches!(
        name,
        "lib" | "mod" | "obj" | "callback" | "arr" | "fn" | "table" | "ctor" | "store"
    )
}

fn is_renamable_from_object_key(name: &str) -> bool {
    is_closure_name(name) || param_name_is_placeholder(name) || is_weak_usage_name(name)
}

// Walk object literals and `obj.key = var` after folding/reconstruct, and name
// the captured slot / parameter from the key even when the value is no longer
// a raw `closure_*` (a prior pass may have painted it `lib`).
fn apply_object_key_literals(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &mut ClosureContext,
    param_names: &mut BTreeMap<u32, Vec<Option<String>>>,
) -> usize {
    let fids: Vec<u32> = all_ir.keys().copied().collect();
    // (owner, slot) -> unique key, or None if ambiguous
    let mut slot_keys: BTreeMap<(u32, u32), Option<String>> = BTreeMap::new();
    // (func, var) -> key, for the rename in that function
    let mut var_keys: BTreeMap<(u32, String), String> = BTreeMap::new();
    // Same-function `{ login: arg0 }` / Parameter
    let mut direct_params: Vec<(u32, u32, String)> = Vec::new();

    for fid in &fids {
        let Some(stmts) = all_ir.get(fid) else { continue };
        let mut hits = ObjectKeyHits::default();
        for s in stmts {
            hits.visit_statement(s);
        }
        for hit in hits.0 {
            if !is_usable_object_key(&hit.key) {
                continue;
            }
            if let Some(idx) = hit.param {
                direct_params.push((*fid, idx, hit.key.clone()));
            }
            let Some(var) = hit.var else { continue };
            if !is_renamable_from_object_key(&var) {
                continue;
            }
            if let Some(idx) = FactoryRoles::extract_param_index(&var) {
                direct_params.push((*fid, idx, hit.key.clone()));
            }
            if let Some((owner, slot)) = resolve_var_to_slot(*fid, &var, closure_ctx) {
                if slot_is_metro_role(closure_ctx, owner, slot) {
                    continue;
                }
                match slot_keys.get(&(owner, slot)) {
                    None => {
                        slot_keys.insert((owner, slot), Some(hit.key.clone()));
                    }
                    Some(Some(existing)) if existing != &hit.key => {
                        slot_keys.insert((owner, slot), None);
                    }
                    _ => {}
                }
                var_keys.insert((*fid, var), hit.key);
            }
        }
    }

    for (fid, idx, key) in &direct_params {
        fill_param_name(param_names, *fid, *idx, key);
    }

    let mut total = 0;
    let mut per_func: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for ((owner, slot), key) in &slot_keys {
        let Some(key) = key else { continue };
        if let Some(idx) = param_index_stored_in_slot(
            *owner,
            *slot,
            closure_ctx,
            all_ir,
            param_names,
        ) {
            fill_param_name(param_names, *owner, idx, key);
        }
        let owner_var = format!("closure_{slot}");
        per_func
            .entry(*owner)
            .or_default()
            .entry(owner_var)
            .or_insert_with(|| key.clone());
        if let Some(info) = closure_ctx.function_closures.get(owner) {
            if let Some(ClosureSlotValue::Variable(v)) = info.slots.get(slot) {
                if is_renamable_from_object_key(v) {
                    per_func
                        .entry(*owner)
                        .or_default()
                        .entry(v.clone())
                        .or_insert_with(|| key.clone());
                }
            }
        }
        closure_ctx.update_slot_variable(*owner, *slot, key.clone());
    }
    for ((fid, var), key) in var_keys {
        per_func.entry(fid).or_default().entry(var).or_insert(key);
    }

    let mut keys: Vec<u32> = per_func.keys().copied().collect();
    keys.sort();
    for fid in keys {
        let renames = &per_func[&fid];
        if let Some(stmts) = all_ir.get_mut(&fid) {
            total += renames.len();
            crate::analysis::naming::rename_variables_in_stmts(stmts, renames);
        }
    }
    total
}

#[derive(Default)]
struct ObjectKeyHits(Vec<ObjectKeyHit>);

struct ObjectKeyHit {
    key: String,
    var: Option<String>,
    param: Option<u32>,
}

impl<'a> Visitor<'a> for ObjectKeyHits {
    fn visit_statement(&mut self, stmt: &'a Statement) {
        if let Statement::Assign {
            target: AssignTarget::Member { property, .. },
            value,
        } = stmt
        {
            self.record(property.clone(), value);
        }
        self.walk_statement(stmt);
    }

    fn visit_expression(&mut self, expr: &'a Expression) {
        if let Expression::Object { properties } = expr {
            for p in properties {
                if let Some(key) = ident_from_property(&p.key) {
                    self.record(key, &p.value);
                }
            }
        }
        self.walk_expression(expr);
    }
}

impl ObjectKeyHits {
    fn record(&mut self, key: String, value: &Expression) {
        match value {
            Expression::Value(Value::Variable(n)) => self.0.push(ObjectKeyHit {
                key,
                var: Some(n.clone()),
                param: None,
            }),
            Expression::Value(Value::Parameter(idx)) => self.0.push(ObjectKeyHit {
                key,
                var: None,
                param: Some(*idx),
            }),
            _ => {}
        }
    }
}

fn slot_is_metro_role(closure_ctx: &ClosureContext, owner: u32, slot: u32) -> bool {
    matches!(
        closure_ctx
            .function_closures
            .get(&owner)
            .and_then(|info| info.slots.get(&slot)),
        Some(ClosureSlotValue::Variable(v))
            if matches!(
                v.as_str(),
                "require"
                    | "dependencyMap"
                    | "exports"
                    | "module"
                    | "global"
                    | "importDefault"
                    | "importAll"
                    | "args"
            )
    )
}

fn resolve_var_to_slot(
    func_id: u32,
    name: &str,
    closure_ctx: &ClosureContext,
) -> Option<(u32, u32)> {
    if let Some((level, slot)) = parse_closure_capture(name) {
        let owner = if level == 0 {
            closure_ctx.parent_function.get(&func_id).copied()
        } else {
            closure_ctx.ancestor_at(func_id, level)
        };
        return owner.map(|o| (o, slot));
    }
    // Already-renamed capture: match the current slot value walking outward.
    let mut current = func_id;
    for _ in 0..8 {
        if let Some(info) = closure_ctx.function_closures.get(&current) {
            let mut matches: Vec<u32> = info
                .slots
                .iter()
                .filter_map(|(&slot, val)| match val {
                    ClosureSlotValue::Variable(v) if v == name => Some(slot),
                    _ => None,
                })
                .collect();
            if matches.len() == 1 {
                return Some((current, matches.remove(0)));
            }
        }
        current = *closure_ctx.parent_function.get(&current)?;
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::ClosureInfo;
    use crate::ir::{ObjectProperty, PropertyKey};

    fn var(name: &str) -> Expression {
        Expression::Value(Value::Variable(name.to_string()))
    }

    fn object_with(key: &str, value: Expression) -> Expression {
        Expression::Object {
            properties: vec![ObjectProperty {
                key: PropertyKey::Ident(key.to_string()),
                value,
            }],
        }
    }

    #[test]
    fn parse_two_number_capture() {
        assert_eq!(parse_closure_capture("closure_1_0"), Some((1, 0)));
        assert_eq!(parse_closure_capture("closure_0"), Some((0, 0)));
        assert_eq!(parse_closure_capture("closure_1_6"), Some((1, 6)));
        assert_eq!(parse_closure_capture("c3"), Some((0, 3)));
        assert!(parse_closure_capture("login").is_none());
    }

    #[test]
    fn object_key_names_captured_param() {
        // Parent (fid 1) stores param 0 into env slot 0. Child (fid 2) uses
        // `{ login: closure_1_0 }`. The key is ground truth for the param.
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Variable("arg0".into()));
        ctx.add_closure_info(1, info);

        let mut all_ir = BTreeMap::new();
        all_ir.insert(
            1,
            vec![Statement::Assign {
                target: AssignTarget::Variable("closure_0".into()),
                value: Expression::Value(Value::Parameter(0)),
            }],
        );
        all_ir.insert(
            2,
            vec![Statement::Return(Some(object_with("login", var("closure_1_0"))))],
        );

        let mut param_names = BTreeMap::new();
        let renamed = rename_closure_variables_cross_function(&mut all_ir, &mut ctx, &mut param_names);
        assert!(renamed > 0, "child capture should be renamed");
        assert_eq!(
            param_names.get(&1).and_then(|v| v.first()).cloned().flatten(),
            Some("login".to_string())
        );

        let child = format!("{:?}", all_ir.get(&2).unwrap());
        assert!(child.contains("login"), "child body should use login, got {child}");
        assert!(
            !child.contains("closure_1_0"),
            "child should not keep the diagnostic capture name, got {child}"
        );
    }

    #[test]
    fn object_key_overrides_placeholder_param() {
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Variable("arg0".into()));
        ctx.add_closure_info(1, info);

        let mut all_ir = BTreeMap::new();
        all_ir.insert(
            1,
            vec![Statement::Assign {
                target: AssignTarget::Variable("closure_0".into()),
                value: Expression::Value(Value::Parameter(0)),
            }],
        );
        all_ir.insert(
            2,
            vec![Statement::Expr(object_with("login", var("closure_1_0")))],
        );

        let mut param_names = BTreeMap::new();
        param_names.insert(1, vec![Some("closure_1_6".into())]);
        rename_closure_variables_cross_function(&mut all_ir, &mut ctx, &mut param_names);
        assert_eq!(
            param_names.get(&1).and_then(|v| v.first()).cloned().flatten(),
            Some("login".to_string()),
            "object-key ground truth must replace a leaked closure_* param name"
        );
    }

    #[test]
    fn member_assign_key_names_capture() {
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Variable("arg0".into()));
        ctx.add_closure_info(1, info);

        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![]);
        all_ir.insert(
            2,
            vec![Statement::Assign {
                target: AssignTarget::Member {
                    object: var("obj"),
                    property: "login".into(),
                },
                value: var("closure_1_0"),
            }],
        );

        let mut param_names = BTreeMap::new();
        rename_closure_variables_cross_function(&mut all_ir, &mut ctx, &mut param_names);
        assert_eq!(
            param_names.get(&1).and_then(|v| v.first()).cloned().flatten(),
            Some("login".to_string())
        );
    }

    #[test]
    fn recovers_login_from_already_renamed_lib() {
        // W10 painted the capture `lib` (type fallback). After reconstruct the
        // object key is visible as `{ login: lib }` and must still recover login.
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Variable("lib".into()));
        ctx.add_closure_info(1, info);

        let mut all_ir = BTreeMap::new();
        all_ir.insert(
            1,
            vec![Statement::Assign {
                target: AssignTarget::Variable("lib".into()),
                value: Expression::Value(Value::Parameter(0)),
            }],
        );
        all_ir.insert(
            2,
            vec![Statement::Return(Some(object_with("login", var("lib"))))],
        );

        let mut param_names = BTreeMap::new();
        rename_closure_variables_cross_function(&mut all_ir, &mut ctx, &mut param_names);
        assert_eq!(
            param_names.get(&1).and_then(|v| v.first()).cloned().flatten(),
            Some("login".to_string())
        );
        let child = format!("{:?}", all_ir.get(&2).unwrap());
        assert!(
            child.contains("login") && !child.contains("Variable(\"lib\")"),
            "lib must be renamed to login, got {child}"
        );
    }

    #[test]
    fn does_not_rename_metro_require_from_object_key() {
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        let mut info = ClosureInfo::new();
        info.store_slot(1, ClosureSlotValue::Variable("require".into()));
        ctx.add_closure_info(1, info);

        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![]);
        all_ir.insert(
            2,
            vec![Statement::Return(Some(object_with("login", var("closure_1_1"))))],
        );

        let mut param_names = BTreeMap::new();
        rename_closure_variables_cross_function(&mut all_ir, &mut ctx, &mut param_names);
        assert!(
            param_names.get(&1).is_none()
                || param_names[&1].iter().all(|n| n.as_deref() != Some("login")),
            "must not steal the require slot name from an object key"
        );
        let child = format!("{:?}", all_ir.get(&2).unwrap());
        assert!(
            child.contains("require") || child.contains("closure_1_1"),
            "require capture must not become login, got {child}"
        );
    }
}

