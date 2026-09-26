// Drop the fast path hermesc builds around `f.call(...)` and `f.apply(...)`.
//
// HBC >= 97 compiles a `.call` / `.apply` site as a guard on the intrinsic:
//
//   if (f.call === HermesBuiltin.functionPrototypeCall) { r = f(this, ...) }
//   else { r = f.call(this, ...) }
//
// Both arms compute the same value by construction, the compiler only
// specialised the common case. The generic arm is the one that reads like the
// source, so the guard is replaced by it and the fast path goes away. A guard
// whose generic arm cannot be told apart from what follows it is left alone.

use crate::ir::MutVisitor;
use crate::ir::{map_nested_bodies, BinaryOp, Expression, PropertyKey, Statement, UnaryOp, Value};

pub fn fold_builtin_guards(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            Statement::If {
                condition,
                then_body,
                else_body,
            } => match guard_polarity(&condition) {
                Some(Polarity::FastIsThen) => {
                    // The generic arm is the else branch. With no else branch the
                    // generic code is what follows the if, so the whole if goes.
                    out.extend(fold_builtin_guards(else_body));
                }
                Some(Polarity::FastIsElse) if !else_body.is_empty() => {
                    out.extend(fold_builtin_guards(then_body));
                }
                _ => out.push(Statement::If {
                    condition,
                    then_body: fold_builtin_guards(then_body),
                    else_body: fold_builtin_guards(else_body),
                }),
            },
            other => out.push(map_nested_bodies(other, fold_builtin_guards)),
        }
    }
    let mut folder = ConditionalFolder;
    folder.visit_statement_list(&mut out);
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Polarity {
    // `x === Builtin`: the then branch is the specialised path.
    FastIsThen,
    // `x !== Builtin` or `!(x === Builtin)`: the then branch is the generic path.
    FastIsElse,
}

fn guard_polarity(condition: &Expression) -> Option<Polarity> {
    match condition {
        Expression::Unary {
            op: UnaryOp::Not,
            operand,
        } => guard_polarity(operand).map(|p| match p {
            Polarity::FastIsThen => Polarity::FastIsElse,
            Polarity::FastIsElse => Polarity::FastIsThen,
        }),
        Expression::Binary { op, left, right } => {
            if !is_builtin_ref(left) && !is_builtin_ref(right) {
                return None;
            }
            match op {
                BinaryOp::StrictEq | BinaryOp::Eq => Some(Polarity::FastIsThen),
                BinaryOp::StrictNeq | BinaryOp::Neq => Some(Polarity::FastIsElse),
                _ => None,
            }
        }
        _ => None,
    }
}

// `HermesBuiltin.<name>` as `handle_jmp_builtin_is` spells it, whether the
// object is still the global read or was resolved to a plain name since.
fn is_builtin_ref(expr: &Expression) -> bool {
    let Expression::Member {
        object,
        property: PropertyKey::Ident(_),
        ..
    } = expr
    else {
        return matches!(expr, Expression::Unknown { opcode, .. } if opcode.starts_with("builtin"));
    };
    match object.as_ref() {
        Expression::Member {
            object: global,
            property: PropertyKey::Ident(name),
            ..
        } => name == "HermesBuiltin" && matches!(global.as_ref(), Expression::Value(Value::Global)),
        Expression::Value(Value::Binding(crate::ir::Binding::Variable(name))) => {
            name == "HermesBuiltin"
        }
        _ => false,
    }
}

// The same guard once structure recovery turned the diamond into a ternary.
struct ConditionalFolder;

impl MutVisitor for ConditionalFolder {
    fn visit_expression(&mut self, expr: &mut Expression) {
        self.walk_expression(expr);
        let polarity = match expr {
            Expression::Conditional { condition, .. } => guard_polarity(condition),
            _ => None,
        };
        let Some(polarity) = polarity else {
            return;
        };
        let placeholder = Expression::Value(Value::Constant(crate::ir::Constant::Undefined));
        if let Expression::Conditional {
            then_expr,
            else_expr,
            ..
        } = std::mem::replace(expr, placeholder)
        {
            *expr = match polarity {
                Polarity::FastIsThen => *else_expr,
                Polarity::FastIsElse => *then_expr,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Binding, Constant};

    fn builtin() -> Expression {
        Expression::Member {
            object: Box::new(Expression::Member {
                object: Box::new(Expression::Value(Value::Global)),
                property: PropertyKey::Ident("HermesBuiltin".into()),
                optional: false,
            }),
            property: PropertyKey::Ident("functionPrototypeCall".into()),
            optional: false,
        }
    }

    fn reg(n: u32) -> Expression {
        Expression::Value(Value::Binding(Binding::Register(n)))
    }

    fn assign(n: u32, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Register(n)),
            value,
        }
    }

    #[test]
    fn keeps_the_generic_arm_of_an_if() {
        let guard = Expression::binary(BinaryOp::StrictEq, reg(4), builtin());
        let fast = assign(4, Expression::constant(Constant::Integer(1)));
        let generic = assign(4, Expression::constant(Constant::Integer(2)));
        let out = fold_builtin_guards(vec![Statement::If {
            condition: guard,
            then_body: vec![fast],
            else_body: vec![generic.clone()],
        }]);
        assert_eq!(out, vec![generic]);
    }

    #[test]
    fn a_negated_guard_keeps_the_then_arm() {
        let guard = Expression::binary(BinaryOp::StrictNeq, reg(4), builtin());
        let generic = assign(4, Expression::constant(Constant::Integer(2)));
        let fast = assign(4, Expression::constant(Constant::Integer(1)));
        let out = fold_builtin_guards(vec![Statement::If {
            condition: guard,
            then_body: vec![generic.clone()],
            else_body: vec![fast],
        }]);
        assert_eq!(out, vec![generic]);
    }

    #[test]
    fn a_guard_with_only_a_fast_arm_is_dropped() {
        let guard = Expression::binary(BinaryOp::StrictEq, reg(4), builtin());
        let after = Statement::Return(Some(reg(4)));
        let out = fold_builtin_guards(vec![
            Statement::If {
                condition: guard,
                then_body: vec![assign(4, Expression::constant(Constant::Integer(1)))],
                else_body: vec![],
            },
            after.clone(),
        ]);
        assert_eq!(out, vec![after]);
    }

    #[test]
    fn a_ternary_guard_keeps_the_generic_value() {
        let guard = Expression::binary(BinaryOp::StrictEq, reg(4), builtin());
        let out = fold_builtin_guards(vec![Statement::Return(Some(Expression::Conditional {
            condition: Box::new(guard),
            then_expr: Box::new(Expression::constant(Constant::Integer(1))),
            else_expr: Box::new(Expression::constant(Constant::Integer(2))),
        }))]);
        assert_eq!(
            out,
            vec![Statement::Return(Some(Expression::constant(
                Constant::Integer(2)
            )))]
        );
    }

    #[test]
    fn an_ordinary_comparison_is_untouched() {
        let cond = Expression::binary(BinaryOp::StrictEq, reg(4), reg(5));
        let stmt = Statement::If {
            condition: cond,
            then_body: vec![assign(4, Expression::constant(Constant::Integer(1)))],
            else_body: vec![assign(4, Expression::constant(Constant::Integer(2)))],
        };
        assert_eq!(fold_builtin_guards(vec![stmt.clone()]), vec![stmt]);
    }
}
