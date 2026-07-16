use crate::ir::{AssignTarget, Expression, Statement, Value, VarKind};
use std::collections::BTreeMap;

// Insert `const`/`let` declarations for first-assignments of variables.
// Converts `x = expr;` into `const x = expr;` (if never reassigned) or `let x = expr;`.
// Skips parameters and variables already declared via `Statement::Let`.
pub fn insert_declarations(stmts: &mut Vec<Statement>, params: &[String]) {
    // Phase 1: Count total writes per variable across the entire function body
    let mut write_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut let_declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    count_writes(stmts, &mut write_count, &mut let_declared);

    // A variable whose first occurrence is a READ is free / captured from an
    // enclosing scope (e.g. counter `c0` mutated inside a returned closure, or
    // legacy `closure_N`). Re-declaring it here shadows the outer binding and
    // causes TDZ (`const c0 = c0 + 1`). Skip declaring free vars; the owner
    // scope still declares them (as `let` when mutated across scopes).
    let free_closures = free_captured_vars(stmts);

    let param_set: std::collections::HashSet<&str> = params.iter().map(|s| s.as_str()).collect();
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Phase 1b: Hoist loop-carried variables. A variable assigned ONLY inside
    // loop(s) but read OUTSIDE the loop would otherwise get its `let` placed
    // inside the loop body, leaving the outside use referencing an out-of-scope
    // binding. Declare those at the function top (`let x;`) and keep their
    // in-loop assignments as plain assignments.
    let mut assigned_in_loop = std::collections::HashSet::new();
    let mut assigned_out_loop = std::collections::HashSet::new();
    let mut ref_out_loop = std::collections::HashSet::new();
    collect_scope_info(stmts, false, &mut assigned_in_loop, &mut assigned_out_loop, &mut ref_out_loop);

    let mut hoist: Vec<String> = assigned_in_loop
        .iter()
        .filter(|v| {
            !assigned_out_loop.contains(*v)
                && ref_out_loop.contains(*v)
                && !param_set.contains(v.as_str())
                && !let_declared.contains(*v)
                && is_valid_js_identifier(v)
        })
        .cloned()
        .collect();
    // Hoist destructuring-pattern names: patterns are emitted as bare assignments
    // (`[a,b] = e`, `({x,y} = e)`), so their names must be declared once at the
    // top, two patterns can share a register-derived name (`x`/`y` reused), and
    // an inline `let` per pattern would redeclare.
    let mut pattern_names: Vec<String> = Vec::new();
    collect_pattern_names(stmts, &mut pattern_names);
    for name in pattern_names {
        if !param_set.contains(name.as_str()) && is_valid_js_identifier(&name) {
            hoist.push(name);
        }
    }
    hoist.sort();
    hoist.dedup();

    if !hoist.is_empty() {
        let mut decls: Vec<Statement> = hoist
            .iter()
            .map(|name| Statement::Let {
                name: name.clone(),
                value: Expression::constant(crate::ir::Constant::Undefined),
                kind: VarKind::Let,
            })
            .collect();
        for name in &hoist {
            declared.insert(name.clone());
        }
        decls.append(stmts);
        *stmts = decls;
    }

    // Phase 2: Walk statements, converting first assignment to declaration
    insert_decls_in_block(stmts, &write_count, &let_declared, &param_set, &free_closures, &mut declared);
}

fn is_env_slot_name(name: &str) -> bool {
    // closure_0, c0, c12, typical Hermes env / counter bindings
    if name.strip_prefix("closure_").is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    name.len() >= 2
        && name.starts_with('c')
        && name[1..].chars().all(|c| c.is_ascii_digit())
}

// Variables whose first textual occurrence in the function is a READ (read
// before written), captured from an enclosing scope. Must not be declared
// here. Evaluation order: an assignment reads its value expression before
// writing its target.
fn free_captured_vars(stmts: &[Statement]) -> std::collections::HashSet<String> {
    let mut first_seen: BTreeMap<String, bool> = BTreeMap::new(); // name -> first was a read
    scan_first_use(stmts, &mut first_seen);
    first_seen
        .into_iter()
        .filter(|(name, read_first)| {
            *read_first
                && is_valid_js_identifier(name)
                // Don't treat reserved/global builtins as free "locals" to skip,                 // they simply aren't declared; skipping is still correct.
                && !name.is_empty()
        })
        .map(|(name, _)| name)
        .collect()
}

// Record, per variable, whether its first occurrence (pre-order, value-before-
// target for assignments) is a read. Only the first occurrence is kept.
fn scan_first_use(stmts: &[Statement], first_seen: &mut BTreeMap<String, bool>) {
    for stmt in stmts {
        match stmt {
            Statement::Assign { target, value } => {
                for r in expr_var_reads(value) {
                    first_seen.entry(r).or_insert(true);
                }
                if let AssignTarget::Variable(name) = target {
                    first_seen.entry(name.clone()).or_insert(false);
                } else {
                    for r in target_var_reads(target) {
                        first_seen.entry(r).or_insert(true);
                    }
                }
            }
            Statement::Let { name, value, .. } => {
                for r in expr_var_reads(value) {
                    first_seen.entry(r).or_insert(true);
                }
                first_seen.entry(name.clone()).or_insert(false);
            }
            Statement::Return(Some(e)) | Statement::Throw(e) | Statement::Expr(e) => {
                for r in expr_var_reads(e) {
                    first_seen.entry(r).or_insert(true);
                }
            }
            Statement::If { condition, then_body, else_body } => {
                for r in expr_var_reads(condition) {
                    first_seen.entry(r).or_insert(true);
                }
                scan_first_use(then_body, first_seen);
                scan_first_use(else_body, first_seen);
            }
            Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
                for r in expr_var_reads(condition) {
                    first_seen.entry(r).or_insert(true);
                }
                scan_first_use(body, first_seen);
            }
            Statement::For { init, condition, update, body } => {
                if let Some(s) = init { scan_first_use(std::slice::from_ref(s), first_seen); }
                if let Some(c) = condition {
                    for r in expr_var_reads(c) { first_seen.entry(r).or_insert(true); }
                }
                if let Some(u) = update { scan_first_use(std::slice::from_ref(u), first_seen); }
                scan_first_use(body, first_seen);
            }
            Statement::Block(inner) => scan_first_use(inner, first_seen),
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                scan_first_use(try_body, first_seen);
                scan_first_use(catch_body, first_seen);
                scan_first_use(finally_body, first_seen);
            }
            Statement::Switch { discriminant, cases, default } => {
                for r in expr_var_reads(discriminant) {
                    first_seen.entry(r).or_insert(true);
                }
                for (_, body) in cases { scan_first_use(body, first_seen); }
                if let Some(d) = default { scan_first_use(d, first_seen); }
            }
            _ => {}
        }
    }
}

fn expr_var_reads(expr: &Expression) -> Vec<String> {
    use crate::ir::Visitor;
    struct R(Vec<String>);
    impl<'b> Visitor<'b> for R {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                self.0.push(n.clone());
            }
            self.walk_expression(e);
        }
    }
    let mut r = R(Vec::new());
    r.visit_expression(expr);
    r.0
}

fn target_var_reads(target: &AssignTarget) -> Vec<String> {
    match target {
        AssignTarget::Member { object, .. } => expr_var_reads(object),
        AssignTarget::Index { object, key } => {
            let mut v = expr_var_reads(object);
            v.extend(expr_var_reads(key));
            v
        }
        _ => Vec::new(),
    }
}

// Collect, split by whether the statement is inside a loop body:
// - variables assigned inside any loop / outside all loops
// - variable names referenced (read or written) outside all loops
fn collect_scope_info(
    stmts: &[Statement],
    in_loop: bool,
    assigned_in_loop: &mut std::collections::HashSet<String>,
    assigned_out_loop: &mut std::collections::HashSet<String>,
    ref_out_loop: &mut std::collections::HashSet<String>,
) {
    use crate::ir::AssignTarget;
    for stmt in stmts {
        // Record assignment targets by scope.
        if let Statement::Assign { target: AssignTarget::Variable(name), .. } = stmt {
            if in_loop {
                assigned_in_loop.insert(name.clone());
            } else {
                assigned_out_loop.insert(name.clone());
            }
        }
        if !in_loop {
            for name in stmt_var_refs(stmt) {
                ref_out_loop.insert(name);
            }
        }
        // Recurse, entering loop scope where appropriate.
        match stmt {
            Statement::While { body, .. } | Statement::DoWhile { body, .. }
            | Statement::For { body, .. } | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => {
                collect_scope_info(body, true, assigned_in_loop, assigned_out_loop, ref_out_loop);
            }
            Statement::If { then_body, else_body, .. } => {
                collect_scope_info(then_body, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
                collect_scope_info(else_body, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
            }
            Statement::Block(inner) => {
                collect_scope_info(inner, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
            }
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                collect_scope_info(try_body, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
                collect_scope_info(catch_body, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
                collect_scope_info(finally_body, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    collect_scope_info(body, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
                }
                if let Some(d) = default {
                    collect_scope_info(d, in_loop, assigned_in_loop, assigned_out_loop, ref_out_loop);
                }
            }
            _ => {}
        }
    }
}

// Variable names referenced (read or written) directly by a statement's own
// expressions/targets (NOT recursing into nested block bodies, the caller
// handles recursion with scope tracking).
fn stmt_var_refs(stmt: &Statement) -> Vec<String> {
    let mut names = Vec::new();
    match stmt {
        Statement::Assign { target, value } => {
            if let crate::ir::AssignTarget::Variable(n) = target {
                names.push(n.clone());
            }
            collect_expr_vars(value, &mut names);
        }
        Statement::Let { name, value, .. } => {
            names.push(name.clone());
            collect_expr_vars(value, &mut names);
        }
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_expr_vars(e, &mut names)
        }
        Statement::If { condition, .. } => collect_expr_vars(condition, &mut names),
        Statement::While { condition, .. } | Statement::DoWhile { condition, .. } => {
            collect_expr_vars(condition, &mut names)
        }
        Statement::Switch { discriminant, .. } => collect_expr_vars(discriminant, &mut names),
        _ => {}
    }
    names
}

// Collect destructuring-pattern target names across the whole function.
fn collect_pattern_names(stmts: &[Statement], out: &mut Vec<String>) {
    for stmt in stmts {
        if let Statement::Assign { target, .. } = stmt {
            if matches!(
                target,
                AssignTarget::DestructuringArray(_)
                    | AssignTarget::DestructuringArrayRest { .. }
                    | AssignTarget::DestructuringObject(_)
                    | AssignTarget::DestructuringObjectRest { .. }
            ) {
                out.extend(destructuring_target_names(target));
            }
        }
        match stmt {
            Statement::If { then_body, else_body, .. } => {
                collect_pattern_names(then_body, out);
                collect_pattern_names(else_body, out);
            }
            Statement::While { body, .. } | Statement::DoWhile { body, .. }
            | Statement::For { body, .. } | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => collect_pattern_names(body, out),
            Statement::Block(inner) => collect_pattern_names(inner, out),
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                collect_pattern_names(try_body, out);
                collect_pattern_names(catch_body, out);
                collect_pattern_names(finally_body, out);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    collect_pattern_names(body, out);
                }
                if let Some(d) = default {
                    collect_pattern_names(d, out);
                }
            }
            _ => {}
        }
    }
}

// Variable names bound by a destructuring assignment target.
fn destructuring_target_names(target: &AssignTarget) -> Vec<String> {
    let mut out = Vec::new();
    fn add(t: &AssignTarget, out: &mut Vec<String>) {
        match t {
            AssignTarget::Variable(n) => out.push(n.clone()),
            AssignTarget::DestructuringArray(elems) => {
                for e in elems.iter().flatten() {
                    add(&e.0, out);
                }
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                for e in elements.iter().flatten() {
                    add(&e.0, out);
                }
                add(rest, out);
            }
            AssignTarget::DestructuringObject(props) => {
                for p in props {
                    add(&p.1, out);
                }
            }
            AssignTarget::DestructuringObjectRest { properties, rest } => {
                for p in properties {
                    add(&p.1, out);
                }
                add(rest, out);
            }
            _ => {}
        }
    }
    add(target, &mut out);
    out
}

fn collect_expr_vars(expr: &Expression, out: &mut Vec<String>) {
    use crate::ir::Visitor;
    struct VarCollector<'a>(&'a mut Vec<String>);
    impl<'a, 'b> Visitor<'b> for VarCollector<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Variable(v)) = e {
                self.0.push(v.clone());
            }
            self.walk_expression(e);
        }
    }
    VarCollector(out).visit_expression(expr);
}

fn count_writes(
    stmts: &[Statement],
    writes: &mut BTreeMap<String, usize>,
    let_declared: &mut std::collections::HashSet<String>,
) {
    for stmt in stmts {
        count_writes_stmt(stmt, writes, let_declared);
    }
}

fn count_writes_stmt(
    stmt: &Statement,
    writes: &mut BTreeMap<String, usize>,
    let_declared: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Statement::Assign { target: AssignTarget::Variable(name), .. } => {
            *writes.entry(name.clone()).or_insert(0) += 1;
        }
        Statement::Assign { target: AssignTarget::Register(r), .. } => {
            *writes.entry(format!("r{r}")).or_insert(0) += 1;
        }
        // A destructuring assign (`let [a, b] = e`) is rendered as its own `let`
        // declaration; record its bound names so a later `a = ...` is a plain
        // reassignment, not a duplicate declaration.
        Statement::Assign { target, .. }
            if matches!(
                target,
                AssignTarget::DestructuringArray(_)
                    | AssignTarget::DestructuringArrayRest { .. }
                    | AssignTarget::DestructuringObject(_)
                    | AssignTarget::DestructuringObjectRest { .. }
            ) =>
        {
            for name in destructuring_target_names(target) {
                let_declared.insert(name);
            }
        }
        Statement::Let { name, .. } => {
            let_declared.insert(name.clone());
        }
        Statement::If { condition: _, then_body, else_body } => {
            count_writes(then_body, writes, let_declared);
            count_writes(else_body, writes, let_declared);
        }
        Statement::While { body, .. } | Statement::DoWhile { body, .. }
        | Statement::For { body, .. } | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => {
            // Variables assigned inside loops are always multi-write
            let mut inner_writes: BTreeMap<String, usize> = BTreeMap::new();
            let mut inner_lets = std::collections::HashSet::new();
            count_writes(body, &mut inner_writes, &mut inner_lets);
            for (name, count) in inner_writes {
                // Treat loop body assignments as at least 2 writes (since loops repeat)
                *writes.entry(name).or_insert(0) += count.max(2);
            }
            let_declared.extend(inner_lets);
        }
        Statement::Block(inner) => count_writes(inner, writes, let_declared),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            count_writes(try_body, writes, let_declared);
            count_writes(catch_body, writes, let_declared);
            count_writes(finally_body, writes, let_declared);
        }
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases {
                count_writes(body, writes, let_declared);
            }
            if let Some(d) = default {
                count_writes(d, writes, let_declared);
            }
        }
        _ => {}
    }
}

fn insert_decls_in_block(
    stmts: &mut [Statement],
    write_count: &BTreeMap<String, usize>,
    let_declared: &std::collections::HashSet<String>,
    params: &std::collections::HashSet<&str>,
    free_closures: &std::collections::HashSet<String>,
    declared: &mut std::collections::HashSet<String>,
) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::Assign { target: AssignTarget::Variable(name), value } => {
                // Skip invalid JS identifiers (numbers, strings, reserved words used as names)
                if is_valid_js_identifier(name)
                    && !params.contains(name.as_str())
                    && !let_declared.contains(name)
                    && !declared.contains(name)
                    && !free_closures.contains(name)
                    && !is_self_assignment_var(name, value)
                {
                    declared.insert(name.clone());
                    let writes = write_count.get(name).copied().unwrap_or(1);
                    // Prefer `let` when mutated more than once, or when the name
                    // looks like a Hermes env slot (`c0`, `closure_N`), those
                    // are often mutated from nested closures even if this
                    // function only shows one write.
                    let kind = if writes > 1 || is_env_slot_name(name) {
                        VarKind::Let
                    } else {
                        VarKind::Const
                    };
                    *stmt = Statement::Let {
                        name: name.clone(),
                        value: value.clone(),
                        kind,
                    };
                }
            }
            Statement::Assign { target: AssignTarget::Register(r), value } => {
                let name = format!("r{r}");
                if !params.contains(name.as_str())
                    && !let_declared.contains(&name)
                    && !declared.contains(&name)
                {
                    declared.insert(name.clone());
                    let writes = write_count.get(&name).copied().unwrap_or(1);
                    let kind = if writes <= 1 { VarKind::Const } else { VarKind::Let };
                    *stmt = Statement::Let {
                        name,
                        value: value.clone(),
                        kind,
                    };
                }
            }
            Statement::If { then_body, else_body, .. } => {
                insert_decls_in_block(then_body, write_count, let_declared, params, free_closures, declared);
                insert_decls_in_block(else_body, write_count, let_declared, params, free_closures, declared);
            }
            Statement::While { body, .. } | Statement::DoWhile { body, .. }
            | Statement::For { body, .. } | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => {
                insert_decls_in_block(body, write_count, let_declared, params, free_closures, declared);
            }
            Statement::Block(inner) => {
                insert_decls_in_block(inner, write_count, let_declared, params, free_closures, declared);
            }
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                insert_decls_in_block(try_body, write_count, let_declared, params, free_closures, declared);
                insert_decls_in_block(catch_body, write_count, let_declared, params, free_closures, declared);
                insert_decls_in_block(finally_body, write_count, let_declared, params, free_closures, declared);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases.iter_mut() {
                    insert_decls_in_block(body, write_count, let_declared, params, free_closures, declared);
                }
                if let Some(d) = default {
                    insert_decls_in_block(d, write_count, let_declared, params, free_closures, declared);
                }
            }
            _ => {}
        }
    }
}

// Check if a name is a valid JavaScript identifier (not a number, not a string literal).
fn is_valid_js_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // Must not start with a digit
    // SAFETY: name.is_empty() is checked above
    let first = match name.chars().next() {
        Some(c) => c,
        None => return false,
    };
    if first.is_ascii_digit() || first == '"' || first == '\'' {
        return false;
    }
    // Must be alphanumeric + _ + $
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

// Check if assignment is a self-assignment: `name = name`
fn is_self_assignment_var(name: &str, value: &Expression) -> bool {
    matches!(value, Expression::Value(Value::Variable(v)) if v == name)
}
