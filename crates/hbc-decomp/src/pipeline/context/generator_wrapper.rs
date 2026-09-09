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
    let is_param_value = |e: &Expression| match e {
        Expression::Value(Value::Parameter(_))
        | Expression::Value(Value::This)
        | Expression::Value(Value::Arguments) => true,
        Expression::Value(Value::Variable(v)) => {
            v == "self"
                || v == "arguments"
                || (v.starts_with("arg")
                    && v.as_bytes().get(3).is_some_and(|b| b.is_ascii_digit())
                    && v[3..].chars().all(|c| c.is_ascii_digit()))
        }
        _ => false,
    };
    let is_env_init = |s: &Statement| -> bool {
        match s {
            Statement::Let { name, value, .. } => {
                is_env_slot_name(name) && (is_zero(value) || is_param_value(value))
            }
            Statement::Assign {
                target: AssignTarget::ClosureVar { .. },
                value,
            } => is_zero(value) || is_param_value(value),
            Statement::Assign {
                target: AssignTarget::Variable(n),
                value,
            } => is_env_slot_name(n) && (is_zero(value) || is_param_value(value)),
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
        // CreateGenerator + kick: `const g = (function*(){})(); g.next(); return g`
        [Statement::Let { name, value, .. }, start, Statement::Return(Some(Expression::Value(Value::Variable(v))))]
            if name == v && is_iterator_next(start, name) =>
        {
            inner_gen_id(value)
        }
        [Statement::Assign {
            target: AssignTarget::Variable(name),
            value,
        }, start, Statement::Return(Some(Expression::Value(Value::Variable(v))))]
            if name == v && is_iterator_next(start, name) =>
        {
            inner_gen_id(value)
        }
        _ => None,
    }
}

fn is_iterator_next(stmt: &Statement, name: &str) -> bool {
    use crate::ir::{Expression, PropertyKey, Value};
    let Expression::Call { callee, arguments } = (match stmt {
        Statement::Expr(e) => e,
        _ => return false,
    }) else {
        return false;
    };
    let empty_or_undef = arguments.is_empty()
        || (arguments.len() == 1
            && matches!(
                &arguments[0],
                Expression::Value(Value::Constant(crate::ir::Constant::Undefined))
                    | Expression::Value(Value::Constant(crate::ir::Constant::Integer(0)))
            ));
    if !empty_or_undef {
        return false;
    }
    matches!(
        callee.as_ref(),
        Expression::Member {
            object,
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } if p == "next"
            && matches!(object.as_ref(), Expression::Value(Value::Variable(v)) if v == name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Expression, FunctionId, Statement, Value, VarKind};

    fn gen_call() -> Expression {
        Expression::Call {
            callee: Box::new(Expression::Function {
                id: FunctionId(9),
                name: None,
                is_arrow: false,
                is_async: false,
                is_generator: true,
            }),
            arguments: vec![],
        }
    }

    fn next_stmt(name: &str) -> Statement {
        Statement::Expr(Expression::Call {
            callee: Box::new(Expression::Member {
                object: Box::new(Expression::Value(Value::Variable(name.into()))),
                property: crate::ir::PropertyKey::Ident("next".into()),
                optional: false,
            }),
            arguments: vec![],
        })
    }

    #[test]
    fn collapses_create_generator_plus_next() {
        let body = vec![
            Statement::Assign {
                target: AssignTarget::Variable("c7".into()),
                value: Expression::Value(Value::Constant(crate::ir::Constant::Integer(0))),
            },
            Statement::Assign {
                target: AssignTarget::Variable("closure_0".into()),
                value: Expression::Value(Value::Parameter(0)),
            },
            Statement::Let {
                name: "iter".into(),
                value: gen_call(),
                kind: VarKind::Const,
            },
            next_stmt("iter"),
            Statement::Return(Some(Expression::Value(Value::Variable("iter".into())))),
        ];
        assert_eq!(generator_wrapper_target(&body), Some(9));
    }
}
