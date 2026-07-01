use crate::analysis::rename_registers;
use crate::ir::{AssignTarget, BinaryOp, Constant, Expression, PropertyKey, Statement, UnaryOp, Value};
use std::collections::BTreeMap;

// Detect for-in loop patterns and rebuild them as `for (key in object)`.
//
// Hermes lowers `for (k in o)` to the property-enumeration opcodes
// (GetPNameList / GetNextPName). After IR build + structure recovery the shape is:
//
//   keys = Object.keys(o)                 // GetPNameList (our lowering)
//   if (keys === undefined) {             // JmpUndefined: no enumerable props
//   } else {
//     cur = keys[idx]                     // first GetNextPName (peeled to header)
//     while (!(cur === undefined)) {      // JmpUndefined: enumeration exhausted
//       <body using cur>                  // the back-edge re-runs GetNextPName
//     }
//   }
//
// The internal index/advance of GetNextPName has no source-level form, so we
// match this whole shape and emit `for (cur in o) { <body> }`, dropping the
// enumeration plumbing (the per-iteration fetch is implied by for-in semantics).
pub fn detect_for_in_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = recurse(stmts);
    let mut result: Vec<Statement> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        if i + 1 < stmts.len() {
            if let Some(for_in) = try_match_for_in(&stmts[i], &stmts[i + 1]) {
                result.push(for_in);
                i += 2;
                continue;
            }
        }
        result.push(stmts[i].clone());
        i += 1;
    }
    result
}

// Recurse into nested blocks first so inner for-in loops are rebuilt too.
fn recurse(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::While { condition, body } => Statement::While {
                condition,
                body: detect_for_in_loops(body),
            },
            Statement::DoWhile { body, condition } => Statement::DoWhile {
                body: detect_for_in_loops(body),
                condition,
            },
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: detect_for_in_loops(then_body),
                else_body: detect_for_in_loops(else_body),
            },
            Statement::For { init, condition, update, body } => Statement::For {
                init,
                condition,
                update,
                body: detect_for_in_loops(body),
            },
            Statement::ForIn { variable, object, body } => Statement::ForIn {
                variable,
                object,
                body: detect_for_in_loops(body),
            },
            Statement::ForOf { variable, iterable, body } => Statement::ForOf {
                variable,
                iterable,
                body: detect_for_in_loops(body),
            },
            Statement::Block(inner) => Statement::Block(detect_for_in_loops(inner)),
            Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => {
                Statement::TryCatch {
                    try_body: detect_for_in_loops(try_body),
                    catch_param,
                    catch_body: detect_for_in_loops(catch_body),
                    finally_body: detect_for_in_loops(finally_body),
                }
            }
            other => other,
        })
        .collect()
}

// Match `keys = Object.keys(obj)` followed by the enumeration `if`.
fn try_match_for_in(keys_stmt: &Statement, if_stmt: &Statement) -> Option<Statement> {
    // [0] keys_reg = Object.keys(obj)
    let (keys_reg, obj_expr) = match keys_stmt {
        Statement::Assign { target: AssignTarget::Register(r), value } => {
            (*r, is_object_keys_call(value)?)
        }
        _ => return None,
    };

    // [1] if (keys_reg === undefined) {} else { <loop> }
    let else_body = match if_stmt {
        Statement::If { condition, then_body, else_body }
            if then_body.is_empty() && is_undefined_check_eq(condition, keys_reg) =>
        {
            else_body
        }
        _ => return None,
    };

    // else_body: cur_reg = keys_reg[idx]; [label]; while (!(cur_reg === undefined)) { body }
    let mut idx = 0;
    let cur_reg = loop {
        match else_body.get(idx)? {
            Statement::Assign { target: AssignTarget::Register(r), value }
                if is_index_into(value, keys_reg) =>
            {
                let r = *r;
                idx += 1;
                break r;
            }
            Statement::Comment(_) => idx += 1,
            _ => return None,
        }
    };

    // Skip a label comment between the peeled fetch and the while.
    while matches!(else_body.get(idx), Some(Statement::Comment(_))) {
        idx += 1;
    }

    let body = match else_body.get(idx)? {
        Statement::While { condition, body } if is_undefined_check_neq(condition, cur_reg) => body,
        _ => return None,
    };
    // The loop must be the last meaningful statement of the else branch.
    if else_body[idx + 1..]
        .iter()
        .any(|s| !matches!(s, Statement::Comment(_)))
    {
        return None;
    }

    // Strip the enumeration back-edge fetch (`cur = keys[idx]`) and the trailing
    // `// continue`, then bind the loop variable.
    let var_name = format!("key{cur_reg}");
    let cleaned: Vec<Statement> = body
        .iter()
        .filter(|s| !matches!(s, Statement::Comment(c) if c == "continue"))
        .filter(|s| !is_assign_of_index(s, cur_reg, keys_reg))
        .cloned()
        .collect();
    let mut map = BTreeMap::new();
    map.insert(cur_reg, var_name.clone());
    let cleaned = rename_registers(cleaned, &map);

    Some(Statement::ForIn {
        variable: var_name,
        object: obj_expr,
        body: detect_for_in_loops(cleaned),
    })
}

// `Object.keys(obj)` -> Some(obj)
fn is_object_keys_call(expr: &Expression) -> Option<Expression> {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.len() == 1 {
            if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } =
                callee.as_ref()
            {
                if prop == "keys" {
                    if let Expression::Value(Value::Variable(name)) = object.as_ref() {
                        if name == "Object" {
                            return Some(arguments[0].clone());
                        }
                    }
                }
            }
        }
    }
    None
}

// `reg[<anything>]` — the GetNextPName lowering (property at the internal index).
fn is_index_into(expr: &Expression, base_reg: u32) -> bool {
    if let Expression::Member { object, property: PropertyKey::Computed(_), .. } = expr {
        if let Expression::Value(Value::Register(r)) = object.as_ref() {
            return *r == base_reg;
        }
    }
    false
}

fn is_assign_of_index(stmt: &Statement, dst_reg: u32, base_reg: u32) -> bool {
    if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
        return *r == dst_reg && is_index_into(value, base_reg);
    }
    false
}

// `reg === undefined`
fn is_undefined_check_eq(expr: &Expression, reg: u32) -> bool {
    if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = expr {
        return touches_undefined(left, right, reg);
    }
    false
}

// `reg !== undefined` or `!(reg === undefined)`
fn is_undefined_check_neq(expr: &Expression, reg: u32) -> bool {
    match expr {
        Expression::Binary { op: BinaryOp::StrictNeq, left, right } => {
            touches_undefined(left, right, reg)
        }
        Expression::Unary { op: UnaryOp::Not, operand } => is_undefined_check_eq(operand, reg),
        _ => false,
    }
}

fn touches_undefined(left: &Expression, right: &Expression, reg: u32) -> bool {
    let is_reg = |e: &Expression| matches!(e, Expression::Value(Value::Register(r)) if *r == reg);
    let is_undef =
        |e: &Expression| matches!(e, Expression::Value(Value::Constant(Constant::Undefined)));
    (is_reg(left) && is_undef(right)) || (is_reg(right) && is_undef(left))
}
