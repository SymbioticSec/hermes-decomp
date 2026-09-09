// Legacy iterator protocol (HBC 59-71)
//
// Before IteratorBegin/IteratorNext/IteratorClose existed (HBC < 74), Hermes
// lowered `for (x of src)` to the spec's full {value,done} protocol. After IR
// build + structure recovery the shape is:
//
//   iter   = src[Symbol.iterator].call(src)          // get iterator
//   HermesInternal.ensureObject(iter, "...")
//   next   = iter.next
//   result = next.call(iter)                          // first .next()
//   HermesInternal.ensureObject(result, "...")
//   done   = result.done
//   while (!done) {
//     x = result.value
//     try { <body>; // continue } catch (e) { iter.return?.(); throw e }
//   }                                                 // back-edge re-runs .next()
//
// We match the `while`, trace `done`→`result`→`iter`→`src`, drop the protocol
// plumbing, and emit `for (x of src) { <body> }`. The per-iteration `.next()`
// (re-run via the loop back-edge) is reintroduced by for-of semantics.

use super::{is_iterator_call, unwrap_iterator_body};
use crate::analysis::rename_registers;
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};

pub fn detect_legacy_for_of(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = recurse_legacy(stmts);

    // Build a register -> defining-expression map for this statement level.
    let mut defs: HashMap<u32, Expression> = HashMap::new();
    for s in &stmts {
        if let Statement::Assign { target: AssignTarget::Register(r), value } = s {
            defs.insert(*r, value.clone());
        }
    }

    // Find the first `while` that matches the legacy protocol.
    let mut found: Option<(usize, LegacyForOf)> = None;
    for (i, s) in stmts.iter().enumerate() {
        if let Statement::While { condition, body } = s {
            if let Some(m) = match_legacy_for_of(condition, body, &defs) {
                found = Some((i, m));
                break;
            }
        }
    }
    let (while_idx, m) = match found {
        Some(x) => x,
        None => return stmts,
    };

    // Rebuild the statement list: drop protocol-defining statements and the
    // ensureObject side-effects, and replace the `while` with the `for-of`.
    let var_name = format!("item{}", m.value_reg);
    let mut rename = BTreeMap::new();
    rename.insert(m.value_reg, var_name.clone());
    let loop_body = rename_registers(detect_legacy_for_of(m.loop_body), &rename);
    let for_of = Statement::ForOf {
        variable: var_name,
        iterable: m.iterable,
        body: loop_body,
    };

    let mut result = Vec::with_capacity(stmts.len());
    for (i, s) in stmts.into_iter().enumerate() {
        if i == while_idx {
            result.push(for_of.clone());
            continue;
        }
        // Drop statements that define a protocol register.
        if let Statement::Assign { target: AssignTarget::Register(r), .. } = &s {
            if m.protocol_regs.contains(r) {
                continue;
            }
        }
        // Drop ensureObject side-effect calls (Assign or bare Expr).
        if is_ensure_object_stmt(&s) {
            continue;
        }
        result.push(s);
    }
    result
}

struct LegacyForOf {
    iterable: Expression,
    value_reg: u32,
    protocol_regs: HashSet<u32>,
    loop_body: Vec<Statement>,
}

fn match_legacy_for_of(
    condition: &Expression,
    body: &[Statement],
    defs: &HashMap<u32, Expression>,
) -> Option<LegacyForOf> {
    use crate::ir::UnaryOp;
    // condition: `!done`
    let done_reg = match condition {
        Expression::Unary { op: UnaryOp::Not, operand } => reg_of(operand)?,
        _ => return None,
    };
    // done = result.done
    let result_reg = match defs.get(&done_reg)? {
        Expression::Member { object, property: PropertyKey::Ident(p), .. } if p == "done" => {
            reg_of(object)?
        }
        _ => return None,
    };
    // body[0]: value = result.value
    let (value_reg, rest) = match body.split_first()? {
        (Statement::Assign { target: AssignTarget::Register(v), value }, rest) => {
            match value {
                Expression::Member { object, property: PropertyKey::Ident(p), .. }
                    if p == "value" && reg_of(object) == Some(result_reg) =>
                {
                    (*v, rest)
                }
                _ => return None,
            }
        }
        _ => return None,
    };

    let mut protocol_regs: HashSet<u32> = HashSet::new();
    protocol_regs.insert(done_reg);
    protocol_regs.insert(result_reg);

    // result = next.call(iter)   OR   result = iter.next()
    let iter_reg = match defs.get(&result_reg)? {
        Expression::Call { callee, arguments } => {
            if let Some(next_reg) = reg_of(callee) {
                // next.call(iter): callee is a register holding `iter.next`
                protocol_regs.insert(next_reg);
                match defs.get(&next_reg)? {
                    Expression::Member { object, property: PropertyKey::Ident(p), .. }
                        if p == "next" =>
                    {
                        reg_of(object)?
                    }
                    _ => return None,
                }
            } else if let Expression::Member { object, property: PropertyKey::Ident(p), .. } =
                callee.as_ref()
            {
                // iter.next() directly
                if p != "next" {
                    return None;
                }
                let _ = arguments;
                reg_of(object)?
            } else {
                return None;
            }
        }
        _ => return None,
    };
    protocol_regs.insert(iter_reg);

    // iter = src[Symbol.iterator].call(src)   OR   iter = src[Symbol.iterator]()
    let iterable = match defs.get(&iter_reg)? {
        // .call(src) form: callee is a register holding `src[Symbol.iterator]`
        Expression::Call { callee, arguments }
            if reg_of(callee).is_some() && arguments.len() == 1 =>
        {
            let access_reg = reg_of(callee)?;
            protocol_regs.insert(access_reg);
            match defs.get(&access_reg)? {
                Expression::Member { object, property: PropertyKey::Computed(c), .. }
                    if is_symbol_iterator(c, defs, &mut protocol_regs) =>
                {
                    (**object).clone()
                }
                _ => return None,
            }
        }
        // direct call form
        other => is_iterator_call(other)?,
    };

    // Pull in alias registers (`r = Register(p)` where p is a protocol reg).
    let mut added = true;
    while added {
        added = false;
        for (&r, v) in defs.iter() {
            if protocol_regs.contains(&r) {
                continue;
            }
            if let Some(src) = reg_of_value(v) {
                if protocol_regs.contains(&src) {
                    protocol_regs.insert(r);
                    added = true;
                }
            }
        }
    }

    // Drop `value = result.value` (now the loop variable) and unwrap the
    // try/return iterator-cleanup wrapper around the body.
    let loop_body = unwrap_iterator_body(rest, iter_reg);

    Some(LegacyForOf { iterable, value_reg, protocol_regs, loop_body })
}

// `c` is `Symbol.iterator` (possibly via a register holding it). Records any
// intermediate registers in `protocol_regs`.
fn is_symbol_iterator(
    c: &Expression,
    defs: &HashMap<u32, Expression>,
    protocol_regs: &mut HashSet<u32>,
) -> bool {
    let resolved = if let Some(r) = reg_of(c) {
        protocol_regs.insert(r);
        match defs.get(&r) {
            Some(e) => e,
            None => return false,
        }
    } else {
        c
    };
    matches!(
        resolved,
        Expression::Member { property: PropertyKey::Ident(p), .. } if p == "iterator"
    )
}

fn reg_of(e: &Expression) -> Option<u32> {
    match e {
        Expression::Value(Value::Register(r)) => Some(*r),
        _ => None,
    }
}

fn reg_of_value(e: &Expression) -> Option<u32> {
    match e {
        Expression::Value(Value::Register(r)) => Some(*r),
        _ => None,
    }
}

// `HermesInternal.ensureObject(...)` as a standalone statement.
fn is_ensure_object_stmt(s: &Statement) -> bool {
    let expr = match s {
        Statement::Expr(e) => e,
        Statement::Assign { value, .. } => value,
        _ => return false,
    };
    if let Expression::Call { callee, .. } = expr {
        if let Expression::Member { property: PropertyKey::Ident(p), .. } = callee.as_ref() {
            return p == "ensureObject";
        }
    }
    false
}

fn recurse_legacy(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::While { condition, body } => Statement::While {
                condition,
                body: detect_legacy_for_of(body),
            },
            Statement::DoWhile { body, condition } => Statement::DoWhile {
                body: detect_legacy_for_of(body),
                condition,
            },
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: detect_legacy_for_of(then_body),
                else_body: detect_legacy_for_of(else_body),
            },
            Statement::For { init, condition, update, body } => Statement::For {
                init,
                condition,
                update,
                body: detect_legacy_for_of(body),
            },
            Statement::ForOf { variable, iterable, body } => Statement::ForOf {
                variable,
                iterable,
                body: detect_legacy_for_of(body),
            },
            Statement::ForIn { variable, object, body } => Statement::ForIn {
                variable,
                object,
                body: detect_legacy_for_of(body),
            },
            Statement::Block(inner) => Statement::Block(detect_legacy_for_of(inner)),
            Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => {
                Statement::TryCatch {
                    try_body: detect_legacy_for_of(try_body),
                    catch_param,
                    catch_body: detect_legacy_for_of(catch_body),
                    finally_body: detect_legacy_for_of(finally_body),
                }
            }
            other => other,
        })
        .collect()
}
