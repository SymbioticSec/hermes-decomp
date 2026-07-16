pub mod context;
pub mod info;
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};

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
        Statement::Delete { target, result } => Statement::Delete {
            target: resolve_expr(target, info),
            result,
        },
        Statement::Expr(e) => Statement::Expr(resolve_expr(e, info)),
        Statement::Return(Some(e)) => Statement::Return(Some(resolve_expr(e, info))),
        Statement::Throw(e) => Statement::Throw(resolve_expr(e, info)),
        Statement::Let { name, value, kind } => Statement::Let {
            name,
            value: resolve_expr(value, info),
            kind,
        },
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
        Statement::DoWhile { body, condition } => Statement::DoWhile {
            body: resolve_closures(body, info),
            condition: resolve_expr(condition, info),
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
        Statement::ForIn {
            variable,
            object,
            body,
        } => Statement::ForIn {
            variable,
            object: resolve_expr(object, info),
            body: resolve_closures(body, info),
        },
        Statement::ForOf {
            variable,
            iterable,
            body,
        } => Statement::ForOf {
            variable,
            iterable: resolve_expr(iterable, info),
            body: resolve_closures(body, info),
        },
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => Statement::Switch {
            discriminant: resolve_expr(discriminant, info),
            cases: cases
                .into_iter()
                .map(|(v, b)| (resolve_expr(v, info), resolve_closures(b, info)))
                .collect(),
            default: default.map(|d| resolve_closures(d, info)),
        },
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => Statement::TryCatch {
            try_body: resolve_closures(try_body, info),
            catch_param,
            catch_body: resolve_closures(catch_body, info),
            finally_body: resolve_closures(finally_body, info),
        },
        Statement::Class {
            name,
            super_class,
            constructor,
            methods,
        } => Statement::Class {
            name,
            super_class: super_class.map(|e| resolve_expr(e, info)),
            constructor: constructor.map(|s| Box::new(resolve_stmt(*s, info))),
            methods: methods
                .into_iter()
                .map(|mut m| {
                    m.value = resolve_expr(m.value, info);
                    m
                })
                .collect(),
        },
        Statement::CondGoto {
            condition,
            target,
            fallthrough,
        } => Statement::CondGoto {
            condition: resolve_expr(condition, info),
            target,
            fallthrough,
        },
        Statement::Block(inner) => Statement::Block(resolve_closures(inner, info)),
        other => other,
    }
}

fn resolve_target(target: AssignTarget, info: &ClosureInfo) -> AssignTarget {
    match target {
        AssignTarget::ClosureVar { level, slot } => {
            let encoded = encode_level_slot(level, slot);
            let name = if info.slots.contains_key(&encoded) {
                info.get_slot_name(encoded)
            } else if level == 0 {
                info.get_slot_name(slot)
            } else {
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
        AssignTarget::DestructuringObject(props) => AssignTarget::DestructuringObject(
            props
                .into_iter()
                .map(|(k, t, def)| {
                    (
                        k,
                        resolve_target(t, info),
                        def.map(|e| resolve_expr(e, info)),
                    )
                })
                .collect(),
        ),
        AssignTarget::DestructuringObjectRest { properties, rest } => {
            AssignTarget::DestructuringObjectRest {
                properties: properties
                    .into_iter()
                    .map(|(k, t, def)| {
                        (
                            k,
                            resolve_target(t, info),
                            def.map(|e| resolve_expr(e, info)),
                        )
                    })
                    .collect(),
                rest: Box::new(resolve_target(*rest, info)),
            }
        }
        AssignTarget::DestructuringArray(elements) => AssignTarget::DestructuringArray(
            elements
                .into_iter()
                .map(|e| e.map(|(t, def)| (resolve_target(t, info), def.map(|d| resolve_expr(d, info)))))
                .collect(),
        ),
        AssignTarget::DestructuringArrayRest { elements, rest } => {
            AssignTarget::DestructuringArrayRest {
                elements: elements
                    .into_iter()
                    .map(|e| {
                        e.map(|(t, def)| (resolve_target(t, info), def.map(|d| resolve_expr(d, info))))
                    })
                    .collect(),
                rest: Box::new(resolve_target(*rest, info)),
            }
        }
        AssignTarget::Rest(inner) => AssignTarget::Rest(Box::new(resolve_target(*inner, info))),
        other => other,
    }
}

fn resolve_property(property: PropertyKey, info: &ClosureInfo) -> PropertyKey {
    match property {
        PropertyKey::Computed(e) => PropertyKey::Computed(Box::new(resolve_expr(*e, info))),
        other => other,
    }
}

fn resolve_expr(expr: Expression, info: &ClosureInfo) -> Expression {
    match expr {
        Expression::Value(Value::ClosureVar { level, slot }) => {
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
            property: resolve_property(property, info),
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
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(resolve_expr(*target, info)),
            value: Box::new(resolve_expr(*value, info)),
        },
        Expression::Spread(e) => Expression::Spread(Box::new(resolve_expr(*e, info))),
        Expression::TemplateLiteral { quasis, expressions } => Expression::TemplateLiteral {
            quasis,
            expressions: expressions
                .into_iter()
                .map(|e| resolve_expr(e, info))
                .collect(),
        },
        Expression::Yield { value, delegate } => Expression::Yield {
            value: Box::new(resolve_expr(*value, info)),
            delegate,
        },
        Expression::Await(e) => Expression::Await(Box::new(resolve_expr(*e, info))),
        Expression::JSXElement {
            tag,
            attributes,
            children,
        } => Expression::JSXElement {
            tag,
            attributes: attributes
                .into_iter()
                .map(|(k, v)| (k, resolve_expr(v, info)))
                .collect(),
            children: children
                .into_iter()
                .map(|c| resolve_expr(c, info))
                .collect(),
        },
        other => other,
    }
}
