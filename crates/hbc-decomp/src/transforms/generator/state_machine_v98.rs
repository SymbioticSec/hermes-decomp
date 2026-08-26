// Reconstruct ES6 generators for HBC >=97.
//
// HBC v97 DELETED the dedicated generator opcodes (StartGenerator / SaveGenerator
// / ResumeGenerator / CompleteGenerator). The frontend now desugars `function*`
// into a plain-opcode state machine: a switch over two env slots (a `status` and
// a resume `label`), wrapped in guards. This pass recognizes that exact shape and
// rebuilds the flat `yield` body.
//
// Shape produced by v98 (post structure-recovery + closure resolution):
//
//   if (closure_0 === 2) { closure_0 = 3; HermesBuiltin.throwTypeError(); }   // executing guard
//   else if (tmp === 3) { ...completed-resume handling... }                   // completed guard
//   else {
//     try {
//       closure_0 = 2;                 // status = executing
//       tmp = closure_1;               // label copy (optional)
//       if (0 === closure_1) { <case 0> }
//       else if (1 === tmp) { <case 1> }
//       else if (2 === tmp) { <case 2> }
//       else { <done> }
//     } catch { ... }
//   }
//
// where each `<case N>` is:
//
//   if (arg0 === 1) { throw arg1; }                         // .throw(v)
//   else if (arg0 === 2) { return {value: arg1, done:true}; }  // .return(v)
//   else { <pre-code>; closure_1 = N+1; closure_0 = 1; return {value: V, done:false}; }  // yield V
//
// We extract `V` per label (in order) and emit `yield V`; the terminal case
// (`return {value: undefined, done: true}`) ends the body. Conservative: any
// deviation makes the whole pass bail and return the input unchanged, so an
// unrecognized generator keeps today's (raw) output rather than wrong code.

use crate::ir::{AssignTarget, BinaryOp, Constant, Expression, PropertyKey, Statement, Value};

pub fn reconstruct_generator_v98(body: Vec<Statement>) -> Vec<Statement> {
    try_reconstruct(&body).unwrap_or(body)
}

// A parsed state-machine case: the value the case resumes with (`x = <resume>`),
// the real code before the suspend point, the yielded/returned value and whether
// this is the terminal (done) case.
#[derive(Clone)]
struct ParsedCase {
    resume_binding: Option<AssignTarget>,
    pre: Vec<Statement>,
    value: Expression,
    done: bool,
}

fn try_reconstruct(body: &[Statement]) -> Option<Vec<Statement>> {
    let dispatch = find_label_dispatch(body)?;
    let cases = collect_label_cases(dispatch)?;
    // A real state machine has at least one yield label plus the terminal case.
    if cases.len() < 2 {
        return None;
    }

    // Variables compared against an integer literal anywhere in the machine are
    // the status / label slots and their copies (`c0`, `c1`, `tmp3`...). Their
    // assignments are bookkeeping and get dropped from the reconstructed body.
    let state_vars = collect_state_vars(body);

    let parsed: Vec<Option<ParsedCase>> = cases
        .iter()
        .map(|(_, b)| parse_case(b, &state_vars))
        .collect();

    // Happy path: every case parsed, only the last is terminal. Linear
    // `yield; yield; return` with no catch label in the middle.
    if parsed.iter().all(|p| p.is_some()) {
        let all: Vec<ParsedCase> = parsed.iter().flatten().cloned().collect();
        let (last, rest) = all.split_last()?;
        if last.done && rest.iter().all(|c| !c.done) {
            return emit_yield_chain(&all[..all.len() - 1], last);
        }
    }

    // Async + try/catch (forgotPassword, etc.): a catch label sits between the
    // yield case and the resume case (`else if (1 === label) { throw / return }`).
    // Keep every suspend and the *last* terminal; skip middle error labels.
    // An unparsed case is only skipped when it throws (catch-like). Anything
    // else unknown means bail, so we never drop a real yield.
    let mut suspends: Vec<ParsedCase> = Vec::new();
    let mut terminal: Option<ParsedCase> = None;
    for (p, (_, raw)) in parsed.into_iter().zip(cases.iter()) {
        match p {
            Some(c) if !c.done => suspends.push(c),
            Some(c) => terminal = Some(c),
            None if body_has_throw(raw) => {}
            None => return None,
        }
    }
    emit_yield_chain(&suspends, terminal.as_ref()?)
}

fn body_has_throw(stmts: &[Statement]) -> bool {
    use crate::ir::Visitor;
    struct C(bool);
    impl<'a> Visitor<'a> for C {
        fn visit_statement(&mut self, s: &'a Statement) {
            if matches!(s, Statement::Throw(_)) {
                self.0 = true;
            }
            if !self.0 {
                self.walk_statement(s);
            }
        }
    }
    let mut c = C(false);
    for s in stmts {
        c.visit_statement(s);
        if c.0 {
            return true;
        }
    }
    false
}

// `suspends` are the yield cases in source order; `terminal` is the done case
// (resume after the last yield). Thread `yield value` into each next binding.
fn emit_yield_chain(suspends: &[ParsedCase], terminal: &ParsedCase) -> Option<Vec<Statement>> {
    if suspends.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (i, case) in suspends.iter().enumerate() {
        if i > 0 {
            push_yield(&mut out, &suspends[i - 1].value, &case.resume_binding);
        }
        out.extend(case.pre.iter().cloned());
    }
    push_yield(
        &mut out,
        &suspends.last()?.value,
        &terminal.resume_binding,
    );
    out.extend(terminal.pre.iter().cloned());
    if !is_undefined(&terminal.value) {
        out.push(Statement::Return(Some(terminal.value.clone())));
    }
    if !out.iter().any(stmt_has_yield_deep) {
        return None;
    }
    Some(out)
}

fn push_yield(out: &mut Vec<Statement>, value: &Expression, binding: &Option<AssignTarget>) {
    let yielded = Expression::Yield {
        value: Box::new(value.clone()),
        delegate: false,
    };
    match binding {
        Some(target) => out.push(Statement::Assign {
            target: target.clone(),
            value: yielded,
        }),
        None => out.push(Statement::Expr(yielded)),
    }
}

// Collect every variable name that is compared against an integer literal; these
// are the state-machine status / label slots, never user data.
fn collect_state_vars(body: &[Statement]) -> std::collections::HashSet<String> {
    use crate::ir::Visitor;
    struct C(std::collections::HashSet<String>);
    impl<'b> Visitor<'b> for C {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = e {
                for (a, b) in [(left, right), (right, left)] {
                    if int_const(a).is_some() {
                        if let Expression::Value(Value::Variable(n)) = b.as_ref() {
                            self.0.insert(n.clone());
                        }
                    }
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(std::collections::HashSet::new());
    for s in body {
        c.visit_statement(s);
    }
    c.0
}

// Parse one case body into (resume binding, real pre-code, yielded value, done).
fn parse_case(case_body: &[Statement], state_vars: &std::collections::HashSet<String>) -> Option<ParsedCase> {
    let real = strip_arg_protocol(case_body);

    // A leading `x = <resume param>` binds the value the generator was resumed
    // with (the result of the previous yield / await).
    let mut resume_binding = None;
    let mut idx = 0;
    if let Some(Statement::Assign { target, value }) = real.first() {
        if is_resume_param(value) {
            resume_binding = Some(target.clone());
            idx = 1;
        }
    }

    let mut pre = Vec::new();
    let (value, done) = collect_pre_and_yield(&real[idx..], &mut pre, state_vars)?;
    Some(ParsedCase { resume_binding, pre, value, done })
}

// Walk a case body, appending real code to `pre` and returning the suspend
// point's (value, done). Conditional early exits (`if (guard) { return X }`) that
// guard the suspend point are flattened: the guard is kept in `pre` and the
// fall-through (its else branch) continues to the yield.
fn collect_pre_and_yield(
    stmts: &[Statement],
    pre: &mut Vec<Statement>,
    state_vars: &std::collections::HashSet<String>,
) -> Option<(Expression, bool)> {
    let mut i = 0;
    while i < stmts.len() {
        // The suspend point is the result-object return; everything up to it is
        // real code (minus state bookkeeping and dead inits).
        if let Some((value, done, consumed)) = parse_result_return(&stmts[i..]) {
            if i + consumed != stmts.len() {
                return None; // unexpected trailing code after the return
            }
            return Some((value, done));
        }
        // A trailing `if` where one branch is a terminal exit and the other
        // continues to the suspend point flattens to a guard `if (cond) { exit }`
        // plus the continuation branch. Either branch may be the terminal one.
        if i + 1 == stmts.len() {
            if let Statement::If { condition, then_body, else_body } = &stmts[i] {
                if !else_body.is_empty() {
                    if let Some(exit) = reconstruct_exit_body(then_body, state_vars) {
                        pre.push(Statement::If {
                            condition: condition.clone(),
                            then_body: exit,
                            else_body: Vec::new(),
                        });
                        return collect_pre_and_yield(else_body, pre, state_vars);
                    }
                    if let Some(exit) = reconstruct_exit_body(else_body, state_vars) {
                        pre.push(Statement::If {
                            condition: Expression::unary(crate::ir::UnaryOp::Not, condition.clone()),
                            then_body: exit,
                            else_body: Vec::new(),
                        });
                        return collect_pre_and_yield(then_body, pre, state_vars);
                    }
                }
            }
        }
        if !is_bookkeeping(&stmts[i], state_vars) {
            pre.push(stmts[i].clone());
        }
        i += 1;
    }
    None
}

// Reconstruct a terminal branch (`{ ...; return {value:V,done:true} }` or
// `{ ...; throw X }`) into plain `return V` / `throw X`, dropping bookkeeping.
// Returns None if the branch is not a clean terminal exit.
fn reconstruct_exit_body(
    body: &[Statement],
    state_vars: &std::collections::HashSet<String>,
) -> Option<Vec<Statement>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        if let Statement::Throw(_) = &body[i] {
            if i + 1 != body.len() {
                return None;
            }
            out.push(body[i].clone());
            return Some(out);
        }
        if let Some((value, done, consumed)) = parse_result_return(&body[i..]) {
            if !done || i + consumed != body.len() {
                return None; // not a terminal (done) exit
            }
            if !is_undefined(&value) {
                out.push(Statement::Return(Some(value)));
            } else {
                out.push(Statement::Return(None));
            }
            return Some(out);
        }
        if !is_bookkeeping(&body[i], state_vars) {
            out.push(body[i].clone());
        }
        i += 1;
    }
    None
}

// Drop state-slot writes, dead `x = undefined` inits and label copies.
fn is_bookkeeping(s: &Statement, state_vars: &std::collections::HashSet<String>) -> bool {
    if let Statement::Assign { target: AssignTarget::Variable(n), value } = s {
        if state_vars.contains(n) {
            return true;
        }
        if matches!(value, Expression::Value(Value::Constant(Constant::Undefined))) {
            return true;
        }
        if let Expression::Value(Value::Variable(src)) = value {
            if state_vars.contains(src) {
                return true;
            }
        }
    }
    false
}

// Recognise the result-object shapes the frontend emits:
//   return { value: V, done: D }
//   obj = { value: V, done: D }; return obj          (after slot-fill fold)
//   obj = { value: <ignored>, done: D }; obj[0] = V; return obj
// Returns (value, done, statements_consumed).
fn parse_result_return(stmts: &[Statement]) -> Option<(Expression, bool, usize)> {
    // Direct literal return.
    if let Statement::Return(Some(expr)) = &stmts[0] {
        if let Some((v, d)) = parse_result_object(expr) {
            return Some((v, d, 1));
        }
    }
    // Folded slot-fill: `obj = { value: V, done: D }; return obj`. Nested object
    // folding used to turn the 3-statement form below into this, which made the
    // whole generator pass bail and leave the raw v98 state machine in the dump.
    if stmts.len() >= 2 {
        if let Statement::Return(Some(Expression::Value(Value::Variable(o2)))) = &stmts[1] {
            if let Some((o1, obj_expr)) = assigned_object(&stmts[0]) {
                if o1 == o2 {
                    if let Some((v, d)) = parse_result_object(obj_expr) {
                        return Some((v, d, 2));
                    }
                }
            }
        }
    }
    // Incremental object build then return.
    if stmts.len() >= 3 {
        if let (
            Statement::Assign { target: AssignTarget::Variable(o1), value: Expression::Object { properties } },
            Statement::Assign { target: value_target, value: real_value },
            Statement::Return(Some(Expression::Value(Value::Variable(o3)))),
        ) = (&stmts[0], &stmts[1], &stmts[2])
        {
            let obj_is = |e: &Expression, name: &str| matches!(e, Expression::Value(Value::Variable(v)) if v == name);
            // The fill of the `value` slot reaches here either as the raw slot
            // index the bytecode emits or, once slot indexes have been renamed
            // against the object shape, as the named member. Both are the same
            // store into property 0.
            let value_fill = match value_target {
                AssignTarget::Index { object, key } => Some((
                    object,
                    matches!(key, Expression::Value(Value::Constant(Constant::Integer(0)))),
                )),
                AssignTarget::Member { object, property } => Some((object, property == "value")),
                _ => None,
            };
            let (object, fills_value) = value_fill?;
            if o1 == o3 && obj_is(object, o1) && fills_value {
                let done = properties.iter().find_map(|p| match &p.key {
                    PropertyKey::Ident(k) | PropertyKey::String(k) if k == "done" => Some(is_truthy(&p.value)),
                    _ => None,
                })?;
                return Some((real_value.clone(), done, 3));
            }
        }
    }
    None
}

fn is_resume_param(e: &Expression) -> bool {
    matches!(e, Expression::Value(Value::Parameter(1)))
}

fn is_undefined(e: &Expression) -> bool {
    matches!(e, Expression::Value(Value::Constant(Constant::Undefined)))
}

fn stmt_has_yield_deep(s: &Statement) -> bool {
    use crate::ir::Visitor;
    struct C(bool);
    impl<'b> Visitor<'b> for C {
        fn visit_expression(&mut self, e: &'b Expression) {
            if matches!(e, Expression::Yield { .. }) {
                self.0 = true;
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(false);
    c.visit_statement(s);
    c.0
}

// --- locating the label dispatch ---

// Find the TryCatch try-body (search through the status-guard if/else nest) then
// the label-dispatch `if` inside it.
fn find_label_dispatch(body: &[Statement]) -> Option<&Statement> {
    let try_body = find_generator_try(body)?;
    try_body.iter().find(|s| is_label_dispatch_if(s))
}

fn find_generator_try(body: &[Statement]) -> Option<&Vec<Statement>> {
    for s in body {
        match s {
            Statement::TryCatch { try_body, .. } => return Some(try_body),
            Statement::If { then_body, else_body, .. } => {
                if let Some(t) = find_generator_try(then_body) {
                    return Some(t);
                }
                if let Some(t) = find_generator_try(else_body) {
                    return Some(t);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_label_dispatch_if(s: &Statement) -> bool {
    matches!(s, Statement::If { condition, .. } if label_of_condition(condition).is_some())
}

// `<int> === <var>` or `<var> === <int>` → the integer label.
fn label_of_condition(cond: &Expression) -> Option<i32> {
    if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = cond {
        if let (Some(k), true) = (int_const(left), is_var(right)) {
            return Some(k);
        }
        if let (true, Some(k)) = (is_var(left), int_const(right)) {
            return Some(k);
        }
    }
    None
}

fn int_const(e: &Expression) -> Option<i32> {
    match e {
        Expression::Value(Value::Constant(Constant::Integer(n))) => Some(*n),
        _ => None,
    }
}

fn is_var(e: &Expression) -> bool {
    // The status/label live in the generator's own environment slots, which at
    // this stage are `ClosureVar`; a `tmp` copy of the label is a plain Variable.
    matches!(
        e,
        Expression::Value(Value::Variable(_)) | Expression::Value(Value::ClosureVar { .. })
    )
}

// Walk the `if (0===l) {..} else if (1===l) {..} else {done}` chain into
// (label, body) pairs, in source order. The trailing non-label `else` is the
// terminal/done case (given a sentinel label).
fn collect_label_cases(mut s: &Statement) -> Option<Vec<(i32, Vec<Statement>)>> {
    let mut cases = Vec::new();
    loop {
        let Statement::If { condition, then_body, else_body } = s else {
            break;
        };
        let Some(k) = label_of_condition(condition) else {
            break;
        };
        cases.push((k, then_body.clone()));
        if else_body.len() == 1 {
            // Either the next label `if`, or a single-statement done case.
            if is_label_dispatch_if(&else_body[0]) {
                s = &else_body[0];
                continue;
            }
            cases.push((i32::MAX, else_body.clone()));
            break;
        } else if !else_body.is_empty() {
            cases.push((i32::MAX, else_body.clone())); // multi-statement done case
            break;
        } else {
            break;
        }
    }
    if cases.is_empty() {
        None
    } else {
        Some(cases)
    }
}

// --- per-case extraction ---

// Navigate past the `if (arg0===1) {throw} else if (arg0===2) {return} else {..}`
// resume-protocol wrapper to the real (next) branch.
fn strip_arg_protocol(body: &[Statement]) -> &[Statement] {
    if body.len() == 1 {
        if let Statement::If { condition, else_body, .. } = &body[0] {
            if is_resume_protocol_cond(condition) {
                return strip_arg_protocol(else_body);
            }
        }
    }
    body
}

// `arg0 === 1` or `arg0 === 2` (resume method check; arg0 is Parameter(0)).
fn is_resume_protocol_cond(cond: &Expression) -> bool {
    if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = cond {
        let is_p0 = |e: &Expression| matches!(e, Expression::Value(Value::Parameter(0)));
        let is_12 = |e: &Expression| matches!(int_const(e), Some(1) | Some(2));
        return (is_p0(left) && is_12(right)) || (is_p0(right) && is_12(left));
    }
    false
}

// Extract (value, done) from an `{value: V, done: D}` object literal.
fn parse_result_object(expr: &Expression) -> Option<(Expression, bool)> {
    let Expression::Object { properties } = expr else {
        return None;
    };
    let mut value = None;
    let mut done = None;
    for p in properties {
        let key = match &p.key {
            PropertyKey::Ident(k) | PropertyKey::String(k) => k.as_str(),
            _ => continue,
        };
        match key {
            "value" => value = Some(p.value.clone()),
            "done" => done = Some(is_truthy(&p.value)),
            _ => {}
        }
    }
    Some((value?, done?))
}

fn is_truthy(e: &Expression) -> bool {
    match e {
        Expression::Value(Value::Constant(Constant::Bool(b))) => *b,
        Expression::Value(Value::Constant(Constant::Integer(n))) => *n != 0,
        _ => false,
    }
}

fn assigned_object(stmt: &Statement) -> Option<(&str, &Expression)> {
    match stmt {
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value,
        } => Some((name, value)),
        Statement::Let { name, value, .. } => Some((name, value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ObjectProperty;

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Variable(n.into()))
    }
    fn param(i: u32) -> Expression {
        Expression::Value(Value::Parameter(i))
    }
    fn int(n: i32) -> Expression {
        Expression::constant(Constant::Integer(n))
    }
    fn bool_c(b: bool) -> Expression {
        Expression::constant(Constant::Bool(b))
    }
    fn eq(l: Expression, r: Expression) -> Expression {
        Expression::binary(BinaryOp::StrictEq, l, r)
    }
    fn obj_value_done(value: Expression, done: bool) -> Expression {
        Expression::Object {
            properties: vec![
                ObjectProperty {
                    key: PropertyKey::Ident("value".into()),
                    value,
                },
                ObjectProperty {
                    key: PropertyKey::Ident("done".into()),
                    value: bool_c(done),
                },
            ],
        }
    }
    fn arg_protocol(real: Vec<Statement>) -> Vec<Statement> {
        vec![Statement::If {
            condition: eq(param(0), int(1)),
            then_body: vec![Statement::Throw(param(1))],
            else_body: vec![Statement::If {
                condition: eq(param(0), int(2)),
                then_body: vec![
                    Statement::Assign {
                        target: AssignTarget::Variable("obj".into()),
                        value: obj_value_done(param(1), true),
                    },
                    Statement::Return(Some(var("obj"))),
                ],
                else_body: real,
            }],
        }]
    }

    #[test]
    fn reconstructs_folded_two_statement_result_object() {
        // Shape after nested slot-fill: yield is a direct `{value,done}` return,
        // terminal is `obj = {value, done}; return obj` (the 2-stmt form).
        let yield_case = arg_protocol(vec![
            Statement::Expr(Expression::Call {
                callee: Box::new(var("dispatch")),
                arguments: vec![],
            }),
            Statement::Return(Some(obj_value_done(
                Expression::Call {
                    callee: Box::new(var("post")),
                    arguments: vec![],
                },
                false,
            ))),
        ]);
        let done_case = arg_protocol(vec![
            Statement::Assign {
                target: AssignTarget::Variable("obj".into()),
                value: obj_value_done(
                    Expression::Member {
                        object: Box::new(param(1)),
                        property: PropertyKey::Ident("token".into()),
                        optional: false,
                    },
                    true,
                ),
            },
            Statement::Return(Some(var("obj"))),
        ]);
        let body = vec![Statement::If {
            condition: eq(var("status"), int(2)),
            then_body: vec![Statement::Expr(Expression::Call {
                callee: Box::new(var("throwTypeError")),
                arguments: vec![],
            })],
            else_body: vec![Statement::TryCatch {
                try_body: vec![Statement::If {
                    condition: eq(int(0), var("label")),
                    then_body: yield_case,
                    else_body: done_case,
                }],
                catch_param: Some("e".into()),
                catch_body: vec![Statement::Throw(var("e"))],
                finally_body: vec![],
            }],
        }];

        let out = reconstruct_generator_v98(body);
        let has_yield = out.iter().any(stmt_has_yield_deep);
        assert!(has_yield, "expected flattened yield, got {out:?}");
        let dump = format!("{out:?}");
        assert!(!dump.contains("throwTypeError"), "raw executing-guard leaked: {dump}");
    }

    #[test]
    fn reconstructs_async_try_catch_skipping_error_label() {
        // forgotPassword shape: label 0 yields, label 1 is the catch handler
        // (throw / early return), trailing else is the resume after the yield.
        let yield_case = arg_protocol(vec![
            Statement::Expr(Expression::Call {
                callee: Box::new(var("dispatchRequest")),
                arguments: vec![],
            }),
            Statement::Return(Some(obj_value_done(var("posted"), false))),
        ]);
        let catch_case = vec![Statement::If {
            condition: var("isPhone"),
            then_body: vec![Statement::Return(Some(obj_value_done(
                Expression::constant(Constant::Bool(false)),
                true,
            )))],
            else_body: vec![Statement::Throw(var("err"))],
        }];
        let done_case = arg_protocol(vec![
            Statement::Assign {
                target: AssignTarget::Variable("body".into()),
                value: param(1),
            },
            Statement::Expr(Expression::Call {
                callee: Box::new(var("dispatchSent")),
                arguments: vec![],
            }),
            Statement::Return(Some(obj_value_done(
                Expression::Member {
                    object: Box::new(var("body")),
                    property: PropertyKey::Ident("method".into()),
                    optional: false,
                },
                true,
            ))),
        ]);
        let body = vec![Statement::If {
            condition: eq(var("status"), int(2)),
            then_body: vec![Statement::Expr(Expression::Call {
                callee: Box::new(var("throwTypeError")),
                arguments: vec![],
            })],
            else_body: vec![Statement::TryCatch {
                try_body: vec![Statement::If {
                    condition: eq(int(0), var("label")),
                    then_body: yield_case,
                    else_body: vec![Statement::If {
                        condition: eq(int(1), var("label")),
                        then_body: catch_case,
                        else_body: done_case,
                    }],
                }],
                catch_param: Some("e".into()),
                catch_body: vec![Statement::Throw(var("e"))],
                finally_body: vec![],
            }],
        }];

        let out = reconstruct_generator_v98(body);
        let dump = format!("{out:?}");
        assert!(out.iter().any(stmt_has_yield_deep), "expected yield, got {dump}");
        assert!(!dump.contains("throwTypeError"), "raw guard leaked: {dump}");
        assert!(dump.contains("dispatchRequest"), "lost request dispatch: {dump}");
        assert!(dump.contains("dispatchSent"), "lost sent dispatch: {dump}");
    }
}
