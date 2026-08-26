use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};

// Reconstruct spread syntax from the Hermes spread/apply protocol.
//
// Array spread `[...a, 4, 5]` compiles to:
//   t = []                               (NewArray, size hint)
//   _ = HermesBuiltin.arraySpread(t, a, 0)
//   t[idx]   = 4                         (PutOwnByVal, idx = arraySpread's return)
//   t[idx+1] = 5
//   -> t = [...a, 4, 5]
//
// Spread call `f(...a)` compiles to a spread-built argument array passed to
//   HermesBuiltin.apply(f, args, thisArg)
//   -> f(...a)            (thisArg undefined)
//   -> f.apply(thisArg, args)  (otherwise)
pub fn transform_spread_rest(stmts: &mut Vec<Statement>) {
    fold_array_spreads(stmts);
    reconstruct_apply(stmts);

    // Rest args: `r = HermesBuiltin.copyRestArgs(N)` is `arguments` from index N as
    // a real array. N == 0 -> `[...arguments]`; otherwise
    // `Array.prototype.slice.call(arguments, N)`. (A bare spread would be invalid
    // outside an array/call, e.g. `return ...arguments`.)
    for stmt in stmts.iter_mut() {
        if let Statement::Assign { value, .. } = stmt {
            if let Some(args) = is_builtin_call(value, "copyRestArgs") {
                let all_args = || Expression::Array {
                    elements: vec![Some(Expression::Spread(Box::new(Expression::Value(
                        Value::Variable("arguments".to_string()),
                    ))))],
                };
                let n_is_zero = matches!(
                    args.first(),
                    Some(Expression::Value(Value::Constant(crate::ir::Constant::Integer(0))))
                        | None
                );
                *value = if n_is_zero {
                    all_args() // [...arguments]
                } else {
                    // [...arguments].slice(N)
                    Expression::Call {
                        callee: Box::new(Expression::member(all_args(), "slice")),
                        arguments: vec![args[0].clone()],
                    }
                };
            }
        }
    }
}

// Fold `t = []; arraySpread(t, src, _); t[..]=v; ...` into `t = [...src, v, ...]`.
fn fold_array_spreads(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i < stmts.len() {
        let arr_reg = match array_literal_reg(&stmts[i]) {
            Some(r) => r,
            None => {
                i += 1;
                continue;
            }
        };

        // The array register may be aliased by call-frame setup `Mov`s
        // (`r9 = r6`) before the spread/put statements, interleaved with unrelated
        // pure setup (`r_src = items`, `r_zero = 0`). Track aliases, collect the
        // spread/put statements to remove, and step over pure unrelated ones.
        let mut aliases: std::collections::HashSet<u32> = std::collections::HashSet::new();
        aliases.insert(arr_reg);

        let mut elements: Vec<Option<Expression>> = Vec::new();
        let mut remove: Vec<usize> = Vec::new();
        let mut saw_spread = false;
        let mut j = i + 1;
        while j < stmts.len() {
            if let Some(src) = arr_spread_into(&stmts[j], &aliases) {
                elements.push(Some(Expression::Spread(Box::new(src))));
                saw_spread = true;
                remove.push(j);
            } else if let Some(val) = put_into_array(&stmts[j], &aliases) {
                elements.push(Some(val));
                remove.push(j);
            } else if let Some(dst) = alias_copy(&stmts[j], &aliases) {
                aliases.insert(dst);
                remove.push(j);
            } else if is_skippable_setup(&stmts[j], &aliases) {
                // Unrelated pure call-frame setup, leave it (becomes dead).
            } else {
                break;
            }
            j += 1;
        }

        if saw_spread {
            if let Statement::Assign { target, .. } = &stmts[i] {
                let target = target.clone();
                stmts[i] = Statement::Assign {
                    target,
                    value: Expression::Array { elements },
                };
            }
            for &idx in remove.iter().rev() {
                stmts.remove(idx);
            }
        }
        i += 1;
    }
}

// `HermesBuiltin.apply(f, args, thisArg)` -> `f(...)` / `f.apply(thisArg, args)`,
// anywhere in each statement's expressions (the apply is often nested as a call
// argument, e.g. `print(apply(f, args, undefined))`).
fn reconstruct_apply(stmts: &mut [Statement]) {
    for idx in 0..stmts.len() {
        // Resolve the args array against the statements BEFORE this one.
        let (before, rest) = stmts.split_at_mut(idx);
        let stmt = &mut rest[0];
        rewrite_applies_in_stmt(stmt, before);
    }
}

fn rewrite_applies_in_stmt(stmt: &mut Statement, before: &[Statement]) {
    match stmt {
        Statement::Assign { value, .. }
        | Statement::Expr(value)
        | Statement::Return(Some(value))
        | Statement::Throw(value) => rewrite_applies_in_expr(value, before),
        _ => {}
    }
}

fn rewrite_applies_in_expr(expr: &mut Expression, before: &[Statement]) {
    // Bottom-up: rewrite children first.
    match expr {
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            rewrite_applies_in_expr(callee, before);
            for a in arguments.iter_mut() {
                rewrite_applies_in_expr(a, before);
            }
        }
        Expression::Member { object, .. } => rewrite_applies_in_expr(object, before),
        Expression::Binary { left, right, .. } => {
            rewrite_applies_in_expr(left, before);
            rewrite_applies_in_expr(right, before);
        }
        Expression::Unary { operand, .. } => rewrite_applies_in_expr(operand, before),
        Expression::Spread(inner) => rewrite_applies_in_expr(inner, before),
        _ => {}
    }
    if let Some(call) = apply_to_call(expr, before) {
        *expr = call;
    }
}

// If `value` is `HermesBuiltin.apply(f, args, thisArg)`, build the equivalent call.
fn apply_to_call(value: &Expression, before: &[Statement]) -> Option<Expression> {
    let args = match is_builtin_call(value, "apply") {
        Some(a) if a.len() >= 2 => a,
        _ => return None,
    };
    let func = args[0].clone();
    let args_array = &args[1];
    let this_arg = args.get(2);

    // Resolve the args array to a literal: either inline, or a register defined
    // earlier as an array literal.
    let elements = resolve_array_elements(args_array, before)?;

    let this_is_undefined = matches!(
        this_arg,
        None | Some(Expression::Value(Value::Constant(crate::ir::Constant::Undefined)))
    );

    if this_is_undefined {
        // f(...elements)
        Some(Expression::Call {
            callee: Box::new(func),
            arguments: elements,
        })
    } else {
        // f.apply(thisArg, argsArray)
        Some(Expression::Call {
            callee: Box::new(Expression::member(func, "apply")),
            arguments: vec![this_arg.cloned().unwrap(), args_array.clone()],
        })
    }
}

// Resolve an expression to array elements: an inline Array literal, or a
// register whose nearest preceding definition is an array literal.
fn resolve_array_elements(expr: &Expression, before: &[Statement]) -> Option<Vec<Expression>> {
    if let Expression::Array { elements } = expr {
        return Some(elements.iter().flatten().cloned().collect());
    }
    if let Expression::Value(Value::Register(r)) = expr {
        for stmt in before.iter().rev() {
            if let Statement::Assign { target: AssignTarget::Register(tr), value } = stmt {
                if tr == r {
                    if let Expression::Array { elements } = value {
                        return Some(elements.iter().flatten().cloned().collect());
                    }
                    return None;
                }
            }
        }
    }
    None
}

// `reg = [..]` / `reg = NewArray` -> the register, if the literal is empty or all
// holes (a size hint to be filled by the following spreads/puts).
fn array_literal_reg(stmt: &Statement) -> Option<u32> {
    if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
        if let Expression::Array { elements } = value {
            if elements.iter().all(|e| e.is_none()) {
                return Some(*r);
            }
        }
    }
    None
}

// `_ = HermesBuiltin.arraySpread(arr, src, _)` targeting the array -> Some(src).
fn arr_spread_into(stmt: &Statement, arrs: &std::collections::HashSet<u32>) -> Option<Expression> {
    let value = match stmt {
        Statement::Assign { value, .. } => value,
        Statement::Expr(value) => value,
        _ => return None,
    };
    let args = is_builtin_call(value, "arraySpread")?;
    if args.len() >= 2 {
        if let Expression::Value(Value::Register(r)) = &args[0] {
            if arrs.contains(r) {
                return Some(args[1].clone());
            }
        }
    }
    None
}

// `arr[..] = val` targeting the array -> Some(val).
fn put_into_array(stmt: &Statement, arrs: &std::collections::HashSet<u32>) -> Option<Expression> {
    if let Statement::Assign { target: AssignTarget::Index { object, .. }, value } = stmt {
        if let Expression::Value(Value::Register(r)) = object {
            if arrs.contains(r) {
                return Some(value.clone());
            }
        }
    }
    None
}

// A pure register assignment that does not reference the array, safe to step
// over while scanning for the array's spread/put statements (e.g. the source
// register and zero index loaded into the arraySpread call frame).
fn is_skippable_setup(stmt: &Statement, arrs: &std::collections::HashSet<u32>) -> bool {
    if let Statement::Assign { target: AssignTarget::Register(dst), value } = stmt {
        if arrs.contains(dst) || value.has_side_effects() {
            return false;
        }
        // Don't step over a statement that reads the array (it might consume it
        // in a way we don't model).
        return !arrs.iter().any(|&r| crate::ir::expr_uses_register(value, r));
    }
    false
}

// `dst = <array-alias>` (register copy) -> Some(dst).
fn alias_copy(stmt: &Statement, arrs: &std::collections::HashSet<u32>) -> Option<u32> {
    if let Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(Value::Register(src)),
    } = stmt
    {
        if arrs.contains(src) {
            return Some(*dst);
        }
    }
    None
}

// If `value` is a call to `HermesBuiltin.<name>(...)`, return its arguments. The
// callee object is lowered as `globalThis.HermesBuiltin` (Member on Global) or a
// bare `HermesBuiltin` variable.
fn is_builtin_call(value: &Expression, name: &str) -> Option<Vec<Expression>> {
    if let Expression::Call { callee, arguments } = value {
        if let Expression::Member { object, property: PropertyKey::Ident(p), .. } = &**callee {
            if p == name && is_hermes_builtin_obj(object) {
                return Some(arguments.clone());
            }
        }
    }
    None
}

fn is_hermes_builtin_obj(expr: &Expression) -> bool {
    // Modern bytecode names the internal-builtin object `HermesBuiltin`; legacy
    // (HBC < 74) names it `HermesInternal`.
    fn is_name(n: &str) -> bool {
        n == "HermesBuiltin" || n == "HermesInternal"
    }
    match expr {
        // bare `HermesBuiltin` / `HermesInternal`
        Expression::Value(Value::Variable(n)) => is_name(n),
        // `globalThis.HermesBuiltin`
        Expression::Member { object, property: PropertyKey::Ident(p), .. } => {
            is_name(p) && matches!(object.as_ref(), Expression::Value(Value::Global))
        }
        _ => false,
    }
}
