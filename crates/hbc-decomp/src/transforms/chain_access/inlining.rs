use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;

pub fn inline_chains_in_stmt(stmt: Statement, to_inline: &BTreeMap<u32, Expression>) -> Statement {
    match stmt {
        Statement::Assign { target, value } => Statement::Assign {
            target: inline_in_target(target, to_inline),
            value: inline_in_expr(value, to_inline),
        },
        Statement::Let { name, value, kind } => Statement::Let {
            name,
            value: inline_in_expr(value, to_inline),
            kind,
        },
        Statement::Return(Some(e)) => Statement::Return(Some(inline_in_expr(e, to_inline))),
        Statement::Return(None) => Statement::Return(None),
        Statement::Throw(e) => Statement::Throw(inline_in_expr(e, to_inline)),
        Statement::Expr(e) => Statement::Expr(inline_in_expr(e, to_inline)),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition: inline_in_expr(condition, to_inline),
            then_body: then_body
                .into_iter()
                .map(|s| inline_chains_in_stmt(s, to_inline))
                .collect(),
            else_body: else_body
                .into_iter()
                .map(|s| inline_chains_in_stmt(s, to_inline))
                .collect(),
        },
        other => other,
    }
}

fn inline_in_target(target: AssignTarget, to_inline: &BTreeMap<u32, Expression>) -> AssignTarget {
    match target {
        AssignTarget::Member { object, property } => AssignTarget::Member {
            object: inline_in_expr(object, to_inline),
            property,
        },
        AssignTarget::Index { object, key } => AssignTarget::Index {
            object: inline_in_expr(object, to_inline),
            key: inline_in_expr(key, to_inline),
        },
        other => other,
    }
}

fn inline_in_expr(expr: Expression, to_inline: &BTreeMap<u32, Expression>) -> Expression {
    match expr {
        Expression::Value(Value::Register(r)) => {
            if let Some(replacement) = to_inline.get(&r) {
                // Recursively inline in case of nested chains
                inline_in_expr(replacement.clone(), to_inline)
            } else {
                Expression::Value(Value::Register(r))
            }
        }
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(inline_in_expr(*left, to_inline)),
            right: Box::new(inline_in_expr(*right, to_inline)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(inline_in_expr(*operand, to_inline)),
        },
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(inline_in_expr(*callee, to_inline)),
            arguments: arguments
                .into_iter()
                .map(|a| inline_in_expr(a, to_inline))
                .collect(),
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(inline_in_expr(*callee, to_inline)),
            arguments: arguments
                .into_iter()
                .map(|a| inline_in_expr(a, to_inline))
                .collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => {
            let new_prop = match property {
                PropertyKey::Computed(e) => {
                    PropertyKey::Computed(Box::new(inline_in_expr(*e, to_inline)))
                }
                other => other,
            };
            Expression::Member {
                object: Box::new(inline_in_expr(*object, to_inline)),
                property: new_prop,
                optional,
            }
        }
        Expression::Array { elements } => Expression::Array {
            elements: elements
                .into_iter()
                .map(|e| e.map(|ex| inline_in_expr(ex, to_inline)))
                .collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .into_iter()
                .map(|p| crate::ir::ObjectProperty {
                    key: match p.key {
                        PropertyKey::Computed(e) => {
                            PropertyKey::Computed(Box::new(inline_in_expr(*e, to_inline)))
                        }
                        other => other,
                    },
                    value: inline_in_expr(p.value, to_inline),
                })
                .collect(),
        },
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => Expression::Conditional {
            condition: Box::new(inline_in_expr(*condition, to_inline)),
            then_expr: Box::new(inline_in_expr(*then_expr, to_inline)),
            else_expr: Box::new(inline_in_expr(*else_expr, to_inline)),
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(inline_in_expr(*target, to_inline)),
            value: Box::new(inline_in_expr(*value, to_inline)),
        },
        Expression::Spread(inner) => {
            Expression::Spread(Box::new(inline_in_expr(*inner, to_inline)))
        }
        Expression::Await(inner) => Expression::Await(Box::new(inline_in_expr(*inner, to_inline))),
        Expression::Yield { value, delegate } => Expression::Yield {
            value: Box::new(inline_in_expr(*value, to_inline)),
            delegate,
        },
        other => other,
    }
}
