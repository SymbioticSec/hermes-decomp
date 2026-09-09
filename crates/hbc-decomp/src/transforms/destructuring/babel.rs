// Reconstruct ES6 array destructuring from the form Babel lowers it to.
//
// Discord and most React Native apps run Babel before Hermes, so `const [a, b] =
// src` never reaches the bytecode. What arrives instead is a call to a runtime
// helper followed by one indexed read per binding:
//
//   let tmp = _slicedToArray(SRC, 2);
//   let a = tmp[0];
//   let b = tmp[1];
//
// which is `const [a, b] = SRC`. The helper itself stays in its own module and is
// untouched here, only its call sites fold.
//
// Conservative on every axis. The temporary must be read at distinct constant
// indices, the reads must follow the call without anything in between, and the
// temporary must appear nowhere else in the function. Anything else is left
// exactly as it was, because a wrong fold would move a read across a side effect.

use crate::ir::{
    map_nested_bodies, AssignTarget, Expression, PropertyKey, Statement, Value, Visitor,
};

// Helpers whose result is an array of the requested length, one entry per binding.
const ARRAY_HELPERS: &[&str] = &["_slicedToArray", "_toArray", "_toConsumableArray"];

type ElementSlots = Vec<Option<(AssignTarget, Option<Expression>)>>;

pub fn reconstruct_babel_array_destructuring(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts: Vec<Statement> = stmts
        .into_iter()
        .map(|s| map_nested_bodies(s, reconstruct_babel_array_destructuring))
        .collect();

    let mut out: Vec<Statement> = Vec::with_capacity(stmts.len());
    let mut i = 0;
    while i < stmts.len() {
        if let Some((tmp, src)) = helper_anchor(&stmts[i]) {
            if let Some((slots, consumed, kept)) = collect_reads(&stmts, i + 1, &tmp) {
                // The temporary must not survive the fold anywhere else.
                let used_before = stmts[..i].iter().any(|s| reads_name(s, &tmp));
                let used_after = stmts[i + 1 + consumed..].iter().any(|s| reads_name(s, &tmp));
                if !used_before && !used_after && slots.iter().any(|s| s.is_some()) {
                    out.push(Statement::Assign {
                        target: AssignTarget::DestructuringArray(slots),
                        value: src,
                    });
                    // Statements that merely sat between the reads are kept, in
                    // their original order, right after the binding they follow.
                    for sk in kept {
                        out.push(stmts[sk].clone());
                    }
                    i += 1 + consumed;
                    continue;
                }
            }
        }
        out.push(unwrap_helper_source(stmts[i].clone()));
        i += 1;
    }
    out
}

// `[a, b] = _slicedToArray(SRC, n)` → `[a, b] = SRC`. The destructuring is already
// reconstructed here, so the helper only stands between the pattern and the value
// it reads. Dropping it changes nothing about what is bound.
fn unwrap_helper_source(stmt: Statement) -> Statement {
    let Statement::Assign { target, value } = stmt else {
        return stmt;
    };
    let is_pattern = matches!(
        target,
        AssignTarget::DestructuringArray(_) | AssignTarget::DestructuringArrayRest { .. }
    );
    if !is_pattern {
        return Statement::Assign { target, value };
    }
    if let Expression::Call { callee, arguments } = &value {
        if let Expression::Value(Value::Variable(name)) = callee.as_ref() {
            if ARRAY_HELPERS.contains(&name.as_str()) {
                if let Some(src) = arguments.first() {
                    return Statement::Assign {
                        target,
                        value: src.clone(),
                    };
                }
            }
        }
    }
    Statement::Assign { target, value }
}

// `tmp = _slicedToArray(SRC, n)` → (temporary name, SRC).
fn helper_anchor(stmt: &Statement) -> Option<(String, Expression)> {
    let (name, value) = match stmt {
        Statement::Let { name, value, .. } => (name, value),
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value,
        } => (name, value),
        _ => return None,
    };
    let Expression::Call { callee, arguments } = value else {
        return None;
    };
    let Expression::Value(Value::Variable(callee_name)) = callee.as_ref() else {
        return None;
    };
    if !ARRAY_HELPERS.contains(&callee_name.as_str()) {
        return None;
    }
    let src = arguments.first()?.clone();
    Some((name.clone(), src))
}

// `TARGET = tmp[i]` reads following the call, and how far the run reaches.
//
// Babel leaves ordinary statements between the reads, so the run is not required
// to be contiguous. A statement may sit inside it only when it touches neither
// the temporary nor a name a later read binds: the reads move up to the call, and
// that is unobservable exactly when nothing in between depends on them.
fn collect_reads(
    stmts: &[Statement],
    start: usize,
    tmp: &str,
) -> Option<(ElementSlots, usize, Vec<usize>)> {
    let mut found: Vec<(usize, AssignTarget)> = Vec::new();
    let mut read_positions: Vec<(usize, AssignTarget)> = Vec::new();
    let mut skipped: Vec<usize> = Vec::new();
    let mut idx = start;
    let mut last_read = start;
    while idx < stmts.len() {
        if let Some((target, n)) = indexed_read(&stmts[idx], tmp) {
            if found.iter().any(|(seen, _)| *seen == n) {
                return None;
            }
            read_positions.push((idx, target.clone()));
            found.push((n, target));
            idx += 1;
            last_read = idx;
            continue;
        }
        // Not a read. It may be stepped over only if it leaves the temporary alone.
        if reads_name(&stmts[idx], tmp) {
            break;
        }
        skipped.push(idx);
        idx += 1;
    }
    // Only the run up to the final read matters; anything after it is untouched.
    let consumed = last_read - start;
    if found.is_empty() {
        return None;
    }
    // A stepped over statement must not mention a name bound by a read that comes
    // after it, since that binding is the one moving up past it. A name bound
    // earlier already sat above the statement and does not move.
    for &sk in skipped.iter().filter(|&&sk| sk < last_read) {
        for (pos, target) in &read_positions {
            if *pos < sk {
                continue;
            }
            if let AssignTarget::Variable(name) = target {
                if reads_name(&stmts[sk], name) {
                    return None;
                }
            }
        }
    }
    let highest = *found.iter().map(|(n, _)| n).max()?;
    let mut slots: ElementSlots = vec![None; highest + 1];
    for (n, target) in found {
        slots[n] = Some((target, None));
    }
    let kept: Vec<usize> = skipped.into_iter().filter(|&sk| sk < last_read).collect();
    Some((slots, consumed, kept))
}

// `TARGET = tmp[N]` → (TARGET, N) for a non negative constant N.
fn indexed_read(stmt: &Statement, tmp: &str) -> Option<(AssignTarget, usize)> {
    let (target, value) = match stmt {
        Statement::Let { name, value, .. } => (AssignTarget::Variable(name.clone()), value),
        Statement::Assign { target, value } => (target.clone(), value),
        _ => return None,
    };
    let Expression::Member { object, property, .. } = value else {
        return None;
    };
    let Expression::Value(Value::Variable(name)) = object.as_ref() else {
        return None;
    };
    if name != tmp {
        return None;
    }
    let n = match property {
        PropertyKey::Index(n) if *n >= 0 => *n as usize,
        _ => return None,
    };
    Some((target, n))
}

// Whether the statement mentions `name` at all, as a read or as a write.
fn reads_name(stmt: &Statement, name: &str) -> bool {
    struct V<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'b> Visitor<'b> for V<'_> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                if n == self.name {
                    self.found = true;
                }
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &'b AssignTarget) {
            if let AssignTarget::Variable(n) = t {
                if n == self.name {
                    self.found = true;
                }
            }
            self.walk_assign_target(t);
        }
        fn visit_statement(&mut self, s: &'b Statement) {
            if let Statement::Let { name, .. } = s {
                if name == self.name {
                    self.found = true;
                }
            }
            self.walk_statement(s);
        }
    }
    let mut v = V { name, found: false };
    v.visit_statement(stmt);
    v.found
}

#[cfg(test)]
mod tests {
    use super::reconstruct_babel_array_destructuring;
    use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value, VarKind};

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Variable(n.into()))
    }

    fn helper_call(src: &str, n: i64) -> Expression {
        Expression::Call {
            callee: Box::new(var("_slicedToArray")),
            arguments: vec![
                var(src),
                Expression::Value(Value::Constant(crate::ir::Constant::Integer(n as i32))),
            ],
        }
    }

    fn let_stmt(name: &str, value: Expression) -> Statement {
        Statement::Let {
            name: name.into(),
            value,
            kind: VarKind::Let,
        }
    }

    fn index_read(tmp: &str, i: i64) -> Expression {
        Expression::Member {
            object: Box::new(var(tmp)),
            property: PropertyKey::Index(i),
            optional: false,
        }
    }

    fn render(stmts: &[Statement]) -> String {
        stmts.iter().map(|s| format!("{s}")).collect()
    }

    #[test]
    fn folds_the_helper_and_its_reads_into_one_pattern() {
        let out = reconstruct_babel_array_destructuring(vec![
            let_stmt("tmp", helper_call("src", 2)),
            let_stmt("a", index_read("tmp", 0)),
            let_stmt("b", index_read("tmp", 1)),
        ]);
        let text = render(&out);
        assert!(text.contains("[a, b] = src"), "{text}");
        assert!(!text.contains("_slicedToArray"), "{text}");
    }

    #[test]
    fn a_statement_between_the_reads_survives() {
        // The reads move up to the call, so anything sitting between them has to be
        // re-emitted. Dropping it would silently delete code.
        let out = reconstruct_babel_array_destructuring(vec![
            let_stmt("tmp", helper_call("src", 2)),
            let_stmt("a", index_read("tmp", 0)),
            let_stmt("keepMe", var("somethingElse")),
            let_stmt("b", index_read("tmp", 1)),
        ]);
        let text = render(&out);
        assert!(text.contains("keepMe"), "the in between statement was lost: {text}");
        assert!(text.contains("[a, b] = src"), "{text}");
    }

    #[test]
    fn a_temporary_read_elsewhere_is_left_alone() {
        // `tmp` outlives the reads, so folding it away would lose the later use.
        let out = reconstruct_babel_array_destructuring(vec![
            let_stmt("tmp", helper_call("src", 3)),
            let_stmt("a", index_read("tmp", 0)),
            let_stmt("later", var("tmp")),
        ]);
        let text = render(&out);
        assert!(text.contains("_slicedToArray"), "{text}");
        assert!(text.contains("later"), "{text}");
    }

    #[test]
    fn an_already_reconstructed_pattern_drops_the_helper() {
        let out = reconstruct_babel_array_destructuring(vec![Statement::Assign {
            target: AssignTarget::DestructuringArray(vec![
                Some((AssignTarget::Variable("a".into()), None)),
                Some((AssignTarget::Variable("b".into()), None)),
            ]),
            value: helper_call("src", 2),
        }]);
        let text = render(&out);
        assert!(text.contains("= src"), "{text}");
        assert!(!text.contains("_slicedToArray"), "{text}");
    }
}
