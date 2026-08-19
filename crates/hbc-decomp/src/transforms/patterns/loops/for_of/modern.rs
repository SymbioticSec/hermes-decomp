use super::{is_iterator_call, unwrap_iterator_body};
use crate::analysis::rename_registers;
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;

// Detect for-of loop patterns and rebuild them as `for (item of source)`.
//
// Hermes lowers `for (x of src)` to the iterator protocol
// (IteratorBegin / IteratorNext / IteratorClose). After IR build + structure
// recovery the shape is:
//
//   iter = src[Symbol.iterator]()        // IteratorBegin
//   val  = iter.next()                    // IteratorNext (value)
//   copy = iter                           // Mov of the iterator/index
//   while (copy !== undefined) {          // done check (iter set to undefined)
//     try { <body using val> } catch (e) { iter.return(); throw e }
//   }
//
// We match that and emit `for (val of src) { <body> }`, dropping the iterator
// plumbing (the per-iteration fetch is reintroduced by for-of semantics).
pub fn detect_for_of_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = recurse(stmts);
    let mut result: Vec<Statement> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        if let Some((consumed, emitted)) = try_match_for_of(&stmts[i..]) {
            result.extend(emitted);
            i += consumed;
            continue;
        }
        result.push(stmts[i].clone());
        i += 1;
    }
    result
}

// Recurse into nested blocks first so inner for-of loops are rebuilt too.
fn recurse(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::While { condition, body } => Statement::While {
                condition,
                body: detect_for_of_loops(body),
            },
            Statement::DoWhile { body, condition } => Statement::DoWhile {
                body: detect_for_of_loops(body),
                condition,
            },
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: detect_for_of_loops(then_body),
                else_body: detect_for_of_loops(else_body),
            },
            Statement::For { init, condition, update, body } => Statement::For {
                init,
                condition,
                update,
                body: detect_for_of_loops(body),
            },
            Statement::ForOf { variable, iterable, body } => Statement::ForOf {
                variable,
                iterable,
                body: detect_for_of_loops(body),
            },
            Statement::ForIn { variable, object, body } => Statement::ForIn {
                variable,
                object,
                body: detect_for_of_loops(body),
            },
            Statement::Block(inner) => Statement::Block(detect_for_of_loops(inner)),
            Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => {
                Statement::TryCatch {
                    try_body: detect_for_of_loops(try_body),
                    catch_param,
                    catch_body: detect_for_of_loops(catch_body),
                    finally_body: detect_for_of_loops(finally_body),
                }
            }
            other => other,
        })
        .collect()
}

// Try to match the iterator sequence at the start of `stmts`. Returns the number
// of leading statements consumed and the statements to emit in their place
// (any non-iterator statements that were interleaved, e.g. an `undefined` load
// the trailing `return` still needs, are preserved, followed by the ForOf).
fn try_match_for_of(stmts: &[Statement]) -> Option<(usize, Vec<Statement>)> {
    // [0] iter = src[Symbol.iterator]()
    let (iter_reg, source) = match &stmts[0] {
        Statement::Assign { target: AssignTarget::Register(r), value } => {
            (*r, is_iterator_call(value)?)
        }
        _ => return None,
    };

    // Scan the iterator plumbing between IteratorBegin and the loop, in any order:
    //   val = iter.next()         (the per-iteration value, capture it)
    //   copy = iter               (alias of the iterator/index register)
    //   x = undefined             (the done sentinel)
    // Stop at the `while` (the iterator loop).
    let mut idx = 1;
    let mut iter_aliases = vec![iter_reg];
    let mut val_reg: Option<u32> = None;
    // Non-iterator statements interleaved with the plumbing that must survive
    // (e.g. an `undefined` constant load referenced after the loop).
    let mut kept: Vec<Statement> = Vec::new();
    while let Some(stmt) = stmts.get(idx) {
        match stmt {
            // val = <alias>.next()
            Statement::Assign { target: AssignTarget::Register(r), value }
                if iter_aliases.iter().any(|&a| is_next_call(value, a)) =>
            {
                val_reg = Some(*r);
                idx += 1;
            }
            // copy = iter (alias)
            Statement::Assign {
                target: AssignTarget::Register(dst),
                value: Expression::Value(Value::Register(src)),
            } if iter_aliases.contains(src) => {
                iter_aliases.push(*dst);
                idx += 1;
            }
            // x = undefined (the sentinel constant), keep it, it may be read later.
            Statement::Assign {
                target: AssignTarget::Register(_),
                value: Expression::Value(Value::Constant(crate::ir::Constant::Undefined)),
            } => {
                kept.push(stmt.clone());
                idx += 1;
            }
            // An unrelated register copy in the plumbing. HBC ≥98 copies the
            // source into the index slot (`Mov idx, src`) BEFORE `IteratorNext`,
            // which isn't an iterator alias, keep it and keep scanning so the
            // following `val = iter.next()` is still recognised.
            Statement::Assign {
                target: AssignTarget::Register(_),
                value: Expression::Value(Value::Register(_)),
            } => {
                kept.push(stmt.clone());
                idx += 1;
            }
            // loop label / offset comments left by structure recovery
            Statement::Comment(_) => idx += 1,
            _ => break,
        }
    }
    let val_reg = val_reg?;

    // while (<iter-alias> !== <undefined>) { body }
    let body = match stmts.get(idx)? {
        Statement::While { condition, body } if is_iter_done_check(condition, &iter_aliases) => body,
        _ => return None,
    };

    // Strip the iterator try/return plumbing and the trailing `// continue`.
    let mut loop_body = unwrap_iterator_body(body, iter_reg);
    // Rename the value register to a named loop variable.
    let var_name = format!("item{val_reg}");
    let mut map = BTreeMap::new();
    map.insert(val_reg, var_name.clone());
    loop_body = rename_registers(loop_body, &map);

    kept.push(Statement::ForOf {
        variable: var_name,
        iterable: source,
        body: detect_for_of_loops(loop_body),
    });
    Some((idx + 1, kept))
}

// `iter_reg.next()`
fn is_next_call(expr: &Expression, iter_reg: u32) -> bool {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.is_empty() {
            if let Expression::Member { object, property: PropertyKey::Ident(p), .. } = callee.as_ref() {
                if p == "next" {
                    if let Expression::Value(Value::Register(r)) = object.as_ref() {
                        return *r == iter_reg;
                    }
                }
            }
        }
    }
    false
}

// `<iter-alias> !== <undefined>`, the iterator done-check. The right side may be
// a literal `undefined` or a register holding it; the iter side is one of the
// tracked aliases. We only require a `!==` touching an iterator alias, since the
// preceding `iter.next()` already established this is an iterator loop.
fn is_iter_done_check(expr: &Expression, iter_aliases: &[u32]) -> bool {
    use crate::ir::{BinaryOp, UnaryOp};
    let touches_iter = |e: &Expression| {
        matches!(e, Expression::Value(Value::Register(r)) if iter_aliases.contains(r))
    };
    match expr {
        // iter !== undefined
        Expression::Binary { op: BinaryOp::StrictNeq, left, right } => {
            touches_iter(left) || touches_iter(right)
        }
        // !(iter === undefined)
        Expression::Unary { op: UnaryOp::Not, operand } => {
            if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = operand.as_ref() {
                touches_iter(left) || touches_iter(right)
            } else {
                false
            }
        }
        _ => false,
    }
}
