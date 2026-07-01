// Reconstruct ES6 array destructuring (`var [a,b,c] = src`, holes `[p,,q]`) for
// HBC >=97, on the post-naming whole-program IR.
//
// After structure recovery + the iterator-cleanup-handler skip, v98 array
// destructuring is a flat iterator protocol:
//
//   iter = SRC[Symbol.iterator]();
//   let r0; if (iter !== undefined) { r0 = iter.next(); }   // element 0 advance
//   TARGET0 = r0;                                            // element 0 binding
//   let r1; ...noise...; if (...) { r1 = iter.next(); }      // element 1 advance
//   TARGET1 = r1;
//   ...                                                      // a hole is an advance
//   if (...) { iter.next(); ...; }                           //   with NO binding
//   if (cond) { iter.return(); }                             // close
//   ...rest...
//
// We collect one slot per `iter.next()` advance (in order), attach the binding
// target when the advance's result is later assigned to a non-scratch l-value,
// and emit `[TARGET0, TARGET1, , ...] = SRC`. Conservative: any deviation leaves
// the statements untouched.

use crate::ir::{
    map_nested_bodies, AssignTarget, Expression, PropertyKey, Statement, Value,
};

// One destructuring element: a binding target (+ optional default), or a hole.
type ElementSlots = Vec<Option<(AssignTarget, Option<Expression>)>>;

pub fn reconstruct_v98_array_destructuring(stmts: Vec<Statement>) -> Vec<Statement> {
    // Recurse into nested blocks first (destructuring can appear inside any body).
    let stmts: Vec<Statement> = stmts
        .into_iter()
        .map(|s| map_nested_bodies(s, reconstruct_v98_array_destructuring))
        .collect();

    let mut out: Vec<Statement> = Vec::with_capacity(stmts.len());
    let mut i = 0;
    while i < stmts.len() {
        if let Some((iter, src)) = iterator_anchor(&stmts[i]) {
            if let Some((slots, close_idx)) = collect(&stmts, i + 1, &iter) {
                if slots.iter().any(|s| s.is_some()) {
                    out.push(Statement::Assign {
                        target: AssignTarget::DestructuringArray(slots),
                        value: src,
                    });
                    // Continue after the close; reconstruct the remainder too.
                    let rest = reconstruct_v98_array_destructuring(stmts[close_idx + 1..].to_vec());
                    out.extend(rest);
                    return out;
                }
            }
        }
        out.push(stmts[i].clone());
        i += 1;
    }
    out
}

// `iter = SRC[Symbol.iterator]()` → (iter l-value as expression, SRC).
fn iterator_anchor(stmt: &Statement) -> Option<(Expression, Expression)> {
    let (target, value) = match stmt {
        Statement::Assign { target: AssignTarget::Register(r), value } => {
            (Expression::Value(Value::Register(*r)), value)
        }
        Statement::Assign { target: AssignTarget::Variable(n), value } => {
            (Expression::Value(Value::Variable(n.clone())), value)
        }
        Statement::Let { name, value, .. } => {
            (Expression::Value(Value::Variable(name.clone())), value)
        }
        _ => return None,
    };
    symbol_iterator_source(value).map(|src| (target, src))
}

// Match `SRC[Symbol.iterator]()` → SRC.
fn symbol_iterator_source(expr: &Expression) -> Option<Expression> {
    let Expression::Call { callee, arguments } = expr else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }
    let Expression::Member { object, property: PropertyKey::Computed(computed), .. } =
        callee.as_ref()
    else {
        return None;
    };
    if let Expression::Member { object: sym, property, .. } = computed.as_ref() {
        let is_iter = matches!(property, PropertyKey::Ident(p) | PropertyKey::String(p) if p == "iterator");
        let is_symbol = matches!(sym.as_ref(), Expression::Value(Value::Variable(s)) if s == "Symbol");
        if is_iter && is_symbol {
            return Some((**object).clone());
        }
    }
    None
}

// Collect element slots from the protocol body until the iterator close.
// Returns (slots, close_index) or None if the close isn't found.
fn collect(stmts: &[Statement], start: usize, iter: &Expression) -> Option<(ElementSlots, usize)> {
    let mut slots: ElementSlots = Vec::new();
    // value-expression of an advance result -> slot index
    let mut result_to_slot: Vec<(Expression, usize)> = Vec::new();
    let mut close_idx = None;

    for (off, stmt) in stmts[start..].iter().enumerate() {
        let idx = start + off;
        if is_close(stmt, iter) {
            close_idx = Some(idx);
            break;
        }
        if let Some(result) = advance_result(stmt, iter) {
            let slot = slots.len();
            slots.push(None);
            if let Some(rv) = result {
                result_to_slot.push((rv, slot));
            }
            continue;
        }
        // A binding `TARGET = <advance result>`.
        if let Statement::Assign { target, value } = stmt {
            if let Some(&(_, slot)) = result_to_slot.iter().find(|(v, _)| v == value) {
                if !is_scratch_target(target) {
                    slots[slot] = Some((target.clone(), None));
                    continue;
                }
            }
        }
        // Anything else is iterator bookkeeping noise (declarations, done flags);
        // skip it. Bail only if we hit another anchor (shouldn't, handled by caller).
    }

    close_idx.map(|c| (slots, c))
}

// If `stmt` performs `... = iter.next()` (or a bare `iter.next()`), return
// Some(Some(resultExpr)) / Some(None) for a hole advance; else None. Recurses
// into if/block bodies since the advance is guarded.
fn advance_result(stmt: &Statement, iter: &Expression) -> Option<Option<Expression>> {
    match stmt {
        Statement::Assign { target: AssignTarget::Register(r), value } if is_iter_next(value, iter) => {
            Some(Some(Expression::Value(Value::Register(*r))))
        }
        Statement::Assign { target: AssignTarget::Variable(n), value } if is_iter_next(value, iter) => {
            Some(Some(Expression::Value(Value::Variable(n.clone()))))
        }
        Statement::Let { name, value, .. } if is_iter_next(value, iter) => {
            Some(Some(Expression::Value(Value::Variable(name.clone()))))
        }
        Statement::Expr(e) if is_iter_next(e, iter) => Some(None),
        Statement::If { then_body, else_body, .. } => {
            // The advance is somewhere inside the guard; the result is whichever
            // branch assigns iter.next().
            for s in then_body.iter().chain(else_body.iter()) {
                if let Some(r) = advance_result(s, iter) {
                    return Some(r);
                }
            }
            None
        }
        Statement::Block(inner) => {
            for s in inner {
                if let Some(r) = advance_result(s, iter) {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

// `iter.next()` with no arguments.
fn is_iter_next(expr: &Expression, iter: &Expression) -> bool {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.is_empty() {
            if let Expression::Member { object, property, .. } = callee.as_ref() {
                let is_next = matches!(property, PropertyKey::Ident(p) | PropertyKey::String(p) if p == "next");
                return is_next && **object == *iter;
            }
        }
    }
    false
}

// The close: an `if` whose body calls `iter.return()`.
fn is_close(stmt: &Statement, iter: &Expression) -> bool {
    if let Statement::If { then_body, else_body, .. } = stmt {
        return then_body.iter().chain(else_body.iter()).any(|s| is_iter_return(s, iter));
    }
    is_iter_return(stmt, iter)
}

fn is_iter_return(stmt: &Statement, iter: &Expression) -> bool {
    if let Statement::Expr(Expression::Call { callee, arguments }) = stmt {
        if arguments.is_empty() {
            if let Expression::Member { object, property, .. } = callee.as_ref() {
                let is_ret = matches!(property, PropertyKey::Ident(p) | PropertyKey::String(p) if p == "return");
                return is_ret && **object == *iter;
            }
        }
    }
    false
}

// A scratch register/tmp l-value is bookkeeping, not a destructuring target.
fn is_scratch_target(t: &AssignTarget) -> bool {
    match t {
        AssignTarget::Register(_) => true,
        AssignTarget::Variable(n) => n.starts_with("tmp") || n.starts_with('r') && n[1..].chars().all(|c| c.is_ascii_digit()),
        _ => false,
    }
}
