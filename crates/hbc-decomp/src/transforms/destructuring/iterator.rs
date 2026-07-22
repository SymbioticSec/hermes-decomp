// Reconstruct array destructuring from the Hermes iterator protocol.
//
// `var [a, b, c] = src` (and holes like `[p, , q]`) compile to:
//   iter = src[Symbol.iterator]()        (IteratorBegin)
//   v0 = iter.next()                      (IteratorNext)
//   done = iter === undefined
//   a = undefined; if (done) {} else { a = v0 }
//   ... per element, the next/assign nested under the done guard ...
//   if (!done) { iter.return() }          (IteratorClose)
//
// We match the whole block and emit `[a, b, c] = src`. Each element's value is
// threaded through registers (often a single shared "current value" register);
// the destructuring target is whichever register holds that value AND is read
// AFTER the block (the real binding). An element whose value never reaches such a
// register is a hole (`,`).

use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value, Visitor};
use std::collections::{HashMap, HashSet};

pub fn detect_iterator_destructuring(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = recurse(stmts);

    // Register -> defining expression for this level (used to resolve the legacy
    // `iter = src[Symbol.iterator].call(src)` / `next = iter.next` chains).
    let mut defs: HashMap<u32, Expression> = HashMap::new();
    for s in &stmts {
        if let Statement::Assign { target: AssignTarget::Register(r), value } = s {
            defs.insert(*r, value.clone());
        }
    }

    let mut result: Vec<Statement> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        // Modern protocol: `iter = src[Symbol.iterator]()`, value-direct next.
        // Legacy protocol (HBC < 74): `.call` forms with {value,done} results.
        let begin = iterator_begin(&stmts[i])
            .map(|(r, s)| (r, s, None))
            .or_else(|| legacy_iterator_begin(&stmts[i], &defs).map(|(r, s, n)| (r, s, Some(n))));
        if let Some((iter_reg, src, next_reg)) = begin {
            if let Some(close) = find_close(&stmts, i + 1, iter_reg) {
                let used_after = registers_used_in(&stmts[close + 1..]);
                let elements =
                    collect_elements(&stmts[i + 1..=close], iter_reg, next_reg, &used_after);
                // Reconstruct only when every element is bound to a distinct
                // register (holes excluded). Otherwise leave the iterator form.
                let bound: Vec<u32> = elements.iter().flatten().copied().collect();
                let unique: HashSet<u32> = bound.iter().copied().collect();
                let all_resolved = !elements.is_empty()
                    && unique.len() == bound.len()
                    && elements.iter().any(|e| e.is_some());
                if all_resolved {
                    // Legacy begin is preceded by the `Symbol.iterator` access
                    // chain (`c = Symbol; c = Symbol.iterator; c = src[c]`) on the
                    // (reused) callee register. Those defs are now dead but survive
                    // dead-elim because the register is reused later, so drop the
                    // trailing access-chain assignments here.
                    if next_reg.is_some() {
                        if let Some(c) = begin_callee_reg(&stmts[i]) {
                            while matches!(
                                result.last(),
                                Some(Statement::Assign {
                                    target: AssignTarget::Register(r),
                                    value: Expression::Member { .. },
                                }) if *r == c
                            ) {
                                result.pop();
                            }
                        }
                    }
                    let targets: Vec<Option<(AssignTarget, Option<Expression>)>> = elements
                        .into_iter()
                        .map(|e| e.map(|r| (AssignTarget::Register(r), None)))
                        .collect();
                    result.push(Statement::Assign {
                        target: AssignTarget::DestructuringArray(targets),
                        value: src,
                    });
                    i = close + 1;
                    continue;
                }
            }
        }
        result.push(stmts[i].clone());
        i += 1;
    }
    result
}

fn recurse(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: detect_iterator_destructuring(then_body),
                else_body: detect_iterator_destructuring(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition,
                body: detect_iterator_destructuring(body),
            },
            Statement::DoWhile { body, condition } => Statement::DoWhile {
                body: detect_iterator_destructuring(body),
                condition,
            },
            Statement::For { init, condition, update, body } => Statement::For {
                init,
                condition,
                update,
                body: detect_iterator_destructuring(body),
            },
            Statement::ForOf { variable, iterable, body } => Statement::ForOf {
                variable,
                iterable,
                body: detect_iterator_destructuring(body),
            },
            Statement::ForIn { variable, object, body } => Statement::ForIn {
                variable,
                object,
                body: detect_iterator_destructuring(body),
            },
            Statement::Block(inner) => Statement::Block(detect_iterator_destructuring(inner)),
            Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => {
                Statement::TryCatch {
                    try_body: detect_iterator_destructuring(try_body),
                    catch_param,
                    catch_body: detect_iterator_destructuring(catch_body),
                    finally_body: detect_iterator_destructuring(finally_body),
                }
            }
            other => other,
        })
        .collect()
}

// `iter = src[Symbol.iterator]()` -> (iter_reg, src).
fn iterator_begin(stmt: &Statement) -> Option<(u32, Expression)> {
    if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
        if let Expression::Call { callee, arguments } = value {
            if arguments.is_empty() {
                if let Expression::Member { object, property: PropertyKey::Computed(c), .. } =
                    callee.as_ref()
                {
                    if let Expression::Member { object: sym, property: PropertyKey::Ident(p), .. } =
                        c.as_ref()
                    {
                        if p == "iterator"
                            && matches!(sym.as_ref(), Expression::Value(Value::Variable(s)) if s == "Symbol")
                        {
                            return Some((*r, (**object).clone()));
                        }
                    }
                }
            }
        }
    }
    None
}

// Legacy iterator begin. Hermes (HBC < 74) lowers `[a,b] = src` to
// `iter = src[Symbol.iterator].call(src)`, in IR a `Call` of the iterator
// method with `src` as the sole (this) argument. The iterator register is fresh
// (never reused), so we read `src` directly from the call argument and resolve
// `next = iter.next` from the def map. We deliberately do NOT verify the
// `Symbol.iterator` access here: the access register is heavily reused, so the
// def map can't reconstruct it. The presence of `iter.next`, `result.done`,
// `result.value`, and `iter.return` (checked by the caller) is what confirms
// this is really the iterator protocol.
fn legacy_iterator_begin(
    stmt: &Statement,
    defs: &HashMap<u32, Expression>,
) -> Option<(u32, Expression, u32)> {
    let (iter_reg, src) = match stmt {
        Statement::Assign {
            target: AssignTarget::Register(r),
            value: Expression::Call { callee, arguments },
        } if arguments.len() == 1 && reg_of(callee).is_some() => (*r, arguments[0].clone()),
        _ => return None,
    };
    // Require `next = iter.next` to exist (the protocol's per-element advance).
    let next_reg = defs.iter().find_map(|(&r, v)| match v {
        Expression::Member { object, property: PropertyKey::Ident(p), .. }
            if p == "next" && reg_of(object) == Some(iter_reg) =>
        {
            Some(r)
        }
        _ => None,
    })?;
    Some((iter_reg, src, next_reg))
}

fn reg_of(e: &Expression) -> Option<u32> {
    match e {
        Expression::Value(Value::Register(r)) => Some(*r),
        _ => None,
    }
}

// The callee register of a legacy iterator-begin call statement.
fn begin_callee_reg(stmt: &Statement) -> Option<u32> {
    if let Statement::Assign { value: Expression::Call { callee, .. }, .. } = stmt {
        return reg_of(callee);
    }
    None
}

// Legacy `result = next.call(iter)`, opens an element, binding `result`.
fn is_legacy_next(value: &Expression, next_reg: u32, iter_reg: u32) -> bool {
    if let Expression::Call { callee, arguments } = value {
        return reg_of(callee) == Some(next_reg)
            && arguments.len() == 1
            && reg_of(&arguments[0]) == Some(iter_reg);
    }
    false
}

// Legacy `elem = result.value`, returns the result register.
fn legacy_value_source(value: &Expression) -> Option<u32> {
    if let Expression::Member { object, property: PropertyKey::Ident(p), .. } = value {
        if p == "value" {
            return reg_of(object);
        }
    }
    None
}

fn is_iter_next(value: &Expression, iter_reg: u32) -> bool {
    if let Expression::Call { callee, arguments } = value {
        if arguments.is_empty() {
            if let Expression::Member { object, property: PropertyKey::Ident(p), .. } =
                callee.as_ref()
            {
                return p == "next"
                    && matches!(object.as_ref(), Expression::Value(Value::Register(r)) if *r == iter_reg);
            }
        }
    }
    false
}

fn is_iter_return(stmt: &Statement, iter_reg: u32) -> bool {
    let expr = match stmt {
        Statement::Expr(e) | Statement::Assign { value: e, .. } => e,
        _ => return false,
    };
    // Modern: `iter.return()`. Legacy: `tmp = iter.return` (the method is then
    // called under an `if (tmp !== undefined)` guard).
    match expr {
        Expression::Call { callee, arguments } if arguments.is_empty() => {
            matches!(
                callee.as_ref(),
                Expression::Member { object, property: PropertyKey::Ident(p), .. }
                    if p == "return"
                        && matches!(object.as_ref(), Expression::Value(Value::Register(r)) if *r == iter_reg)
            )
        }
        Expression::Member { object, property: PropertyKey::Ident(p), .. } => {
            p == "return"
                && matches!(object.as_ref(), Expression::Value(Value::Register(r)) if *r == iter_reg)
        }
        _ => false,
    }
}

fn find_close(stmts: &[Statement], start: usize, iter_reg: u32) -> Option<usize> {
    for (off, stmt) in stmts[start..].iter().enumerate() {
        if stmt_contains_iter_return(stmt, iter_reg) {
            return Some(start + off);
        }
    }
    None
}

fn stmt_contains_iter_return(stmt: &Statement, iter_reg: u32) -> bool {
    if is_iter_return(stmt, iter_reg) {
        return true;
    }
    match stmt {
        Statement::If { then_body, else_body, .. } => {
            then_body.iter().any(|s| stmt_contains_iter_return(s, iter_reg))
                || else_body.iter().any(|s| stmt_contains_iter_return(s, iter_reg))
        }
        Statement::Block(inner) => inner.iter().any(|s| stmt_contains_iter_return(s, iter_reg)),
        _ => false,
    }
}

// Registers referenced (read) anywhere in `stmts`.
fn registers_used_in(stmts: &[Statement]) -> HashSet<u32> {
    struct C(HashSet<u32>);
    impl<'b> Visitor<'b> for C {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Register(r)) = e {
                self.0.insert(*r);
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(HashSet::new());
    for s in stmts {
        c.visit_statement(s);
    }
    c.0
}

// Walk the block in program order (recursing into guards) tracking, for each
// register, which destructuring element's value it currently holds. Each
// `iter.next()` opens a new element; a register read after the block that holds
// an element's value is that element's target. Returns one entry per element
// (`Some(reg)` bound, `None` hole).
fn collect_elements(
    stmts: &[Statement],
    iter_reg: u32,
    next_reg: Option<u32>,
    used_after: &HashSet<u32>,
) -> Vec<Option<u32>> {
    let mut st = WalkState {
        iter_reg,
        next_reg,
        used_after,
        elements: Vec::new(),
        reg_to_elem: HashMap::new(),
        result_to_elem: HashMap::new(),
    };
    st.walk(stmts);
    st.elements
}

struct WalkState<'a> {
    iter_reg: u32,
    // `Some(next)` selects the legacy {value,done} protocol; `None` modern.
    next_reg: Option<u32>,
    used_after: &'a HashSet<u32>,
    elements: Vec<Option<u32>>,
    // register holding an element's VALUE -> element index
    reg_to_elem: HashMap<u32, usize>,
    // (legacy) register holding a `.next()` RESULT object -> element index
    result_to_elem: HashMap<u32, usize>,
}

impl WalkState<'_> {
    fn walk(&mut self, stmts: &[Statement]) {
        for stmt in stmts {
            match stmt {
                Statement::Assign { target: AssignTarget::Register(dst), value } => {
                    if let Some(next_reg) = self.next_reg {
                        // Legacy: `result = next.call(iter)` opens an element;
                        // `elem = result.value` binds it.
                        if is_legacy_next(value, next_reg, self.iter_reg) {
                            let idx = self.elements.len();
                            self.elements.push(None);
                            self.result_to_elem.insert(*dst, idx);
                            continue;
                        }
                        if let Some(res) = legacy_value_source(value) {
                            if let Some(&idx) = self.result_to_elem.get(&res) {
                                self.reg_to_elem.insert(*dst, idx);
                                if self.used_after.contains(dst) {
                                    self.elements[idx] = Some(*dst);
                                }
                                continue;
                            }
                        }
                    } else if is_iter_next(value, self.iter_reg) {
                        // Modern: the next() result register holds the value.
                        let idx = self.elements.len();
                        self.elements.push(None);
                        self.reg_to_elem.insert(*dst, idx);
                        continue;
                    }

                    if let Expression::Value(Value::Register(src)) = value {
                        // Copy: dst inherits src's element (if any).
                        if let Some(&idx) = self.reg_to_elem.get(src) {
                            self.reg_to_elem.insert(*dst, idx);
                            if self.used_after.contains(dst) {
                                self.elements[idx] = Some(*dst);
                            }
                        } else {
                            self.reg_to_elem.remove(dst);
                        }
                    } else {
                        // dst overwritten with something unrelated.
                        self.reg_to_elem.remove(dst);
                    }
                }
                Statement::If { then_body, else_body, .. } => {
                    self.walk(then_body);
                    self.walk(else_body);
                }
                Statement::Block(inner) => self.walk(inner),
                _ => {}
            }
        }
    }
}
