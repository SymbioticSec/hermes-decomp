use crate::ir::{AssignTarget, Constant, Expression, PropertyKey, Statement, Terminator, Value};
use std::collections::BTreeMap;

pub(super) fn substitute_terminator(term: &Terminator, copies: &BTreeMap<u32, Expression>) -> Terminator {
    match term {
        Terminator::Branch {
            condition,
            true_target,
            false_target,
        } => Terminator::Branch {
            condition: substitute_expr(condition, copies),
            true_target: *true_target,
            false_target: *false_target,
        },
        Terminator::Return(Some(e)) => Terminator::Return(Some(substitute_expr(e, copies))),
        Terminator::Throw(e) => Terminator::Throw(substitute_expr(e, copies)),
        Terminator::Switch {
            value,
            cases,
            default,
        } => Terminator::Switch {
            value: substitute_expr(value, copies),
            cases: cases
                .iter()
                .map(|(e, t)| (substitute_expr(e, copies), *t))
                .collect(),
            default: *default,
        },
        _ => term.clone(),
    }
}

pub(super) fn substitute_stmt(stmt: &Statement, copies: &BTreeMap<u32, Expression>) -> Statement {
    match stmt {
        Statement::Expr(e) => Statement::Expr(substitute_expr(e, copies)),
        Statement::Let { name, value, kind } => Statement::Let {
            name: name.clone(),
            value: substitute_expr(value, copies),
            kind: *kind,
        },
        Statement::Assign { target, value } => Statement::Assign {
            target: substitute_target(target, copies),
            value: substitute_expr(value, copies),
        },
        Statement::Return(Some(e)) => Statement::Return(Some(substitute_expr(e, copies))),
        Statement::Throw(e) => Statement::Throw(substitute_expr(e, copies)),
        _ => stmt.clone(),
    }
}

fn substitute_target(target: &AssignTarget, copies: &BTreeMap<u32, Expression>) -> AssignTarget {
    match target {
        AssignTarget::Index { object, key } => AssignTarget::Index {
            object: substitute_expr(object, copies),
            key: substitute_expr(key, copies),
        },
        AssignTarget::Member { object, property } => AssignTarget::Member {
            object: substitute_expr(object, copies),
            property: property.clone(),
        },
        _ => target.clone(),
    }
}

fn substitute_expr(expr: &Expression, copies: &BTreeMap<u32, Expression>) -> Expression {
    match expr {
        Expression::Value(Value::Register(r)) => {
            copies.get(r).cloned().unwrap_or_else(|| expr.clone())
        }
        Expression::Binary { op, left, right } => Expression::binary(
            *op,
            substitute_expr(left, copies),
            substitute_expr(right, copies),
        ),
        Expression::Unary { op, operand } => {
            Expression::unary(*op, substitute_expr(operand, copies))
        }
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(substitute_expr(callee, copies)),
            arguments: arguments
                .iter()
                .map(|a| substitute_expr(a, copies))
                .collect(),
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(substitute_expr(callee, copies)),
            arguments: arguments
                .iter()
                .map(|a| substitute_expr(a, copies))
                .collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => {
            let new_obj = substitute_expr(object, copies);
            let new_prop = substitute_property_key(property, copies);
            Expression::Member {
                object: Box::new(new_obj),
                property: new_prop,
                optional: *optional,
            }
        }
        Expression::Array { elements } => Expression::Array {
            elements: elements
                .iter()
                .map(|e| e.as_ref().map(|ex| substitute_expr(ex, copies)))
                .collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .iter()
                .map(|p| crate::ir::ObjectProperty {
                    key: substitute_property_key(&p.key, copies),
                    value: substitute_expr(&p.value, copies),
                })
                .collect(),
        },
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => Expression::Conditional {
            condition: Box::new(substitute_expr(condition, copies)),
            then_expr: Box::new(substitute_expr(then_expr, copies)),
            else_expr: Box::new(substitute_expr(else_expr, copies)),
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(substitute_expr(target, copies)),
            value: Box::new(substitute_expr(value, copies)),
        },
        Expression::Spread(inner) => Expression::Spread(Box::new(substitute_expr(inner, copies))),
        _ => expr.clone(),
    }
}

fn substitute_property_key(key: &PropertyKey, copies: &BTreeMap<u32, Expression>) -> PropertyKey {
    match key {
        PropertyKey::Computed(expr) => {
            let subst = substitute_expr(expr, copies);
            // If the substituted expression is a constant integer, convert to Index
            match &subst {
                Expression::Value(Value::Constant(Constant::Integer(n))) => {
                    PropertyKey::Index(*n as i64)
                }
                Expression::Value(Value::Constant(Constant::Number(n))) if n.fract() == 0.0 => {
                    PropertyKey::Index(*n as i64)
                }
                _ => PropertyKey::Computed(Box::new(subst)),
            }
        }
        _ => key.clone(),
    }
}
