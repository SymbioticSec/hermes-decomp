use crate::ir::{AssignTarget, Binding, Expression, PropertyKey, Statement, Value};

mod apply;

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
    apply::reconstruct_apply(stmts);

    // Rest args: `r = HermesBuiltin.copyRestArgs(N)` is `arguments` from index N as
    // a real array. N == 0 -> `[...arguments]`; otherwise
    // `Array.prototype.slice.call(arguments, N)`. (A bare spread would be invalid
    // outside an array/call, e.g. `return ...arguments`.)
    for stmt in stmts.iter_mut() {
        if let Statement::Assign { value, .. } = stmt {
            if let Some(args) = is_builtin_call(value, "copyRestArgs") {
                let all_args = || Expression::Array {
                    elements: vec![Some(Expression::Spread(Box::new(Expression::Value(
                        Value::Binding(Binding::Variable("arguments".to_string())),
                    ))))],
                };
                let n_is_zero = matches!(
                    args.first(),
                    Some(Expression::Value(Value::Constant(
                        crate::ir::Constant::Integer(0)
                    ))) | None
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

        let mut elements: Vec<Option<Expression>> = existing_elements(&stmts[i]);
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

pub(super) fn resolve_array_elements(
    expr: &Expression,
    before: &[Statement],
) -> Option<Vec<Expression>> {
    if let Expression::Array { elements } = expr {
        return Some(elements.iter().flatten().cloned().collect());
    }
    if let Expression::Value(Value::Binding(Binding::Register(r))) = expr {
        for stmt in before.iter().rev() {
            if let Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(tr)),
                value,
            } = stmt
            {
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

// `reg = [..]` -> the register. An empty literal is the Hermes size hint. A
// literal that already has elements is the head of `[head, ...rest]`.
fn array_literal_reg(stmt: &Statement) -> Option<u32> {
    if let Statement::Assign {
        target: AssignTarget::Binding(Binding::Register(r)),
        value,
    } = stmt
    {
        if matches!(value, Expression::Array { .. }) {
            return Some(*r);
        }
    }
    None
}

fn existing_elements(stmt: &Statement) -> Vec<Option<Expression>> {
    if let Statement::Assign {
        value: Expression::Array { elements },
        ..
    } = stmt
    {
        // A size hint (`[,]` / holes only) is not a head. Real elements are.
        if elements.iter().any(|e| e.is_some()) {
            return elements.clone();
        }
    }
    Vec::new()
}

// `_ = HermesBuiltin.arraySpread(arr, src, _)` targeting the array -> Some(src).
fn arr_spread_into(stmt: &Statement, arrs: &std::collections::HashSet<u32>) -> Option<Expression> {
    let value = match stmt {
        Statement::Assign { value, .. } => value,
        Statement::Expr(value) => value,
        _ => return None,
    };
    let args = is_builtin_call(value, "arraySpread")?;
    // `[target, source, ...]` is the real call. CallBuiltin also keeps the
    // frame's `this` slot, so the same call can arrive as
    // `[this, target, source, ...]`. The target is whichever of the first two
    // arguments is the array being filled.
    let source_at = if reg_in(&args.first(), arrs) {
        1
    } else if args.len() >= 3 && reg_in(&args.get(1), arrs) {
        2
    } else {
        return None;
    };
    args.get(source_at).cloned()
}

fn reg_in(expr: &Option<&Expression>, arrs: &std::collections::HashSet<u32>) -> bool {
    match expr {
        Some(Expression::Value(Value::Binding(Binding::Register(r)))) => arrs.contains(r),
        _ => false,
    }
}

// `arr[..] = val` targeting the array -> Some(val).
fn put_into_array(stmt: &Statement, arrs: &std::collections::HashSet<u32>) -> Option<Expression> {
    if let Statement::Assign {
        target: AssignTarget::Index { object, .. },
        value,
    } = stmt
    {
        if let Expression::Value(Value::Binding(Binding::Register(r))) = object {
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
    if let Statement::Assign {
        target: AssignTarget::Binding(Binding::Register(dst)),
        value,
    } = stmt
    {
        if arrs.contains(dst) || value.has_side_effects() {
            return false;
        }
        // Don't step over a statement that reads the array (it might consume it
        // in a way we don't model).
        return !arrs
            .iter()
            .any(|&r| crate::ir::expr_uses_register(value, r));
    }
    false
}

// `dst = <array-alias>` (register copy) -> Some(dst).
fn alias_copy(stmt: &Statement, arrs: &std::collections::HashSet<u32>) -> Option<u32> {
    if let Statement::Assign {
        target: AssignTarget::Binding(Binding::Register(dst)),
        value: Expression::Value(Value::Binding(Binding::Register(src))),
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
pub(super) fn is_builtin_call(value: &Expression, name: &str) -> Option<Vec<Expression>> {
    if let Expression::Call { callee, arguments } = value {
        if let Expression::Member {
            object,
            property: PropertyKey::Ident(p),
            ..
        } = &**callee
        {
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
        Expression::Value(Value::Binding(Binding::Variable(n))) => is_name(n),
        // `globalThis.HermesBuiltin`
        Expression::Member {
            object,
            property: PropertyKey::Ident(p),
            ..
        } => is_name(p) && matches!(object.as_ref(), Expression::Value(Value::Global)),
        _ => false,
    }
}
