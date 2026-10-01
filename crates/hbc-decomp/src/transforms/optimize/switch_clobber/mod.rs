// A switch case sometimes stores the discriminant back into the scrutinee.
// The copy is a different register (`r7 = r3.kind`) that var-naming calls
// `kind` because the property is `kind`. Renaming the parameter `arg0` to
// `kind` then prints `kind = kind.kind`, and `kind.voiceState` reads the
// string. Split only that same-name collision onto `{name}Value`. Member
// bases keep the object. Bare uses take the fresh binding.
//
// `node = node.next` is left alone: the property is a different name, and
// later member reads are supposed to see the updated value.

use crate::ir::{AssignTarget, Binding, Expression, PropertyKey, Statement, Value};

pub fn repair_switch_clobbers(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.into_iter().map(repair_stmt).collect()
}

fn repair_stmt(stmt: Statement) -> Statement {
    match stmt {
        Statement::Switch {
            discriminant,
            mut cases,
            mut default,
        } => {
            for (_, body) in cases.iter_mut() {
                repair_case_body(body);
            }
            if let Some(body) = default.as_mut() {
                repair_case_body(body);
            }
            Statement::Switch {
                discriminant,
                cases,
                default,
            }
        }
        other => {
            let mut s = other;
            crate::ir::map_nested_bodies_mut(&mut s, repair_switch_clobbers);
            s
        }
    }
}

fn repair_case_body(body: &mut [Statement]) {
    let mut i = 0;
    while i < body.len() {
        if let Statement::Block(inner) = &mut body[i] {
            repair_case_body(inner);
        } else if let Some((old, new_name)) = clobber_binding(&body[i]) {
            rename_clobber_target(&mut body[i], &new_name);
            for stmt in body.iter_mut().skip(i + 1) {
                if assigns_var(stmt, &old) {
                    break;
                }
                rewrite_value_uses(stmt, &old, &new_name);
            }
        } else {
            recurse_nested(&mut body[i]);
        }
        i += 1;
    }
}

fn recurse_nested(stmt: &mut Statement) {
    match stmt {
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            repair_case_body(then_body);
            repair_case_body(else_body);
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. }
        | Statement::Block(body) => repair_case_body(body),
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => {
            repair_case_body(try_body);
            repair_case_body(catch_body);
            repair_case_body(finally_body);
        }
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases.iter_mut() {
                repair_case_body(body);
            }
            if let Some(body) = default {
                repair_case_body(body);
            }
        }
        _ => {}
    }
}

fn rename_clobber_target(stmt: &mut Statement, new_name: &str) {
    let new_target = AssignTarget::Binding(Binding::Variable(new_name.to_string()));
    match stmt {
        Statement::Assign { target, .. } => *target = new_target,
        Statement::Let { name, .. } => *name = new_name.to_string(),
        Statement::Expr(Expression::Assignment { value, .. }) => {
            let value = value.as_ref().clone();
            *stmt = Statement::Assign {
                target: new_target,
                value,
            };
        }
        _ => {}
    }
}

fn binding_and_value(stmt: &Statement) -> Option<(&str, &Expression)> {
    match stmt {
        Statement::Assign { target, value } => match target {
            AssignTarget::Binding(Binding::Variable(name)) => Some((name, value)),
            _ => None,
        },
        Statement::Let { name, value, .. } => Some((name, value)),
        Statement::Expr(Expression::Assignment { target, value }) => match target.as_ref() {
            AssignTarget::Binding(Binding::Variable(name)) => Some((name, value.as_ref())),
            _ => None,
        },
        _ => None,
    }
}

fn clobber_binding(stmt: &Statement) -> Option<(String, String)> {
    let (name, value) = binding_and_value(stmt)?;
    let Expression::Member {
        object, property, ..
    } = value
    else {
        return None;
    };
    let Expression::Value(Value::Binding(Binding::Variable(base))) = object.as_ref() else {
        return None;
    };
    if base != name {
        return None;
    }
    let prop = match property {
        PropertyKey::Ident(prop) | PropertyKey::String(prop) => prop.as_str(),
        _ => return None,
    };
    // Same spelling on the object, the property, and the copy. That is the
    // parameter/temp collision, not an update like `node = node.next`.
    if prop != name {
        return None;
    }
    Some((name.to_string(), format!("{name}Value")))
}

fn assigns_var(stmt: &Statement, name: &str) -> bool {
    matches!(
        stmt,
        Statement::Assign { target: AssignTarget::Binding(Binding::Variable(n)), .. }
        | Statement::Let { name: n, .. }
            if n == name
    )
}

fn rewrite_value_uses(stmt: &mut Statement, old: &str, new_name: &str) {
    match stmt {
        Statement::Expr(e) | Statement::Throw(e) | Statement::Return(Some(e)) => {
            rewrite_expr(e, old, new_name);
        }
        Statement::Let { value, .. } | Statement::Assign { value, .. } => {
            rewrite_expr(value, old, new_name);
        }
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            rewrite_expr(condition, old, new_name);
            for s in then_body.iter_mut().chain(else_body.iter_mut()) {
                rewrite_value_uses(s, old, new_name);
            }
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            rewrite_expr(condition, old, new_name);
            for s in body.iter_mut() {
                rewrite_value_uses(s, old, new_name);
            }
        }
        Statement::Block(inner) => {
            for s in inner.iter_mut() {
                rewrite_value_uses(s, old, new_name);
            }
        }
        _ => {}
    }
}

fn rewrite_expr(expr: &mut Expression, old: &str, new_name: &str) {
    match expr {
        Expression::Member {
            object, property, ..
        } => {
            let base_is_old = matches!(
                object.as_ref(),
                Expression::Value(Value::Binding(Binding::Variable(n))) if n == old
            );
            if !base_is_old {
                rewrite_expr(object, old, new_name);
            }
            if let PropertyKey::Computed(k) = property {
                rewrite_expr(k, old, new_name);
            }
        }
        Expression::Value(Value::Binding(Binding::Variable(n))) if n == old => {
            *n = new_name.to_string();
        }
        Expression::Unary { operand, .. } => rewrite_expr(operand, old, new_name),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_expr(condition, old, new_name);
            rewrite_expr(then_expr, old, new_name);
            rewrite_expr(else_expr, old, new_name);
        }
        Expression::Binary { left, right, .. } => {
            rewrite_expr(left, old, new_name);
            rewrite_expr(right, old, new_name);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            rewrite_expr(callee, old, new_name);
            for arg in arguments {
                rewrite_expr(arg, old, new_name);
            }
        }
        Expression::Assignment { value, .. } => rewrite_expr(value, old, new_name),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
