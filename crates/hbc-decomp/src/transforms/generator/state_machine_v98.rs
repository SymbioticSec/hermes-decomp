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

fn try_reconstruct(body: &[Statement]) -> Option<Vec<Statement>> {
    let dispatch = find_label_dispatch(body)?;
    let cases = collect_label_cases(dispatch)?;
    // A real state machine has at least one yield label plus the terminal case.
    if cases.len() < 2 {
        return None;
    }
    let mut out = Vec::new();
    for (_label, case_body) in &cases {
        emit_case(case_body, &mut out)?;
    }
    // Sanity: a reconstructed generator must contain at least one yield.
    if !out.iter().any(stmt_has_yield) {
        return None;
    }
    Some(out)
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

fn emit_case(case_body: &[Statement], out: &mut Vec<Statement>) -> Option<()> {
    let real = strip_arg_protocol(case_body);
    let mut pre: Vec<Statement> = Vec::new();
    for s in real {
        match s {
            // Drop the state-machine bookkeeping assignments (status / label).
            // The env slots are ClosureVar; an intermediate label copy is a tmp.
            Statement::Assign { target: AssignTarget::ClosureVar { .. }, .. } => {}
            Statement::Assign { target: AssignTarget::Variable(n), .. } if is_state_var(n) => {}
            Statement::Return(Some(expr)) => {
                let (val, done) = parse_result_object(expr)?;
                // A resume value flowing into user code (`x = yield v`) shows up
                // as a reference to the synthetic resume params; we only handle the
                // value-less form here — bail otherwise so we never emit `arg1`.
                if pre.iter().any(stmt_uses_resume_param) || expr_uses_resume_param(&val) {
                    return None;
                }
                out.append(&mut pre);
                if !done {
                    out.push(Statement::Expr(Expression::Yield {
                        value: Box::new(val),
                        delegate: false,
                    }));
                }
                return Some(());
            }
            // Any other statement is real generator code before the yield.
            other => pre.push(other.clone()),
        }
    }
    // No `{value,done}` return found in this case — not the shape we handle.
    None
}

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

fn is_state_var(name: &str) -> bool {
    // Generator env slots render as closure_N; intermediate copies as tmpN.
    name.starts_with("closure_") || name.starts_with("tmp")
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

// --- resume-param detection (to bail on `x = yield v` generators) ---

fn stmt_uses_resume_param(s: &Statement) -> bool {
    match s {
        Statement::Assign { value, .. } => expr_uses_resume_param(value),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            expr_uses_resume_param(e)
        }
        _ => false,
    }
}

fn expr_uses_resume_param(e: &Expression) -> bool {
    use crate::ir::Visitor;
    struct C(bool);
    impl<'b> Visitor<'b> for C {
        fn visit_expression(&mut self, e: &'b Expression) {
            if matches!(e, Expression::Value(Value::Parameter(0)) | Expression::Value(Value::Parameter(1))) {
                self.0 = true;
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(false);
    c.visit_expression(e);
    c.0
}

fn stmt_has_yield(s: &Statement) -> bool {
    matches!(s, Statement::Expr(Expression::Yield { .. }))
}
