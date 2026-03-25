pub mod context;
pub mod info;
use crate::ir::{AssignTarget, Expression, Statement, Value};

pub use context::ClosureContext;
use info::encode_level_slot;
pub use info::{ClosureInfo, ClosureSlotValue};

// Hermes bytecode uses an "Environment" system for closures.
// Instead of named variables, inner functions access variables via (Environment Index, Slot Index) pairs.
// This pass translates `LoadFromEnvironment(env, slot)` instructions into named variables
// like `outer0_1` or recovers original names if debug info is available.
pub fn resolve_closures(stmts: Vec<Statement>, info: &ClosureInfo) -> Vec<Statement> {
    stmts.into_iter().map(|s| resolve_stmt(s, info)).collect()
}

fn resolve_stmt(stmt: Statement, info: &ClosureInfo) -> Statement {
    match stmt {
        Statement::Assign { target, value } => Statement::Assign {
            target: resolve_target(target, info),
            value: resolve_expr(value, info),
        },
        Statement::Expr(e) => Statement::Expr(resolve_expr(e, info)),
        Statement::Return(Some(e)) => Statement::Return(Some(resolve_expr(e, info))),
        Statement::Throw(e) => Statement::Throw(resolve_expr(e, info)),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition: resolve_expr(condition, info),
            then_body: resolve_closures(then_body, info),
            else_body: resolve_closures(else_body, info),
        },
        Statement::While { condition, body } => Statement::While {
            condition: resolve_expr(condition, info),
            body: resolve_closures(body, info),
        },
        Statement::For {
            init,
            condition,
            update,
            body,
        } => Statement::For {
            init: init.map(|s| Box::new(resolve_stmt(*s, info))),
            condition: condition.map(|c| resolve_expr(c, info)),
            update: update.map(|s| Box::new(resolve_stmt(*s, info))),
            body: resolve_closures(body, info),
        },
        Statement::Block(inner) => Statement::Block(resolve_closures(inner, info)),
        other => other,
    }
}

fn resolve_target(target: AssignTarget, info: &ClosureInfo) -> AssignTarget {
    match target {
        AssignTarget::ClosureVar { level, slot } => {
            // Try to resolve using encoded level+slot first, then fall back to slot-only.
            // Level 0 usually means the immediate parent scope's environment.
            let encoded = encode_level_slot(level, slot);
            let name = if info.slots.contains_key(&encoded) {
                // We found a name in our closure info map (likely from debug info or prior analysis)
                info.get_slot_name(encoded)
            } else if level == 0 {
                // Fallback for immediate parent scope
                info.get_slot_name(slot)
            } else {
                // If we have no name, generate a unique identifier based on location.
                // This ensures re-compilability even if readability is low.
                format!("outer{level}_{slot}")
            };
            AssignTarget::Variable(name)
        }
        AssignTarget::Member { object, property } => AssignTarget::Member {
            object: resolve_expr(object, info),
            property,
        },
        AssignTarget::Index { object, key } => AssignTarget::Index {
            object: resolve_expr(object, info),
            key: resolve_expr(key, info),
        },
        other => other,
    }
}

fn resolve_expr(expr: Expression, info: &ClosureInfo) -> Expression {
    match expr {
        Expression::Value(Value::ClosureVar { level, slot }) => {
            // Try to resolve using encoded level+slot first, then fall back to slot-only
            let encoded = encode_level_slot(level, slot);

            let name = if info.slots.contains_key(&encoded) {
                info.get_slot_name(encoded)
            } else if level == 0 {
                info.get_slot_name(slot)
            } else {
                format!("outer{level}_{slot}")
            };
            Expression::Value(Value::Variable(name))
        }
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(resolve_expr(*left, info)),
            right: Box::new(resolve_expr(*right, info)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(resolve_expr(*operand, info)),
        },
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(resolve_expr(*callee, info)),
            arguments: arguments
                .into_iter()
                .map(|a| resolve_expr(a, info))
                .collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(resolve_expr(*object, info)),
            property,
            optional,
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(resolve_expr(*callee, info)),
            arguments: arguments
                .into_iter()
                .map(|a| resolve_expr(a, info))
                .collect(),
        },
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => Expression::Conditional {
            condition: Box::new(resolve_expr(*condition, info)),
            then_expr: Box::new(resolve_expr(*then_expr, info)),
            else_expr: Box::new(resolve_expr(*else_expr, info)),
        },
        Expression::Array { elements } => Expression::Array {
            elements: elements
                .into_iter()
                .map(|e| e.map(|ex| resolve_expr(ex, info)))
                .collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .into_iter()
                .map(|mut p| {
                    p.value = resolve_expr(p.value, info);
                    p
                })
                .collect(),
        },
        other => other,
    }
}
