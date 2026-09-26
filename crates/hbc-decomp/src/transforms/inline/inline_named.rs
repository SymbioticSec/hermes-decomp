// Post-pipeline variable inlining.
//
// After closure resolution and naming, variables are `Variable("tmp")`, `Variable("closure_0")`, etc.
// This pass inlines variables that are assigned once and used once, eliminating temporaries.

use crate::ir::{map_nested_bodies_mut, AssignTarget, Binding, Expression, Statement, Value};
use std::collections::BTreeMap;

use super::cleanup::cleanup_noise;
use super::counting::{
    apply_pending_to_stmt, count_var_defs_uses, flush_pending, remove_used_from_pending,
    substitute_vars_in_expr,
};

// Inline named variables (tmp*, closure_*, etc.) that are assigned once and used once,
// AND eliminate dead assignments (assigned but never read).
// Applied late in the pipeline after all naming passes.
const MAX_ALIAS_PASSES: usize = 8;

// Eliminate pure aliases of immutable bindings.
//
// A statement `X = V` where X has a generic inlinable name, is assigned exactly
// once in the function, and V is a variable that is never assigned anywhere in the
// function, is a pure alias of an immutable binding (a parameter, an import, a
// module capture). Replace X by V everywhere, INCLUDING loop bodies, which is safe
// because V never changes, and drop the alias definition. Applied to a fixed point
// so chains (`a = b; b = require`) collapse. This removes the `tmp = require`,
// `obj = x` aliases that the use-count inliner leaves inside loops.
pub fn eliminate_immutable_aliases(mut stmts: Vec<Statement>) -> Vec<Statement> {
    for _ in 0..MAX_ALIAS_PASSES {
        let mut def_count: BTreeMap<String, usize> = BTreeMap::new();
        let mut use_count: BTreeMap<String, usize> = BTreeMap::new();
        count_var_defs_uses(&stmts, &mut def_count, &mut use_count);

        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        collect_immutable_aliases(&stmts, &def_count, &mut renames);
        if renames.is_empty() {
            break;
        }
        resolve_transitive_renames(&mut renames);
        // Substitute X -> V (values and targets); the alias definition then becomes
        // `V = V` and is removed by the self-assignment cleanup.
        crate::analysis::naming::rename_variables_in_stmts(&mut stmts, &renames);
        stmts = cleanup_noise(stmts);
    }
    stmts
}

fn collect_immutable_aliases(
    stmts: &[Statement],
    def_count: &BTreeMap<String, usize>,
    out: &mut BTreeMap<String, String>,
) {
    use crate::ir::Visitor;
    struct C<'a> {
        def_count: &'a BTreeMap<String, usize>,
        out: &'a mut BTreeMap<String, String>,
    }
    impl<'b> Visitor<'b> for C<'_> {
        fn visit_statement(&mut self, s: &'b Statement) {
            let alias = match s {
                Statement::Let { name, value, .. } => Some((name, value)),
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(name)),
                    value,
                } => Some((name, value)),
                _ => None,
            };
            if let Some((name, Expression::Value(Value::Binding(Binding::Variable(v))))) = alias {
                if v != name
                    && is_inlinable_name(name)
                    && self.def_count.get(name).copied() == Some(1)
                    && self.def_count.get(v).copied().unwrap_or(0) == 0
                    && !self.out.contains_key(name)
                {
                    self.out.insert(name.clone(), v.clone());
                }
            }
            self.walk_statement(s);
        }
    }
    let mut c = C { def_count, out };
    for s in stmts {
        c.visit_statement(s);
    }
}

// Collapse alias chains so `X -> Y` becomes `X -> Z` when `Y -> Z` also holds.
fn resolve_transitive_renames(renames: &mut BTreeMap<String, String>) {
    let snapshot = renames.clone();
    for target in renames.values_mut() {
        let mut cur = target.clone();
        let mut hops = 0;
        while let Some(next) = snapshot.get(&cur) {
            if next == &cur || hops > 32 {
                break;
            }
            cur = next.clone();
            hops += 1;
        }
        *target = cur;
    }
}

pub fn inline_named_variables(stmts: Vec<Statement>) -> Vec<Statement> {
    inline_named_variables_keeping(stmts, &std::collections::HashSet::new())
}

// The same pass, with `keep` naming the variables that must stay bound
// whatever this body does with them: the slots a nested function reads or
// writes. A module storing `set = new Set()` for its exported `clear()` to
// use had the store reduced to `new Set();` because this body never read it.
pub fn inline_named_variables_keeping(
    stmts: Vec<Statement>,
    keep: &std::collections::HashSet<String>,
) -> Vec<Statement> {
    // Phase 1: Count definitions and uses of all variables over the WHOLE
    // function (count_var_defs_uses recurses into nested blocks). The candidate
    // sets are derived once here and reused for nested blocks, recomputing them
    // per-block is unsound: a variable assigned inside an `if` branch but read
    // *after* the `if` has a block-local use count of 0, so it would be wrongly
    // treated as dead and its branch (then the whole `if`) eliminated.
    let mut def_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut use_count: BTreeMap<String, usize> = BTreeMap::new();
    count_var_defs_uses(&stmts, &mut def_count, &mut use_count);

    let inline_candidates: std::collections::HashSet<String> = def_count
        .iter()
        .filter(|(name, &defs)| {
            if defs != 1 || !is_inlinable_name(name) || keep.contains(*name) {
                return false;
            }
            use_count.get(*name).copied().unwrap_or(0) == 1
        })
        .map(|(name, _)| name.clone())
        .collect();

    let multi_use_candidates: std::collections::HashSet<String> = def_count
        .iter()
        .filter(|(name, &defs)| {
            if defs != 1 || !is_inlinable_name(name) || keep.contains(*name) {
                return false;
            }
            let uses = use_count.get(*name).copied().unwrap_or(0);
            // Allow more multi-use pure inlines (cheap member chains / consts).
            uses > 1 && uses <= 48
        })
        .map(|(name, _)| name.clone())
        .collect();

    let dead_candidates: std::collections::HashSet<String> = def_count
        .iter()
        .filter(|(name, &defs)| {
            defs >= 1
                && use_count.get(*name).copied().unwrap_or(0) == 0
                && is_dead_inlinable_name(name)
                && !keep.contains(*name)
        })
        .map(|(name, _)| name.clone())
        .collect();

    inline_named_with_candidates(
        stmts,
        &inline_candidates,
        &multi_use_candidates,
        &dead_candidates,
        &BTreeMap::new(),
    )
}

// Process statements using PRE-COMPUTED (whole-function) candidate sets, and
// recurse into nested blocks with the SAME sets, see `inline_named_variables`.
fn inline_named_with_candidates(
    stmts: Vec<Statement>,
    inline_candidates: &std::collections::HashSet<String>,
    multi_use_candidates: &std::collections::HashSet<String>,
    dead_candidates: &std::collections::HashSet<String>,
    inherited_defs: &BTreeMap<String, Expression>,
) -> Vec<Statement> {
    if inline_candidates.is_empty() && dead_candidates.is_empty() && multi_use_candidates.is_empty()
    {
        return stmts;
    }

    // Phase 2: Process statements
    // For multi-use candidates, we collect their definitions first, then do a substitution pass.
    // A definition the enclosing block already folded away is visible here too,
    // otherwise `length = arr.length` vanished from the outer block while the
    // `else if (1 === length)` chain nested inside it kept reading the name.
    let mut multi_use_defs: BTreeMap<String, Expression> = inherited_defs.clone();
    // Where in `result` each definition was folded, so a nested block only
    // inherits the definitions that precede it.
    let mut def_pos: BTreeMap<String, usize> = BTreeMap::new();
    let mut result = Vec::new();
    let mut pending: BTreeMap<String, Expression> = BTreeMap::new();

    for stmt in stmts {
        flush_pending_blocked_by(&stmt, &mut pending, &mut result);
        // Extract variable name and value for both Assign and Let statements
        let var_info = match &stmt {
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(name)),
                value,
            } => Some((name.clone(), value.clone(), false)),
            Statement::Let { name, value, .. } => Some((name.clone(), value.clone(), true)),
            _ => None,
        };

        if let Some((name, value, _is_let)) = var_info {
            // Never inline/defer/eliminate function definitions -- they may be accessed via closure
            if matches!(value, Expression::Function { .. }) {
                let mut stmt = stmt;
                apply_pending_to_stmt(&mut stmt, &mut pending);
                if !multi_use_defs.is_empty() {
                    apply_multi_use_to_stmt(&mut stmt, &multi_use_defs);
                }
                result.push(stmt);
            } else if inline_candidates.contains(&name) {
                // Single-use inline candidate: defer until usage
                let mut value = value;
                let consumed = value.clone();
                substitute_vars_in_expr(&mut value, &pending);
                remove_used_from_pending(&consumed, &mut pending);
                substitute_vars_in_expr(&mut value, &multi_use_defs);

                // Constants go to multi_use_defs so they work in nested blocks
                if is_constant_expr(&value) {
                    def_pos.insert(name.clone(), result.len());
                    multi_use_defs.insert(name.clone(), value);
                } else {
                    if value.has_side_effects() && pending.values().any(|e| e.has_side_effects()) {
                        flush_pending(&mut pending, &mut result);
                    }
                    pending.insert(name.clone(), value);
                }
            } else if multi_use_candidates.contains(&name) {
                // Multi-use candidate: only inline if value is simple (no side effects, short)
                let mut value = value;
                let consumed = value.clone();
                substitute_vars_in_expr(&mut value, &pending);
                remove_used_from_pending(&consumed, &mut pending);
                substitute_vars_in_expr(&mut value, &multi_use_defs);
                if is_simple_pure_expr(&value) {
                    def_pos.insert(name.clone(), result.len());
                    multi_use_defs.insert(name.clone(), value);
                    // Don't emit the assignment -- it will be substituted at use sites
                } else {
                    // Not simple enough: emit as normal
                    let mut stmt = Statement::Assign {
                        target: AssignTarget::Binding(Binding::Variable(name.clone())),
                        value,
                    };
                    apply_pending_to_stmt(&mut stmt, &mut pending);
                    result.push(stmt);
                }
            } else if dead_candidates.contains(&name) {
                // Dead assignment: keep only the side effect
                // NEVER drop function definitions -- they may be accessed via closure slots
                if matches!(value, Expression::Function { .. }) {
                    let mut stmt = stmt;
                    apply_pending_to_stmt(&mut stmt, &mut pending);
                    if !multi_use_defs.is_empty() {
                        apply_multi_use_to_stmt(&mut stmt, &multi_use_defs);
                    }
                    result.push(stmt);
                } else if value.has_side_effects() {
                    let mut value = value;
                    let consumed = value.clone();
                    substitute_vars_in_expr(&mut value, &pending);
                    remove_used_from_pending(&consumed, &mut pending);
                    substitute_vars_in_expr(&mut value, &multi_use_defs);
                    result.push(Statement::Expr(value));
                }
                // else: pure expression assigned to dead var -- drop entirely
            } else {
                let mut stmt = stmt;
                apply_pending_to_stmt(&mut stmt, &mut pending);
                result.push(stmt);
            }
        } else {
            let mut stmt = stmt;
            apply_pending_to_stmt(&mut stmt, &mut pending);
            // Also substitute multi-use definitions
            if !multi_use_defs.is_empty() {
                apply_multi_use_to_stmt(&mut stmt, &multi_use_defs);
            }
            result.push(stmt);
        }
    }

    flush_pending(&mut pending, &mut result);

    // Phase 3: Recurse into sub-blocks to apply inlining there too, reusing the
    // whole-function candidate sets and the definitions folded before the block.
    for (i, stmt) in result.iter_mut().enumerate() {
        if !has_nested_bodies(stmt) {
            continue;
        }
        let visible: BTreeMap<String, Expression> = multi_use_defs
            .iter()
            .filter(|(name, _)| def_pos.get(*name).is_none_or(|p| *p <= i))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        recurse_inline_blocks(
            stmt,
            inline_candidates,
            multi_use_candidates,
            dead_candidates,
            &visible,
        );
    }

    // Phase 4: Clean up noise
    result = cleanup_noise(result);

    result
}

// Recursively apply inlining to inner blocks of structured statements, EXCEPT
// loop bodies. Inlining inside a loop from isolated def/use counts is unsound: a
// loop-carried variable defined in the body but read in the condition or after
// the loop (via the back-edge) looks dead/single-use locally and would be wrongly
// eliminated (e.g. `sum = sum + i` dropped). Non-loop blocks (if/try/switch/block)
// are recursed with the SHARED whole-function candidate sets (passed in) so a
// variable used after the block is not treated as dead inside it.
fn recurse_inline_blocks(
    stmt: &mut Statement,
    inline_candidates: &std::collections::HashSet<String>,
    multi_use_candidates: &std::collections::HashSet<String>,
    dead_candidates: &std::collections::HashSet<String>,
    inherited_defs: &BTreeMap<String, Expression>,
) {
    match stmt {
        // Loop bodies: leave untouched (correctness over extra inlining).
        Statement::While { .. }
        | Statement::DoWhile { .. }
        | Statement::For { .. }
        | Statement::ForIn { .. }
        | Statement::ForOf { .. } => {}
        _ => map_nested_bodies_mut(stmt, |s| {
            inline_named_with_candidates(
                s,
                inline_candidates,
                multi_use_candidates,
                dead_candidates,
                inherited_defs,
            )
        }),
    }
}

fn has_nested_bodies(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::If { .. }
            | Statement::TryCatch { .. }
            | Statement::Switch { .. }
            | Statement::Block(_)
    )
}

// Check if an expression is a simple constant (integer, string, bool, null, undefined).
fn is_constant_expr(expr: &Expression) -> bool {
    matches!(expr, Expression::Value(Value::Constant(_)))
}

// Check if an expression is simple and pure (safe to duplicate for multi-use inlining).
// A deferred value only moves past a statement that cannot change what it
// evaluates to. Before a statement with effects (a call, a store, any nested
// control flow), everything but a plain constant or name read is written out
// where it was. A name read is written out before a statement that writes
// that name anywhere inside it. `r = f.call(x)` was carried across a whole
// try/catch this way and came out after it.
fn flush_pending_blocked_by(
    stmt: &Statement,
    pending: &mut BTreeMap<String, Expression>,
    result: &mut Vec<Statement>,
) {
    if pending.is_empty() {
        return;
    }
    // `apply_pending_to_stmt` substitutes into flat statements only, so a
    // structured statement is where every deferred value gets written out.
    let structured = !matches!(
        stmt,
        Statement::Assign { .. }
            | Statement::Let { .. }
            | Statement::Expr(_)
            | Statement::Return(_)
            | Statement::Throw(_)
            | Statement::Delete { .. }
            | Statement::Comment(_)
    );
    let effects = crate::ir::stmt_has_side_effects(stmt);
    let writes_memory = stmt_may_write_memory(stmt);
    let written = assigned_variable_names(stmt);
    let blocked: Vec<String> = pending
        .iter()
        .filter(|(_, value)| {
            if structured || written.iter().any(|w| expr_reads_variable(value, w)) {
                return true;
            }
            if matches!(value, Expression::Value(_)) {
                return false;
            }
            // A value that runs code keeps its place among the other effects. A
            // value that only reads memory (`a.b.c`) may cross a literal or a
            // plain copy, but not a call or a store, which could change it.
            if value.has_side_effects() {
                effects
            } else {
                writes_memory
            }
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in blocked {
        if let Some(value) = pending.remove(&name) {
            result.push(Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(name)),
                value,
            });
        }
    }
}

// Whether a flat statement can change what a property read evaluates to: it
// calls something, constructs something, stores into a property or index,
// deletes one, or suspends. An array or object literal with no call inside
// does none of that, even though it counts as an effect elsewhere.
fn stmt_may_write_memory(stmt: &Statement) -> bool {
    use crate::ir::Visitor;
    struct C(bool);
    impl<'a> Visitor<'a> for C {
        fn visit_expression(&mut self, e: &'a Expression) {
            if matches!(
                e,
                Expression::Call { .. }
                    | Expression::New { .. }
                    | Expression::Assignment { .. }
                    | Expression::Await(_)
                    | Expression::Yield { .. }
                    | Expression::Unknown { .. }
            ) {
                self.0 = true;
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, target: &'a AssignTarget) {
            if !matches!(target, AssignTarget::Binding(_)) {
                self.0 = true;
            }
            self.walk_assign_target(target);
        }
    }
    if matches!(stmt, Statement::Delete { .. }) {
        return true;
    }
    let mut c = C(false);
    c.visit_statement(stmt);
    c.0
}

fn assigned_variable_names(stmt: &Statement) -> Vec<String> {
    use crate::ir::Visitor;
    struct C(Vec<String>);
    impl<'a> Visitor<'a> for C {
        fn visit_assign_target(&mut self, target: &'a AssignTarget) {
            if let AssignTarget::Binding(Binding::Variable(name)) = target {
                self.0.push(name.clone());
            }
            self.walk_assign_target(target);
        }
        fn visit_binding_def(&mut self, name: &'a str) {
            self.0.push(name.to_string());
        }
    }
    let mut c = C(Vec::new());
    c.visit_statement(stmt);
    c.0
}

fn expr_reads_variable(expr: &Expression, name: &str) -> bool {
    use crate::ir::Visitor;
    struct C<'n>(&'n str, bool);
    impl<'a> Visitor<'a> for C<'_> {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
                if n == self.0 {
                    self.1 = true;
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(name, false);
    c.visit_expression(expr);
    c.1
}

fn is_simple_pure_expr(expr: &Expression) -> bool {
    is_simple_pure_expr_depth(expr, 0)
}

fn is_simple_pure_expr_depth(expr: &Expression, depth: u8) -> bool {
    if depth > 4 {
        return false;
    }
    match expr {
        Expression::Value(Value::Binding(Binding::Variable(_)))
        | Expression::Value(Value::Parameter(_))
        | Expression::Value(Value::Constant(_))
        | Expression::Value(Value::Global)
        | Expression::Value(Value::This)
        | Expression::Value(Value::NewTarget)
        | Expression::Value(Value::Super) => true,
        // Member / index chains: a.b.c, a[0]
        Expression::Member {
            object, property, ..
        } => {
            is_simple_pure_expr_depth(object, depth + 1)
                && match property {
                    crate::ir::PropertyKey::Computed(k) => is_simple_pure_expr_depth(k, depth + 1),
                    _ => true,
                }
        }
        Expression::Unary { operand, .. } => is_simple_pure_expr_depth(operand, depth + 1),
        Expression::Binary { left, right, .. } => {
            is_simple_pure_expr_depth(left, depth + 1)
                && is_simple_pure_expr_depth(right, depth + 1)
        }
        _ => false,
    }
}

fn apply_multi_use_to_stmt(stmt: &mut Statement, defs: &BTreeMap<String, Expression>) {
    match stmt {
        Statement::Assign { target, value } => {
            apply_multi_use_to_target(target, defs);
            substitute_vars_in_expr(value, defs);
        }
        Statement::Let { value, .. } => {
            substitute_vars_in_expr(value, defs);
        }
        Statement::Expr(e) => substitute_vars_in_expr(e, defs),
        Statement::Return(Some(e)) | Statement::Throw(e) => substitute_vars_in_expr(e, defs),
        // Loop/branch conditions: substitute (safe, these defs are constants /
        // simple pure values). Bodies are handled by the recursion, but loop
        // bodies are intentionally skipped, so doing the condition here ensures a
        // hoisted constant (e.g. a loop bound) still reaches `while (i < 5)`.
        Statement::While { condition, .. } | Statement::DoWhile { condition, .. } => {
            substitute_vars_in_expr(condition, defs);
        }
        Statement::If { condition, .. } => substitute_vars_in_expr(condition, defs),
        Statement::Switch { discriminant, .. } => substitute_vars_in_expr(discriminant, defs),
        Statement::Delete { target, .. } => substitute_vars_in_expr(target, defs),
        _ => {}
    }
}

fn apply_multi_use_to_target(target: &mut AssignTarget, defs: &BTreeMap<String, Expression>) {
    match target {
        AssignTarget::Member { object, .. } => substitute_vars_in_expr(object, defs),
        AssignTarget::Index { object, key } => {
            substitute_vars_in_expr(object, defs);
            substitute_vars_in_expr(key, defs);
        }
        _ => {}
    }
}

// Shared list of generic register role prefixes (from register naming analysis).
// A name matching "prefix" or "prefixN" (e.g., "obj", "obj2") is generic.
const GENERIC_ROLE_PREFIXES: &[&str] = &[
    "num", "str", "obj", "fn", "arr", "bool", "length", "iter", "promise", "date", "err", "map",
    "set", "key", "val", "idx", "ref", "flag",
];

// Prefixes for intermediate wrapper variables generated by Babel/bundlers.
const WRAPPER_PREFIXES: &[&str] = &["_default", "_interop", "_extends"];

// Suffixes for intermediate computed values.
const INTERMEDIATE_SUFFIXES: &[&str] = &[
    "Result", "Return", "Promise", "Callback", "Handler", "Wrapper",
];

fn is_tmp_or_register(name: &str) -> bool {
    // tmp, tmp2, tmp3, ...
    if name == "tmp"
        || name
            .strip_prefix("tmp")
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    // r0, r1, ...
    if name
        .strip_prefix('r')
        .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    false
}

fn is_generic_role_name(name: &str) -> bool {
    for prefix in GENERIC_ROLE_PREFIXES {
        if name == *prefix
            || name
                .strip_prefix(prefix)
                .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        {
            return true;
        }
    }
    false
}

fn is_wrapper_or_intermediate(name: &str) -> bool {
    if WRAPPER_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return true;
    }
    for suffix in INTERMEDIATE_SUFFIXES {
        if name.ends_with(suffix) || name.contains(suffix) {
            return true;
        }
    }
    false
}

// More restrictive than `is_inlinable_name` -- excludes closure_* since they are
// accessed from other scopes via the closure environment.
pub fn is_dead_inlinable_name(name: &str) -> bool {
    is_tmp_or_register(name) || is_generic_role_name(name) || is_wrapper_or_intermediate(name)
    // Do NOT include closure_* -- they may be read from other function scopes
}

// Check if a variable name is a candidate for inlining (temporary/generic names only).
pub(super) fn is_inlinable_name(name: &str) -> bool {
    // NOTE: `closure_N` is deliberately excluded (as in `is_dead_inlinable_name`).
    // A resolved closure variable is shared with other function scopes; inlining
    // its value within one scope drops the binding the other scope still reads
    // (e.g. a captured counter `closure_0 += 1` mutated inside a returned closure).
    is_tmp_or_register(name) || is_generic_role_name(name) || is_wrapper_or_intermediate(name)
}

#[cfg(test)]
mod pending_order_tests {
    use super::inline_named_variables;
    use crate::ir::{AssignTarget, Binding, Constant, Expression, Statement, Value};

    fn var(name: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(name.into())))
    }

    fn assign(name: &str, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(name.into())),
            value,
        }
    }

    fn call(callee: &str) -> Expression {
        Expression::Call {
            callee: Box::new(var(callee)),
            arguments: vec![],
        }
    }

    #[test]
    fn a_call_is_not_carried_across_a_try() {
        // tmp = f(); try { g() } catch {}; if (tmp) {}
        let stmts = vec![
            assign("tmp", call("f")),
            Statement::TryCatch {
                try_body: vec![Statement::Expr(call("g"))],
                catch_param: None,
                catch_body: vec![],
                finally_body: vec![],
            },
            Statement::If {
                condition: var("tmp"),
                then_body: vec![],
                else_body: vec![],
            },
        ];
        let out = inline_named_variables(stmts);
        assert!(
            matches!(&out[0], Statement::Assign { value, .. } if *value == call("f")),
            "the call stays before the try: {out:?}"
        );
        assert!(matches!(&out[1], Statement::TryCatch { .. }));
    }

    #[test]
    fn a_value_folded_into_another_deferred_value_is_emitted_once() {
        // tmp = f(); tmp2 = tmp + 1; return tmp2
        let stmts = vec![
            assign("tmp", call("f")),
            assign(
                "tmp2",
                Expression::binary(
                    crate::ir::BinaryOp::Add,
                    var("tmp"),
                    Expression::constant(Constant::Integer(1)),
                ),
            ),
            Statement::Return(Some(var("tmp2"))),
        ];
        let out = inline_named_variables(stmts);
        let rendered: Vec<String> = out.iter().map(|s| s.to_string()).collect();
        let calls = rendered.iter().filter(|s| s.contains("f()")).count();
        assert_eq!(calls, 1, "f() rendered once: {rendered:?}");
    }
}
