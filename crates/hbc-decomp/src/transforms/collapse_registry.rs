// Collapse the Metro module-registration bootstrap.
//
// The bundle entry function ends with one `__d(factory, moduleId, [deps])` call
// per module (thousands of them). Each factory is elided to `{ ... }` because the
// module it defines is already rendered in full above as its own block, so this
// run is pure runtime wiring that duplicates the module list. Replace a long run
// of these registrations with a single comment; the `__r(...)` run calls that
// follow are left untouched.

use crate::ir::{Constant, Expression, PropertyKey, Statement, Value};

// Only collapse a run this long or longer, so the real registry (thousands of
// entries) is folded while any incidental `__d` call is left in place.
const MIN_RUN: usize = 16;

pub fn collapse_metro_registry(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out = Vec::with_capacity(stmts.len());
    let mut run: Vec<Statement> = Vec::new();
    for stmt in stmts {
        if is_module_registration(&stmt) {
            run.push(stmt);
            continue;
        }
        flush(&mut out, &mut run);
        out.push(stmt);
    }
    flush(&mut out, &mut run);
    out
}

fn flush(out: &mut Vec<Statement>, run: &mut Vec<Statement>) {
    if run.len() >= MIN_RUN {
        out.push(Statement::Comment(format!(
            "Metro registry: {} module registrations omitted (each __d(factory, id, deps) wires a module rendered above)",
            run.len()
        )));
        run.clear();
    } else {
        out.append(run);
    }
}

// A `__d(factory, moduleId, deps)` statement: a bare call to `__d` whose second
// argument is the integer module id.
fn is_module_registration(stmt: &Statement) -> bool {
    let Statement::Expr(Expression::Call { callee, arguments }) = stmt else {
        return false;
    };
    if !callee_is_define(callee) || arguments.len() < 2 {
        return false;
    }
    // The module id is the second argument (an integer). Metro method calls carry
    // a leading `undefined`/`this` argument in some bundles, so also accept the id
    // at index 2 when index 1 is not the integer.
    matches!(
        arguments.get(1),
        Some(Expression::Value(Value::Constant(Constant::Integer(_))))
    ) || matches!(
        arguments.get(2),
        Some(Expression::Value(Value::Constant(Constant::Integer(_))))
    )
}

// Whether an expression names the Metro define function `__d` (a bare global or a
// member access whose property is `__d`).
fn callee_is_define(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Variable(name)) => name == "__d",
        Expression::Member { property, .. } => matches!(
            property,
            PropertyKey::Ident(p) | PropertyKey::String(p) if p == "__d"
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factory() -> Expression {
        Expression::Function { id: crate::ir::FunctionId(1), name: None, is_arrow: false, is_async: false, is_generator: false }
    }

    fn define(id: i32) -> Statement {
        Statement::Expr(Expression::Call {
            callee: Box::new(Expression::Value(Value::Variable("__d".into()))),
            arguments: vec![
                factory(),
                Expression::Value(Value::Constant(Constant::Integer(id))),
                Expression::Array { elements: vec![] },
            ],
        })
    }

    fn run_call(name: &str) -> Statement {
        Statement::Expr(Expression::Call {
            callee: Box::new(Expression::Value(Value::Variable(name.into()))),
            arguments: vec![Expression::Value(Value::Constant(Constant::Integer(0)))],
        })
    }

    #[test]
    fn long_run_is_collapsed_and_run_calls_kept() {
        let mut stmts: Vec<Statement> = (0..20).map(define).collect();
        stmts.push(run_call("__r"));
        let out = collapse_metro_registry(stmts);
        // One comment for the 20 registrations, then the __r call.
        assert!(matches!(&out[0], Statement::Comment(c) if c.contains("20 module registrations")));
        assert!(matches!(&out[1], Statement::Expr(Expression::Call { .. })));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn short_run_is_left_alone() {
        let stmts = vec![define(0), define(1), run_call("__r")];
        let out = collapse_metro_registry(stmts);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|s| matches!(s, Statement::Expr(_))));
    }
}
