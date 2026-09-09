use super::*;
use crate::ir::{AssignTarget, Expression, Statement, Value};
use std::collections::BTreeMap;

// Variables whose first textual occurrence in the function is a READ (read
// before written), captured from an enclosing scope. Must not be declared
// here. Evaluation order: an assignment reads its value expression before
// writing its target.
pub(super) fn free_captured_vars(stmts: &[Statement]) -> std::collections::HashSet<String> {
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
pub(super) fn collect_scope_info(
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

pub(super) fn collect_expr_vars(expr: &Expression, out: &mut Vec<String>) {
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
