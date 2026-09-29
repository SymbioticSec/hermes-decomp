// Reconstructs JSXElement nodes from React.createElement and _jsx / _jsxs calls.
//
// Two-phase approach:
// 1. Resolve 1-hop object props: `const p = { a: 1 }; jsx(Tag, p)` → use `{ a: 1 }`
// 2. Match factory calls and lower to Expression::JSXElement

use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Constant, Expression, MutVisitor, ObjectProperty,
    PropertyKey, Statement, Value,
};
use std::collections::{BTreeMap, HashSet};

/// Run JSX reconstruction over a statement list (idempotent).
pub fn reconstruct_jsx(mut stmts: Vec<Statement>) -> Vec<Statement> {
    stmts = resolve_prop_object_vars(stmts);
    JSXReconstructor::new().visit_statement_list(&mut stmts);
    stmts
}

// ---------------------------------------------------------------------------
// Phase 1, 1-hop props resolution (same block, sequential)
// ---------------------------------------------------------------------------

fn resolve_prop_object_vars(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out = Vec::with_capacity(stmts.len());
    let mut objects: BTreeMap<String, Expression> = BTreeMap::new();

    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, kind } => {
                let value = maybe_subst_call(value, &objects);
                if matches!(value, Expression::Object { .. }) {
                    objects.insert(name.clone(), value.clone());
                }
                out.push(Statement::Let { name, value, kind });
            }
            Statement::Assign {
                target: AssignTarget::Variable(name),
                value,
            } => {
                let value = maybe_subst_call(value, &objects);
                if matches!(value, Expression::Object { .. }) {
                    objects.insert(name.clone(), value.clone());
                } else {
                    objects.remove(&name);
                }
                out.push(Statement::Assign {
                    target: AssignTarget::Variable(name),
                    value,
                });
            }
            other => {
                let mut s = other;
                // Nested blocks get their own scope (fresh map via recursion).
                map_nested_bodies_mut(&mut s, resolve_prop_object_vars);
                // Also rewrite jsx calls at this level if Assign/Expr not caught above.
                rewrite_stmt_calls(&mut s, &objects);
                out.push(s);
            }
        }
    }
    out
}

fn maybe_subst_call(mut expr: Expression, objects: &BTreeMap<String, Expression>) -> Expression {
    subst_jsx_props_in_expr(&mut expr, objects);
    expr
}

fn rewrite_stmt_calls(stmt: &mut Statement, objects: &BTreeMap<String, Expression>) {
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            subst_jsx_props_in_expr(e, objects);
        }
        Statement::Assign { value, .. } | Statement::Let { value, .. } => {
            subst_jsx_props_in_expr(value, objects);
        }
        _ => {}
    }
}

fn subst_jsx_props_in_expr(expr: &mut Expression, objects: &BTreeMap<String, Expression>) {
    subst_jsx_props_in_expr_inner(expr, objects, &mut HashSet::new());
}

fn subst_jsx_props_in_expr_inner(
    expr: &mut Expression,
    objects: &BTreeMap<String, Expression>,
    expanding: &mut HashSet<String>,
) {
    match expr {
        Expression::Call { callee, arguments } if is_jsx_call(callee) && arguments.len() >= 2 => {
            let mut expanded_name = None;
            if let Expression::Value(Value::Variable(name)) = &arguments[1] {
                if let Some(obj) = objects.get(name) {
                    if expanding.insert(name.clone()) {
                        expanded_name = Some(name.clone());
                        arguments[1] = obj.clone();
                    }
                }
            }
            // Keep the variable reference when the tracked object reaches the
            // JSX call that is currently expanding it.  Replacing it again
            // would clone the same object forever.
            subst_jsx_props_in_expr_inner(&mut arguments[1], objects, expanding);
            if let Some(name) = expanded_name {
                expanding.remove(&name);
            }
            // Recurse into the other arguments once. Classic createElement
            // children can contain JSX calls too.
            for (index, argument) in arguments.iter_mut().enumerate() {
                if index != 1 {
                    subst_jsx_props_in_expr_inner(argument, objects, expanding);
                }
            }
            subst_jsx_props_in_expr_inner(callee, objects, expanding);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            subst_jsx_props_in_expr_inner(callee, objects, expanding);
            for a in arguments {
                subst_jsx_props_in_expr_inner(a, objects, expanding);
            }
        }
        Expression::Binary { left, right, .. } => {
            subst_jsx_props_in_expr_inner(left, objects, expanding);
            subst_jsx_props_in_expr_inner(right, objects, expanding);
        }
        Expression::Unary { operand, .. }
        | Expression::Spread(operand)
        | Expression::Await(operand)
        | Expression::Yield { value: operand, .. } => {
            subst_jsx_props_in_expr_inner(operand, objects, expanding)
        }
        Expression::Member { object, .. } => {
            subst_jsx_props_in_expr_inner(object, objects, expanding)
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            subst_jsx_props_in_expr_inner(condition, objects, expanding);
            subst_jsx_props_in_expr_inner(then_expr, objects, expanding);
            subst_jsx_props_in_expr_inner(else_expr, objects, expanding);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() {
                subst_jsx_props_in_expr_inner(e, objects, expanding);
            }
        }
        Expression::Object { properties } => {
            for p in properties {
                subst_jsx_props_in_expr_inner(&mut p.value, objects, expanding);
            }
        }
        Expression::Assignment { target, value } => {
            subst_jsx_props_in_expr_inner(target, objects, expanding);
            subst_jsx_props_in_expr_inner(value, objects, expanding);
        }
        Expression::JSXElement {
            attributes,
            children,
            ..
        } => {
            for (_, v) in attributes {
                subst_jsx_props_in_expr_inner(v, objects, expanding);
            }
            for c in children {
                subst_jsx_props_in_expr_inner(c, objects, expanding);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Phase 2, factory match → JSXElement
// ---------------------------------------------------------------------------

pub struct JSXReconstructor;

impl JSXReconstructor {
    pub fn new() -> Self {
        Self
    }
}

impl MutVisitor for JSXReconstructor {
    fn visit_expression(&mut self, expr: &mut Expression) {
        self.walk_expression(expr);
        if let Expression::Call { callee, arguments } = expr {
            if is_jsx_call(callee) && !arguments.is_empty() {
                if let Some(jsx_node) = build_jsx_element(callee, arguments) {
                    *expr = jsx_node;
                }
            }
        }
    }
}

fn jsx_factory_name(callee: &Expression) -> Option<&str> {
    let raw = match callee {
        Expression::Member {
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } => p.as_str(),
        Expression::Value(Value::Variable(n)) => n.as_str(),
        _ => return None,
    };
    // Strip leading underscores and common runtime prefixes.
    let stripped = raw.strip_prefix('_').unwrap_or(raw);
    Some(stripped)
}

fn is_jsx_call(callee: &Expression) -> bool {
    matches!(
        jsx_factory_name(callee),
        Some(
            "createElement"
                | "jsx"
                | "jsxs"
                | "jsxDEV"
                | "jsxsDEV"
                | "jsxDev"
                | "jsxsDev"
        )
    )
}

fn is_modern_factory(callee: &Expression) -> bool {
    matches!(
        jsx_factory_name(callee),
        Some("jsx" | "jsxs" | "jsxDEV" | "jsxsDEV" | "jsxDev" | "jsxsDev")
    )
}

fn build_jsx_element(callee: &Expression, arguments: &[Expression]) -> Option<Expression> {
    // JSX tags must be identifiers or member paths (or string HTML tags).
    // Calls like `importDefault(36)` are valid createElement first-args but NOT
    // valid JSX tag forms, leave those as jsx()/createElement() calls.
    let tag_name = match &arguments[0] {
        Expression::Value(Value::Constant(Constant::String(s))) => s.clone(),
        Expression::Value(Value::Variable(v)) => v.clone(),
        Expression::Member { object, property, .. } => {
            if let (Expression::Value(Value::Variable(obj_name)), PropertyKey::Ident(prop_name)) =
                (object.as_ref(), property)
            {
                format!("{obj_name}.{prop_name}")
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let tag_name = if tag_name == "Fragment"
        || tag_name == "_Fragment"
        || tag_name.ends_with(".Fragment")
        || tag_name == "React.Fragment"
    {
        String::new()
    } else {
        tag_name
    };

    let is_modern = is_modern_factory(callee);
    let mut jsx_attributes = Vec::new();
    let mut jsx_children = Vec::new();

    if is_modern && arguments.len() >= 3 {
        if !matches!(
            arguments[2],
            Expression::Value(Value::Constant(Constant::Undefined | Constant::Null))
        ) {
            jsx_attributes.push(("key".to_string(), arguments[2].clone()));
        }
    }

    if is_modern {
        if arguments.len() >= 2 {
            match &arguments[1] {
                Expression::Object { properties } => {
                    push_props(properties, &mut jsx_attributes, &mut jsx_children, true);
                }
                Expression::Value(Value::Constant(Constant::Null | Constant::Undefined)) => {}
                other => jsx_attributes.push(("...".to_string(), other.clone())),
            }
        }
    } else {
        if arguments.len() >= 2 {
            match &arguments[1] {
                Expression::Object { properties } => {
                    push_props(properties, &mut jsx_attributes, &mut jsx_children, false);
                }
                Expression::Value(Value::Constant(Constant::Null | Constant::Undefined)) => {}
                Expression::Spread(_) => {
                    jsx_attributes.push(("...".to_string(), arguments[1].clone()));
                }
                other => {
                    jsx_attributes.push((
                        "...".to_string(),
                        Expression::Spread(Box::new(other.clone())),
                    ));
                }
            }
        }
        for child in arguments.iter().skip(2) {
            jsx_children.push(child.clone());
        }
    }

    Some(Expression::JSXElement {
        tag: tag_name,
        attributes: jsx_attributes,
        children: jsx_children,
    })
}

fn push_props(
    properties: &[ObjectProperty],
    attrs: &mut Vec<(String, Expression)>,
    children: &mut Vec<Expression>,
    modern: bool,
) {
    for prop in properties {
        match &prop.key {
            PropertyKey::Ident(k) | PropertyKey::String(k) => {
                if modern && k == "children" {
                    if let Expression::Array { elements } = &prop.value {
                        children.extend(elements.iter().flatten().cloned());
                    } else {
                        children.push(prop.value.clone());
                    }
                } else {
                    attrs.push((k.clone(), prop.value.clone()));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::VarKind;

    fn create_element(props: Expression) -> Expression {
        Expression::call(
            Expression::member(
                Expression::Value(Value::Variable("React".into())),
                "createElement",
            ),
            vec![Expression::Value(Value::Variable("View".into())), props],
        )
    }

    #[test]
    fn props_substitution_stops_at_direct_cycle() {
        let nested = create_element(Expression::Value(Value::Variable("props".into())));
        let props = Expression::Object {
            properties: vec![ObjectProperty {
                key: PropertyKey::Ident("children".into()),
                value: nested,
            }],
        };
        let objects = BTreeMap::from([("props".into(), props.clone())]);
        let mut expr = create_element(Expression::Value(Value::Variable("props".into())));

        subst_jsx_props_in_expr(&mut expr, &objects);

        assert_eq!(expr, create_element(props));
    }

    #[test]
    fn test_classic_jsx_element() {
        let mut expr = Expression::call(
            Expression::member(
                Expression::Value(Value::Variable("React".to_string())),
                "createElement",
            ),
            vec![
                Expression::constant(Constant::String("div".to_string())),
                Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("id".to_string()),
                        value: Expression::constant(Constant::String("main".to_string())),
                    }],
                },
                Expression::constant(Constant::String("Text".to_string())),
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        assert!(matches!(expr, Expression::JSXElement { .. }));
    }

    #[test]
    fn resolves_props_variable_one_hop() {
        let stmts = vec![
            Statement::Let {
                name: "p".into(),
                value: Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("id".into()),
                        value: Expression::constant(Constant::String("x".into())),
                    }],
                },
                kind: VarKind::Let,
            },
            Statement::Expr(Expression::call(
                Expression::Value(Value::Variable("_jsx".into())),
                vec![
                    Expression::constant(Constant::String("div".into())),
                    Expression::Value(Value::Variable("p".into())),
                ],
            )),
        ];
        let out = reconstruct_jsx(stmts);
        match &out[1] {
            Statement::Expr(Expression::JSXElement { tag, attributes, .. }) => {
                assert_eq!(tag, "div");
                assert!(attributes.iter().any(|(k, _)| k == "id"));
            }
            other => panic!("expected jsx expr, got {other:?}"),
        }
    }

    #[test]
    fn test_modern_key_third_arg() {
        let mut expr = Expression::call(
            Expression::Value(Value::Variable("_jsx".into())),
            vec![
                Expression::Value(Value::Variable("Foo".into())),
                Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("title".into()),
                        value: Expression::constant(Constant::String("x".into())),
                    }],
                },
                Expression::Value(Value::Variable("k".into())),
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement { attributes, .. } => {
                assert!(attributes.iter().any(|(k, _)| k == "key"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn test_fragment_empty_tag() {
        let mut expr = Expression::call(
            Expression::Value(Value::Variable("jsxs".into())),
            vec![
                Expression::Value(Value::Variable("_Fragment".into())),
                Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("children".into()),
                        value: Expression::Array {
                            elements: vec![Some(Expression::Value(Value::Variable("a".into())))],
                        },
                    }],
                },
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement { tag, children, .. } => {
                assert_eq!(tag, "");
                assert_eq!(children.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn test_modern_jsx_member_factory() {
        let mut expr = Expression::call(
            Expression::member(Expression::Value(Value::Variable("jsxProd".into())), "jsxs"),
            vec![
                Expression::constant(Constant::String("ul".into())),
                Expression::Object {
                    properties: vec![
                        ObjectProperty {
                            key: PropertyKey::Ident("className".into()),
                            value: Expression::constant(Constant::String("x".into())),
                        },
                        ObjectProperty {
                            key: PropertyKey::Ident("children".into()),
                            value: Expression::Array {
                                elements: vec![
                                    Some(Expression::Value(Value::Variable("a".into()))),
                                    Some(Expression::Value(Value::Variable("b".into()))),
                                ],
                            },
                        },
                    ],
                },
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement {
                tag,
                attributes,
                children,
            } => {
                assert_eq!(tag, "ul");
                assert_eq!(attributes.len(), 1);
                assert_eq!(children.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }
}
