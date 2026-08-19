// Remove dead generic-temp bindings whose value is pure.
//
// Hermes moves call arguments into consecutive registers before a Call (`Mov r19,
// r12; Mov r20, r13; ...; Call fn, r19, ...`). The IR builder reconstructs the
// call from the source registers (`fn(tmp12, tmp13, ...)`), which leaves the
// argument-setup copies (`let tmp19 = tmp12; ...`) reading nothing: they are never
// referenced again. The early dead-assignment pass ran before that reconstruction,
// when the copies still fed the raw Call, so nothing cleans them up afterwards.
//
// This late pass drops a `let X = V` / `X = V` when X is a decompiler-generated
// temp name, V has no side effect, and X is never read anywhere in the function.
// Only unread pure bindings go, so behaviour is unchanged.

use std::collections::HashMap;

use crate::ir::{AssignTarget, Expression, Statement, Value, Visitor};

pub fn remove_dead_temp_bindings(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut reads: HashMap<String, u32> = HashMap::new();
    {
        let mut counter = ReadCounter { reads: &mut reads };
        for s in &stmts {
            counter.visit_statement(s);
        }
    }
    let mut out = strip(stmts, &reads);
    // A removed copy can make a name that fed it unread too, but re-counting once
    // is enough for the shallow copy chains Hermes emits; deeper chains are rare.
    let mut reads2: HashMap<String, u32> = HashMap::new();
    {
        let mut counter = ReadCounter { reads: &mut reads2 };
        for s in &out {
            counter.visit_statement(s);
        }
    }
    out = strip(out, &reads2);
    out
}

fn strip(stmts: Vec<Statement>, reads: &HashMap<String, u32>) -> Vec<Statement> {
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        // Recurse into nested blocks first.
        let stmt = recurse(stmt, reads);
        let drop = match &stmt {
            Statement::Let { name, value, .. } => is_dead_binding(name, value, reads),
            Statement::Assign { target: AssignTarget::Variable(name), value } => {
                is_dead_binding(name, value, reads)
            }
            _ => false,
        };
        if !drop {
            out.push(stmt);
        }
    }
    out
}

fn is_dead_binding(name: &str, value: &Expression, reads: &HashMap<String, u32>) -> bool {
    is_generic_temp(name)
        && reads.get(name).copied().unwrap_or(0) == 0
        && !value.has_side_effects()
}

fn recurse(stmt: Statement, reads: &HashMap<String, u32>) -> Statement {
    match stmt {
        Statement::If { condition, then_body, else_body } => Statement::If {
            condition,
            then_body: strip(then_body, reads),
            else_body: strip(else_body, reads),
        },
        Statement::While { condition, body } => Statement::While { condition, body: strip(body, reads) },
        Statement::DoWhile { body, condition } => Statement::DoWhile { body: strip(body, reads), condition },
        Statement::For { init, condition, update, body } => Statement::For {
            init,
            condition,
            update,
            body: strip(body, reads),
        },
        Statement::ForIn { variable, object, body } => Statement::ForIn { variable, object, body: strip(body, reads) },
        Statement::ForOf { variable, iterable, body } => Statement::ForOf { variable, iterable, body: strip(body, reads) },
        Statement::Block(inner) => Statement::Block(strip(inner, reads)),
        Statement::TryCatch { try_body, catch_body, finally_body, catch_param } => Statement::TryCatch {
            try_body: strip(try_body, reads),
            catch_body: strip(catch_body, reads),
            finally_body: strip(finally_body, reads),
            catch_param,
        },
        Statement::Switch { discriminant, cases, default } => Statement::Switch {
            discriminant,
            cases: cases.into_iter().map(|(c, body)| (c, strip(body, reads))).collect(),
            default: default.map(|d| strip(d, reads)),
        },
        other => other,
    }
}

// Counts reads of variables in expressions. Assignment/Let targets are NOT
// expressions, so a binding's own name is not counted as a read of itself; a
// member/index target's object IS an expression and is counted.
struct ReadCounter<'a> {
    reads: &'a mut HashMap<String, u32>,
}

impl<'a> Visitor<'a> for ReadCounter<'a> {
    fn visit_expression(&mut self, e: &'a Expression) {
        if let Expression::Value(Value::Variable(name)) = e {
            *self.reads.entry(name.clone()).or_insert(0) += 1;
        }
        self.walk_expression(e);
    }
}

// Decompiler-generated register/temp names, safe to drop when unread. User or
// semantically recovered names (email, response, closure captures, ...) are left
// alone even if they appear unread, to avoid deleting meaningful declarations.
fn is_generic_temp(name: &str) -> bool {
    const PREFIXES: &[&str] = &["tmp", "num", "str", "val", "bool", "obj", "arr"];
    for p in PREFIXES {
        if let Some(rest) = name.strip_prefix(p) {
            if rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    // Bare register names `rN`.
    if let Some(rest) = name.strip_prefix('r') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::VarKind;

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Variable(n.into()))
    }

    #[test]
    fn drops_unread_pure_temp_copy() {
        // `let tmp19 = tmp12; f(tmp12);` -> tmp19 is never read, drop it.
        let stmts = vec![
            Statement::Let { name: "tmp19".into(), value: var("tmp12"), kind: VarKind::Let },
            Statement::Expr(Expression::Call {
                callee: Box::new(var("f")),
                arguments: vec![var("tmp12")],
            }),
        ];
        let out = remove_dead_temp_bindings(stmts);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Statement::Expr(Expression::Call { .. })));
    }

    #[test]
    fn keeps_read_temp() {
        let stmts = vec![
            Statement::Let { name: "tmp19".into(), value: var("tmp12"), kind: VarKind::Let },
            Statement::Expr(Expression::Call { callee: Box::new(var("f")), arguments: vec![var("tmp19")] }),
        ];
        let out = remove_dead_temp_bindings(stmts);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn keeps_side_effecting_value() {
        // `let tmp1 = f();` unread but the call must run.
        let stmts = vec![Statement::Let {
            name: "tmp1".into(),
            value: Expression::Call { callee: Box::new(var("f")), arguments: vec![] },
            kind: VarKind::Let,
        }];
        let out = remove_dead_temp_bindings(stmts);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn keeps_named_binding() {
        // A meaningful name is kept even if unread.
        let stmts = vec![Statement::Let { name: "email".into(), value: var("tmp1"), kind: VarKind::Const }];
        let out = remove_dead_temp_bindings(stmts);
        assert_eq!(out.len(), 1);
    }
}
