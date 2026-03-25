use crate::ir::{AssignTarget, BlockId, Constant, Expression, PropertyKey, Statement, Value, CFG};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct PropagationConfig {
    pub max_iterations: usize,
}

impl PropagationConfig {
    pub fn new() -> Self {
        Self { max_iterations: 10 }
    }
}

pub fn propagate(cfg: &mut CFG, config: &PropagationConfig) {
    let max_iter = if config.max_iterations == 0 {
        10
    } else {
        config.max_iterations
    };

    for _ in 0..max_iter {
        let mut changed = false;

        for block_id in cfg.block_ids().collect::<Vec<_>>() {
            if propagate_block(cfg, block_id) {
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

fn propagate_block(cfg: &mut CFG, block_id: BlockId) -> bool {
    let block = match cfg.get_mut(block_id) {
        Some(b) => b,
        None => return false,
    };

    // Build map of simple assignments: reg -> value
    let mut copies: BTreeMap<u32, Expression> = BTreeMap::new();
    let mut changed = false;

    // Take ownership instead of cloning
    let statements = std::mem::take(&mut block.statements);
    let mut new_statements = Vec::with_capacity(statements.len());

    for stmt in statements {
        // Substitute uses
        let substituted = substitute_stmt(&stmt, &copies);
        if substituted != stmt {
            changed = true;
        }

        // Track definitions
        if let Statement::Assign {
            target: AssignTarget::Register(r),
            value,
        } = &substituted
        {
            if is_propagatable(value) {
                copies.insert(*r, value.clone());
            } else {
                copies.remove(r);
            }
        }

        new_statements.push(substituted);
    }

    // Put back the statements
    if let Some(block) = cfg.get_mut(block_id) {
        block.statements = new_statements;
    }

    changed
}

fn is_propagatable(expr: &Expression) -> bool {
    match expr {
        Expression::Value(_) => true,
        // Allow propagation of simple member access on known safe objects
        // e.g., `Object = globalThis.Object` → inline `globalThis.Object`
        Expression::Member {
            object,
            property: PropertyKey::Ident(_),
            optional: false,
        } => matches!(
            object.as_ref(),
            Expression::Value(Value::Global)
                | Expression::Value(Value::Constant(Constant::String(_)))
        ),
        _ => false,
    }
}

fn substitute_stmt(stmt: &Statement, copies: &BTreeMap<u32, Expression>) -> Statement {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CFGBuilder, Constant};

    #[test]
    fn test_constant_propagation() {
        let mut builder = CFGBuilder::new();
        builder.emit(Statement::assign_reg(
            0,
            Expression::constant(Constant::Integer(42)),
        ));
        builder.emit(Statement::assign_reg(
            1,
            Expression::Value(Value::Register(0)),
        ));
        builder.emit_return(Some(Expression::Value(Value::Register(1))));

        let mut cfg = builder.finish();
        propagate(&mut cfg, &PropagationConfig::new());

        let block = cfg.entry_block();
        // After propagation, r1 should be assigned 42, not r0
        if let Statement::Assign { value, .. } = &block.statements[1] {
            assert_eq!(*value, Expression::constant(Constant::Integer(42)));
        }
    }
}
