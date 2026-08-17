// Detect thin generator wrappers that only create/return an inner function*.
use crate::ir::Statement;

pub(super) fn generator_wrapper_target(body: &[Statement]) -> Option<u32> {
    use crate::ir::{AssignTarget, Expression, Value};

    // Skip comments and generator env-slot initializers that a v98 wrapper emits
    // before returning the inner generator (`let c0 = 0`, `closure_N = 0`, …).
    let is_zero = |e: &Expression| {
        matches!(
            e,
            Expression::Value(Value::Constant(
                crate::ir::Constant::Integer(0) | crate::ir::Constant::Undefined
            ))
        )
    };
    let is_env_slot_name = |n: &str| {
        n.starts_with("closure_")
            || (n.len() >= 2
                && n.starts_with('c')
                && n[1..].chars().all(|c| c.is_ascii_digit()))
    };
    let is_env_init = |s: &Statement| -> bool {
        match s {
            Statement::Let { name, value, .. } => {
                is_env_slot_name(name) && is_zero(value)
            }
            Statement::Assign {
                target: AssignTarget::ClosureVar { .. },
                value,
            } => is_zero(value),
            Statement::Assign {
                target: AssignTarget::Variable(n),
                value,
            } => is_env_slot_name(n) && is_zero(value),
            _ => false,
        }
    };
    let meaningful: Vec<&Statement> = body
        .iter()
        .filter(|s| !matches!(s, Statement::Comment(_)) && !is_env_init(s))
        .collect();

    // CreateGenerator is lowered either as `function*(){}` or as
    // `(function*(){})()`, both refer to the same inner function id.
    let inner_gen_id = |e: &Expression| -> Option<u32> {
        match e {
            Expression::Function {
                id,
                is_generator: true,
                ..
            } => Some(id.0),
            Expression::Call {
                callee,
                arguments,
            } if arguments.is_empty()
                || (arguments.len() == 1
                    && matches!(
                        &arguments[0],
                        Expression::Value(Value::Constant(crate::ir::Constant::Undefined))
                            | Expression::Value(Value::This)
                    )) =>
            {
                match callee.as_ref() {
                    Expression::Function {
                        id,
                        is_generator: true,
                        ..
                    } => Some(id.0),
                    _ => None,
                }
            }
            _ => None,
        }
    };

    match meaningful.as_slice() {
        // return function*() { ... }  OR  return (function*(){})()
        [Statement::Return(Some(e))] => inner_gen_id(e),
        // r = function*() { ... }; return r
        // r = (function*(){})(); return r
        [Statement::Assign {
            target: AssignTarget::Register(r),
            value,
        }, Statement::Return(Some(Expression::Value(Value::Register(rr))))]
            if r == rr =>
        {
            inner_gen_id(value)
        }
        // let/const x = function*(){}; return x  (after naming)
        [Statement::Let { name, value, .. }, Statement::Return(Some(Expression::Value(Value::Variable(v))))]
            if name == v =>
        {
            inner_gen_id(value)
        }
        [Statement::Assign {
            target: AssignTarget::Variable(name),
            value,
        }, Statement::Return(Some(Expression::Value(Value::Variable(v))))]
            if name == v =>
        {
            inner_gen_id(value)
        }
        _ => None,
    }
}
