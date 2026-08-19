use crate::ir::{AssignTarget, Expression, ObjectProperty, Statement, Value, stmt_has_side_effects};
use super::is_reg_used;

// Fold `obj = { k0:v0, k1:<placeholder>, ... }; obj[N] = val` (a slot-index
// fill from PutOwnBySlotIdx) into the literal's Nth property. Only replaces a
// placeholder value (null/undefined/empty), which is what the shape-table form
// leaves for non-serializable property values, so a genuine numeric-key write
// is never absorbed. Matches both still-register objects and named
// Variable/Let objects (after register naming).
pub fn fold_slot_index_fills(statements: &mut Vec<Statement>) {
    // Mark consumed fills and drop them all in one pass. Removing each fill inside
    // the loop shifts the tail on every fill, which is O(n^2) on a large function.
    let mut consumed = vec![false; statements.len()];
    let mut i = 0;
    while i < statements.len() {
        let Some((obj, prop_count)) = object_literal_def(&statements[i]) else {
            i += 1;
            continue;
        };

        let mut j = i + 1;
        while j < statements.len() {
            if let Some((slot, val)) = slot_index_fill(&statements[j], &obj, prop_count) {
                if let Some(properties) = object_properties_mut(&mut statements[i]) {
                    if is_placeholder(&properties[slot].value) {
                        properties[slot].value = val;
                        consumed[j] = true;
                        j += 1;
                        continue;
                    }
                }
                break;
            } else if obj_reassigned(&statements[j], &obj)
                || obj_used(&statements[j], &obj)
                || stmt_has_side_effects(&statements[j])
            {
                break;
            }
            j += 1;
        }
        i += 1;
    }
    if consumed.iter().any(|&c| c) {
        let mut idx = 0;
        statements.retain(|_| {
            let keep = !consumed[idx];
            idx += 1;
            keep
        });
    }
}

enum ObjRef {
    Register(u32),
    Name(String),
}

fn object_literal_def(stmt: &Statement) -> Option<(ObjRef, usize)> {
    match stmt {
        Statement::Assign {
            target: AssignTarget::Register(r),
            value: Expression::Object { properties },
        } if !properties.is_empty() => Some((ObjRef::Register(*r), properties.len())),
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value: Expression::Object { properties },
        } if !properties.is_empty() => Some((ObjRef::Name(name.clone()), properties.len())),
        Statement::Let {
            name,
            value: Expression::Object { properties },
            ..
        } if !properties.is_empty() => Some((ObjRef::Name(name.clone()), properties.len())),
        _ => None,
    }
}

fn object_properties_mut(stmt: &mut Statement) -> Option<&mut Vec<ObjectProperty>> {
    match stmt {
        Statement::Assign {
            value: Expression::Object { properties },
            ..
        }
        | Statement::Let {
            value: Expression::Object { properties },
            ..
        } => Some(properties),
        _ => None,
    }
}

// `obj[N] = val` with a constant N < prop_count → (N, val).
fn slot_index_fill(stmt: &Statement, obj: &ObjRef, prop_count: usize) -> Option<(usize, Expression)> {
    let Statement::Assign {
        target: AssignTarget::Index { object, key },
        value,
    } = stmt
    else {
        return None;
    };
    let matches_obj = match (obj, object) {
        (ObjRef::Register(r), Expression::Value(Value::Register(r2))) => r == r2,
        (ObjRef::Name(n), Expression::Value(Value::Variable(n2))) => n == n2,
        _ => false,
    };
    if !matches_obj {
        return None;
    }
    let n = match key {
        Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) if *n >= 0 => {
            *n as usize
        }
        _ => return None,
    };
    if n < prop_count {
        Some((n, value.clone()))
    } else {
        None
    }
}

fn obj_reassigned(stmt: &Statement, obj: &ObjRef) -> bool {
    match (obj, stmt) {
        (
            ObjRef::Register(r),
            Statement::Assign {
                target: AssignTarget::Register(r2),
                ..
            },
        ) => r == r2,
        (
            ObjRef::Name(n),
            Statement::Assign {
                target: AssignTarget::Variable(n2),
                ..
            },
        ) => n == n2,
        (ObjRef::Name(n), Statement::Let { name, .. }) => name == n,
        _ => false,
    }
}

fn obj_used(stmt: &Statement, obj: &ObjRef) -> bool {
    match obj {
        ObjRef::Register(r) => is_reg_used(stmt, *r),
        ObjRef::Name(n) => stmt_uses_var(stmt, n),
    }
}

fn stmt_uses_var(stmt: &Statement, name: &str) -> bool {
    match stmt {
        Statement::Assign { target, value } => {
            target_uses_var(target, name) || expr_uses_var(value, name)
        }
        Statement::Let { value, .. } => expr_uses_var(value, name),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            expr_uses_var(e, name)
        }
        Statement::If { condition, .. }
        | Statement::While { condition, .. }
        | Statement::DoWhile { condition, .. } => expr_uses_var(condition, name),
        _ => false,
    }
}

fn target_uses_var(target: &AssignTarget, name: &str) -> bool {
    match target {
        AssignTarget::Member { object, .. } => expr_uses_var(object, name),
        AssignTarget::Index { object, key } => {
            expr_uses_var(object, name) || expr_uses_var(key, name)
        }
        _ => false,
    }
}

fn expr_uses_var(expr: &Expression, name: &str) -> bool {
    use crate::ir::Visitor;
    struct C<'a>(&'a str, bool);
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                if n == self.0 {
                    self.1 = true;
                    return;
                }
            }
            if !self.1 {
                self.walk_expression(e);
            }
        }
    }
    let mut c = C(name, false);
    c.visit_expression(expr);
    c.1
}

fn is_placeholder(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(
            crate::ir::Constant::Null | crate::ir::Constant::Undefined
        ))
    )
}
