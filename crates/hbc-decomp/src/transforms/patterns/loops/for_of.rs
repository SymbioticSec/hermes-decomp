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

// `obj[Symbol.iterator]()` -> Some(obj)
fn is_iterator_call(expr: &Expression) -> Option<Expression> {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.is_empty() {
            if let Expression::Member { object, property: PropertyKey::Computed(computed), .. } = callee.as_ref() {
                if let Expression::Member { object: sym, property: PropertyKey::Ident(p), .. } = computed.as_ref() {
                    if let Expression::Value(Value::Variable(n)) = sym.as_ref() {
                        if n == "Symbol" && p == "iterator" {
                            return Some((**object).clone());
                        }
                    }
                }
            }
        }
    }
    None
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

// Remove the `try { body } catch { iter.return(); throw }` wrapper and the
// trailing `// continue` marker that the iterator lowering leaves behind.
fn unwrap_iterator_body(body: &[Statement], _iter_reg: u32) -> Vec<Statement> {
    let inner: Vec<Statement> = if body.len() == 1 {
        match &body[0] {
            Statement::TryCatch { try_body, .. } => try_body.clone(),
            _ => body.to_vec(),
        }
    } else {
        body.to_vec()
    };
    inner
        .into_iter()
        .filter(|s| !matches!(s, Statement::Comment(c) if c == "continue"))
        .collect()
}

// ---------------------------------------------------------------------------
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
// ---------------------------------------------------------------------------

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
