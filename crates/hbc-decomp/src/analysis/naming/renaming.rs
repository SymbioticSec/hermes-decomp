use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;

pub fn rename_registers(stmts: Vec<Statement>, names: &BTreeMap<u32, String>) -> Vec<Statement> {
    stmts.into_iter().map(|s| rename_stmt(s, names)).collect()
}

fn rename_stmt(stmt: Statement, names: &BTreeMap<u32, String>) -> Statement {
    match stmt {
        Statement::Assign { target, value } => Statement::Assign {
            target: rename_target(target, names),
            value: rename_expr(value, names),
        },
        Statement::Expr(e) => Statement::Expr(rename_expr(e, names)),
        Statement::Return(Some(e)) => Statement::Return(Some(rename_expr(e, names))),
        Statement::Throw(e) => Statement::Throw(rename_expr(e, names)),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition: rename_expr(condition, names),
            then_body: rename_registers(then_body, names),
            else_body: rename_registers(else_body, names),
        },
        Statement::While { condition, body } => Statement::While {
            condition: rename_expr(condition, names),
            body: rename_registers(body, names),
        },
        Statement::Block(inner) => Statement::Block(rename_registers(inner, names)),
        Statement::Let { name, value, kind } => Statement::Let {
            name,
            value: rename_expr(value, names),
            kind,
        },
        Statement::For { init, condition, update, body } => Statement::For {
            init: init.map(|s| Box::new(rename_stmt(*s, names))),
            condition: condition.map(|e| rename_expr(e, names)),
            update: update.map(|s| Box::new(rename_stmt(*s, names))),
            body: rename_registers(body, names),
        },
        Statement::DoWhile { body, condition } => Statement::DoWhile {
            body: rename_registers(body, names),
            condition: rename_expr(condition, names),
        },
        Statement::ForIn { variable, object, body } => Statement::ForIn {
            variable: rename_string_var(variable, names),
            object: rename_expr(object, names),
            body: rename_registers(body, names),
        },
        Statement::ForOf { variable, iterable, body } => Statement::ForOf {
            variable: rename_string_var(variable, names),
            iterable: rename_expr(iterable, names),
            body: rename_registers(body, names),
        },
        Statement::Switch { discriminant, cases, default } => Statement::Switch {
            discriminant: rename_expr(discriminant, names),
            cases: cases.into_iter().map(|(val, body)| {
                (rename_expr(val, names), rename_registers(body, names))
            }).collect(),
            default: default.map(|d| rename_registers(d, names)),
        },
        Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => Statement::TryCatch {
            try_body: rename_registers(try_body, names),
            catch_param: catch_param.map(|p| rename_string_var(p, names)),
            catch_body: rename_registers(catch_body, names),
            finally_body: rename_registers(finally_body, names),
        },
        Statement::Class { name, super_class, constructor, methods } => {
            // Rename class name if it's a register pattern (r10xxx)
            let new_name = if let Some(reg_num) = name.strip_prefix('r').and_then(|s| s.parse::<u32>().ok()) {
                names.get(&reg_num).cloned().unwrap_or(name)
            } else {
                name
            };
            Statement::Class {
                name: new_name,
                super_class: super_class.map(|e| rename_expr(e, names)),
                constructor: constructor.map(|s| Box::new(rename_stmt(*s, names))),
                methods: methods.into_iter().map(|mut m| {
                    m.value = rename_expr(m.value, names);
                    m
                }).collect(),
            }
        },
        other => other,
    }
}

// Rename a string variable that might be a register name (e.g., "r10027" → "key").
fn rename_string_var(var: String, names: &BTreeMap<u32, String>) -> String {
    if let Some(reg_num) = var.strip_prefix('r').and_then(|s| s.parse::<u32>().ok()) {
        names.get(&reg_num).cloned().unwrap_or(var)
    } else {
        var
    }
}

fn rename_target(target: AssignTarget, names: &BTreeMap<u32, String>) -> AssignTarget {
    match target {
        AssignTarget::Register(r) => {
            if let Some(name) = names.get(&r) {
                AssignTarget::Variable(name.clone())
            } else {
                AssignTarget::Register(r)
            }
        }
        AssignTarget::Member { object, property } => AssignTarget::Member {
            object: rename_expr(object, names),
            property,
        },
        AssignTarget::Index { object, key } => AssignTarget::Index {
            object: rename_expr(object, names),
            key: rename_expr(key, names),
        },
        AssignTarget::DestructuringObject(props) => {
            AssignTarget::DestructuringObject(
                props.into_iter()
                    .map(|(k, t, def)| (k, rename_target(t, names), def.map(|e| rename_expr(e, names))))
                    .collect()
            )
        }
        AssignTarget::DestructuringObjectRest { properties, rest } => {
            AssignTarget::DestructuringObjectRest {
                properties: properties.into_iter()
                    .map(|(k, t, def)| (k, rename_target(t, names), def.map(|e| rename_expr(e, names))))
                    .collect(),
                rest: Box::new(rename_target(*rest, names)),
            }
        }
        AssignTarget::DestructuringArray(elements) => {
            AssignTarget::DestructuringArray(
                elements.into_iter()
                    .map(|e| e.map(|(t, def)| (rename_target(t, names), def.map(|d| rename_expr(d, names)))))
                    .collect()
            )
        }
        AssignTarget::DestructuringArrayRest { elements, rest } => {
            AssignTarget::DestructuringArrayRest {
                elements: elements.into_iter()
                    .map(|e| e.map(|(t, def)| (rename_target(t, names), def.map(|d| rename_expr(d, names)))))
                    .collect(),
                rest: Box::new(rename_target(*rest, names)),
            }
        }
        other => other,
    }
}

fn rename_expr(expr: Expression, names: &BTreeMap<u32, String>) -> Expression {
    match expr {
        Expression::Value(Value::Register(r)) => {
            if let Some(name) = names.get(&r) {
                Expression::Value(Value::Variable(name.clone()))
            } else {
                Expression::Value(Value::Register(r))
            }
        }
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(rename_expr(*left, names)),
            right: Box::new(rename_expr(*right, names)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(rename_expr(*operand, names)),
        },
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(rename_expr(*callee, names)),
            arguments: arguments
                .into_iter()
                .map(|a| rename_expr(a, names))
                .collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(rename_expr(*object, names)),
            property: match property {
                PropertyKey::Computed(k) => PropertyKey::Computed(Box::new(rename_expr(*k, names))),
                other => other,
            },
            optional,
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(rename_expr(*callee, names)),
            arguments: arguments
                .into_iter()
                .map(|a| rename_expr(a, names))
                .collect(),
        },
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => Expression::Conditional {
            condition: Box::new(rename_expr(*condition, names)),
            then_expr: Box::new(rename_expr(*then_expr, names)),
            else_expr: Box::new(rename_expr(*else_expr, names)),
        },
        Expression::Array { elements } => Expression::Array {
            elements: elements
                .into_iter()
                .map(|e| e.map(|ex| rename_expr(ex, names)))
                .collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .into_iter()
                .map(|mut p| {
                    p.value = rename_expr(p.value, names);
                    if let PropertyKey::Computed(k) = p.key {
                        p.key = PropertyKey::Computed(Box::new(rename_expr(*k, names)));
                    }
                    p
                })
                .collect(),
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(rename_expr(*target, names)),
            value: Box::new(rename_expr(*value, names)),
        },
        Expression::Spread(inner) => Expression::Spread(Box::new(rename_expr(*inner, names))),
        Expression::TemplateLiteral { quasis, expressions } => Expression::TemplateLiteral {
            quasis,
            expressions: expressions.into_iter().map(|e| rename_expr(e, names)).collect(),
        },
        Expression::Yield { value, delegate } => Expression::Yield {
            value: Box::new(rename_expr(*value, names)),
            delegate,
        },
        Expression::Await(inner) => Expression::Await(Box::new(rename_expr(*inner, names))),
        other => other,
    }
}

// Rename variables in statements in-place.
pub fn rename_variables_in_stmts(stmts: &mut [Statement], renames: &BTreeMap<String, String>) {
    for stmt in stmts {
        rename_variables_in_stmt(stmt, renames);
    }
}

fn rename_variables_in_stmt(stmt: &mut Statement, renames: &BTreeMap<String, String>) {
    match stmt {
        Statement::Assign { target, value } => {
            rename_variables_in_target(target, renames);
            rename_variables_in_expr(value, renames);
        }
        Statement::Expr(e) => rename_variables_in_expr(e, renames),
        Statement::Return(Some(e)) => rename_variables_in_expr(e, renames),
        Statement::Throw(e) => rename_variables_in_expr(e, renames),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            rename_variables_in_expr(condition, renames);
            rename_variables_in_stmts(then_body, renames);
            rename_variables_in_stmts(else_body, renames);
        }
        Statement::While { condition, body } => {
            rename_variables_in_expr(condition, renames);
            rename_variables_in_stmts(body, renames);
        }
        Statement::DoWhile { body, condition } => {
            rename_variables_in_stmts(body, renames);
            rename_variables_in_expr(condition, renames);
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(i) = init {
                rename_variables_in_stmt(i, renames);
            }
            if let Some(c) = condition {
                rename_variables_in_expr(c, renames);
            }
            if let Some(u) = update {
                rename_variables_in_stmt(u, renames);
            }
            rename_variables_in_stmts(body, renames);
        }
        Statement::Block(inner)
        | Statement::ForOf { body: inner, .. }
        | Statement::ForIn { body: inner, .. } => {
            rename_variables_in_stmts(inner, renames);
        }
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => {
            rename_variables_in_expr(discriminant, renames);
            for (val, body) in cases {
                rename_variables_in_expr(val, renames);
                rename_variables_in_stmts(body, renames);
            }
            if let Some(d) = default {
                rename_variables_in_stmts(d, renames);
            }
        }
        _ => {}
    }
}

fn rename_variables_in_target(target: &mut AssignTarget, renames: &BTreeMap<String, String>) {
    match target {
        AssignTarget::Variable(name) => {
            if let Some(new_name) = renames.get(name) {
                *name = new_name.clone();
            }
        }
        AssignTarget::Member { object, .. } => rename_variables_in_expr(object, renames),
        AssignTarget::Index { object, key } => {
            rename_variables_in_expr(object, renames);
            rename_variables_in_expr(key, renames);
        }
        _ => {}
    }
}

fn rename_variables_in_expr(expr: &mut Expression, renames: &BTreeMap<String, String>) {
    match expr {
        Expression::Value(Value::Variable(name)) => {
            if let Some(new_name) = renames.get(name) {
                *name = new_name.clone();
            }
        }
        Expression::Binary { left, right, .. } => {
            rename_variables_in_expr(left, renames);
            rename_variables_in_expr(right, renames);
        }
        Expression::Unary { operand, .. } => rename_variables_in_expr(operand, renames),
        Expression::Call { callee, arguments } => {
            rename_variables_in_expr(callee, renames);
            for arg in arguments {
                rename_variables_in_expr(arg, renames);
            }
        }
        Expression::Member {
            object, property, ..
        } => {
            rename_variables_in_expr(object, renames);
            if let PropertyKey::Computed(k) = property {
                rename_variables_in_expr(k, renames);
            }
        }
        Expression::New { callee, arguments } => {
            rename_variables_in_expr(callee, renames);
            for arg in arguments {
                rename_variables_in_expr(arg, renames);
            }
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            rename_variables_in_expr(condition, renames);
            rename_variables_in_expr(then_expr, renames);
            rename_variables_in_expr(else_expr, renames);
        }
        Expression::Array { elements } => {
            for elem in elements.iter_mut().flatten() {
                rename_variables_in_expr(elem, renames);
            }
        }
        Expression::Object { properties } => {
            for prop in properties {
                rename_variables_in_expr(&mut prop.value, renames);
                if let PropertyKey::Computed(k) = &mut prop.key {
                    rename_variables_in_expr(k, renames);
                }
            }
        }
        Expression::Assignment { target, value } => {
            rename_variables_in_expr(target, renames);
            rename_variables_in_expr(value, renames);
        }
        _ => {}
    }
}
