use crate::ir::{AssignTarget, Expression, Statement, Value, VarKind};
use std::collections::BTreeMap;

// Insert `const`/`let` declarations for first-assignments of variables.
// Converts `x = expr;` into `const x = expr;` (if never reassigned) or `let x = expr;`.
// Skips parameters and variables already declared via `Statement::Let`.
pub fn insert_declarations(stmts: &mut [Statement], params: &[String]) {
    // Phase 1: Count total writes per variable across the entire function body
    let mut write_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut let_declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    count_writes(stmts, &mut write_count, &mut let_declared);

    // Phase 2: Walk statements, converting first assignment to declaration
    let param_set: std::collections::HashSet<&str> = params.iter().map(|s| s.as_str()).collect();
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();

    insert_decls_in_block(stmts, &write_count, &let_declared, &param_set, &mut declared);
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
                    && !is_self_assignment_var(name, value)
                {
                    declared.insert(name.clone());
                    let writes = write_count.get(name).copied().unwrap_or(1);
                    let kind = if writes <= 1 { VarKind::Const } else { VarKind::Let };
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
                insert_decls_in_block(then_body, write_count, let_declared, params, declared);
                insert_decls_in_block(else_body, write_count, let_declared, params, declared);
            }
            Statement::While { body, .. } | Statement::DoWhile { body, .. }
            | Statement::For { body, .. } | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => {
                insert_decls_in_block(body, write_count, let_declared, params, declared);
            }
            Statement::Block(inner) => {
                insert_decls_in_block(inner, write_count, let_declared, params, declared);
            }
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                insert_decls_in_block(try_body, write_count, let_declared, params, declared);
                insert_decls_in_block(catch_body, write_count, let_declared, params, declared);
                insert_decls_in_block(finally_body, write_count, let_declared, params, declared);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases.iter_mut() {
                    insert_decls_in_block(body, write_count, let_declared, params, declared);
                }
                if let Some(d) = default {
                    insert_decls_in_block(d, write_count, let_declared, params, declared);
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
