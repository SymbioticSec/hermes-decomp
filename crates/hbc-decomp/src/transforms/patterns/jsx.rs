use crate::ir::{Expression, MutVisitor, PropertyKey, Value, Constant};

// Reconstructs JSXElement nodes from React.createElement and _jsx / _jsxs calls.
pub struct JSXReconstructor;

impl JSXReconstructor {
    pub fn new() -> Self {
        Self
    }
}

// Run JSX reconstruction over a statement list. Idempotent (already-built
// JSXElements are left alone). Intended to run on the fully-assembled,
// whole-program IR, where props objects and children arrays are materialized, // the in-pipeline F10 pass runs too early (before object-literal reconstruction)
// to catch most calls.
pub fn reconstruct_jsx(mut stmts: Vec<crate::ir::Statement>) -> Vec<crate::ir::Statement> {
    use crate::ir::MutVisitor;
    JSXReconstructor::new().visit_statement_list(&mut stmts);
    stmts
}

impl MutVisitor for JSXReconstructor {
    fn visit_expression(&mut self, expr: &mut Expression) {
        // Walk children first to reconstruct nested JSX
        self.walk_expression(expr);

        // Try to match a React.createElement or _jsx call
        if let Expression::Call { callee, arguments } = expr {
            if is_jsx_call(callee) && !arguments.is_empty() {
                if let Some(jsx_node) = build_jsx_element(callee, arguments) {
                    *expr = jsx_node;
                }
            }
        }
    }
}

// The JSX factory name behind a call, if any, the property of a member callee
// (`React.createElement`, `jsxProd.jsx`) or a bare/imported function
// (`_jsx`, `jsx`, `createElement`). The leading `_` of minified runtime imports
// is stripped so `_jsxs` and `jsxs` are treated alike.
fn jsx_factory_name(callee: &Expression) -> Option<&str> {
    let raw = match callee {
        Expression::Member {
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } => p.as_str(),
        Expression::Value(Value::Variable(n)) => n.as_str(),
        _ => return None,
    };
    Some(raw.strip_prefix('_').unwrap_or(raw))
}

// classic `createElement(type, props, ...children)` vs the automatic runtime
// `jsx`/`jsxs`/`jsxDEV(type, props)` (children live in `props.children`).
fn is_jsx_call(callee: &Expression) -> bool {
    matches!(
        jsx_factory_name(callee),
        Some("createElement" | "jsx" | "jsxs" | "jsxDEV")
    )
}

fn is_modern_factory(callee: &Expression) -> bool {
    matches!(jsx_factory_name(callee), Some("jsx" | "jsxs" | "jsxDEV"))
}

// Constructs a JSXElement from the arguments of the factory call.
fn build_jsx_element(callee: &Expression, arguments: &[Expression]) -> Option<Expression> {
    // 1. Determine Tag Name
    let tag_name = match &arguments[0] {
        Expression::Value(Value::Constant(Constant::String(s))) => s.clone(),
        Expression::Value(Value::Variable(v)) => v.clone(),
        Expression::Member { object, property, .. } => {
            // For `<Component.SubComponent />`
            if let (Expression::Value(Value::Variable(obj_name)), PropertyKey::Ident(prop_name)) = (object.as_ref(), property) {
                format!("{obj_name}.{prop_name}")
            } else {
                return None; // Too complex
            }
        }
        _ => return None, // Unknown tag type
    };

    // `<>...</>`: the runtime Fragment marker renders as an empty tag.
    let tag_name = if tag_name == "Fragment"
        || tag_name == "_Fragment"
        || tag_name.ends_with(".Fragment")
    {
        String::new()
    } else {
        tag_name
    };

    let is_modern = is_modern_factory(callee);

    let mut jsx_attributes = Vec::new();
    let mut jsx_children = Vec::new();

    // Automatic runtime: `jsx(type, config, key)`, the key is the separate 3rd
    // argument, hoisted out of config into a `key` attribute.
    if is_modern && arguments.len() >= 3 {
        if !matches!(
            arguments[2],
            Expression::Value(Value::Constant(Constant::Undefined | Constant::Null))
        ) {
            jsx_attributes.push(("key".to_string(), arguments[2].clone()));
        }
    }

    // 2. Parse Attributes and Children
    if is_modern {
        // Modern JSX: jsx("div", { className: "foo", children: "bar" }), children
        // live inside the props object under the `children` key.
        if arguments.len() >= 2 {
            match &arguments[1] {
                Expression::Object { properties } => {
                    for prop in properties {
                        match &prop.key {
                            PropertyKey::Ident(k) | PropertyKey::String(k) => {
                                if k == "children" {
                                    if let Expression::Array { elements } = &prop.value {
                                        jsx_children.extend(elements.iter().flatten().cloned());
                                    } else {
                                        jsx_children.push(prop.value.clone());
                                    }
                                } else {
                                    jsx_attributes.push((k.clone(), prop.value.clone()));
                                }
                            }
                            _ => {} // computed keys: ignore
                        }
                    }
                }
                // Opaque props (a variable / spread): `<Tag {...props}/>`.
                Expression::Value(Value::Constant(Constant::Null | Constant::Undefined)) => {}
                other => jsx_attributes.push(("...".to_string(), other.clone())),
            }
        }
    } else {
        // Classic JSX: React.createElement("div", { className: "foo" }, "child1", "child2")
        if arguments.len() >= 2 {
            if let Expression::Object { properties } = &arguments[1] {
                for prop in properties {
                    match &prop.key {
                        PropertyKey::Ident(k) | PropertyKey::String(k) => {
                            jsx_attributes.push((k.clone(), prop.value.clone()));
                        }
                        _ => {}
                    }
                }
            } else if !matches!(arguments[1], Expression::Value(Value::Constant(Constant::Null | Constant::Undefined))) {
                // Spread attributes `<div {...props} />`
                // We model this as an attribute with empty key containing the spread expression for now, 
                // or just skip reconstruction if it's too complex.
                // Let's model spread using a special attribute key like "...spread" since our IR tuple is (String, Expression).
                if let Expression::Spread(_) = &arguments[1] {
                     jsx_attributes.push(("...".to_string(), arguments[1].clone()));
                } else {
                     jsx_attributes.push(("...".to_string(), Expression::Spread(Box::new(arguments[1].clone()))));
                }
            }
        }
        
        // Children start at index 2
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ObjectProperty;

    #[test]
    fn test_classic_jsx_element() {
        // React.createElement("div", { id: "main" }, "Text")
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

        let mut reconstructor = JSXReconstructor::new();
        reconstructor.visit_expression(&mut expr);

        if let Expression::JSXElement { tag, attributes, children } = expr {
            assert_eq!(tag, "div");
            assert_eq!(attributes.len(), 1);
            assert_eq!(attributes[0].0, "id");
            assert_eq!(children.len(), 1);
        } else {
            panic!("Expected JSXElement, got: {:?}", expr);
        }
    }

    // Helper: a modern-runtime call `factory(type, config[, key])`.
    fn modern_call(factory: &str, args: Vec<Expression>) -> Expression {
        Expression::call(Expression::Value(Value::Variable(factory.to_string())), args)
    }

    #[test]
    fn test_modern_jsx_member_factory_extracts_children() {
        // jsxProd.jsxs("ul", { className: "x", children: [a, b] })
        let mut expr = Expression::call(
            Expression::member(Expression::Value(Value::Variable("jsxProd".into())), "jsxs"),
            vec![
                Expression::constant(Constant::String("ul".into())),
                Expression::Object {
                    properties: vec![
                        ObjectProperty { key: PropertyKey::Ident("className".into()), value: Expression::constant(Constant::String("x".into())) },
                        ObjectProperty { key: PropertyKey::Ident("children".into()), value: Expression::Array { elements: vec![
                            Some(Expression::Value(Value::Variable("a".into()))),
                            Some(Expression::Value(Value::Variable("b".into()))),
                        ] } },
                    ],
                },
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement { tag, attributes, children } => {
                assert_eq!(tag, "ul");
                assert_eq!(attributes.len(), 1); // className only; children pulled out
                assert_eq!(attributes[0].0, "className");
                assert_eq!(children.len(), 2);
            }
            other => panic!("expected JSXElement, got {other:?}"),
        }
    }

    #[test]
    fn test_modern_key_third_arg() {
        // _jsx(Foo, { title: "x" }, k) -> key + title
        let mut expr = modern_call(
            "_jsx",
            vec![
                Expression::Value(Value::Variable("Foo".into())),
                Expression::Object { properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("title".into()),
                    value: Expression::constant(Constant::String("x".into())),
                }] },
                Expression::Value(Value::Variable("k".into())),
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement { tag, attributes, .. } => {
                assert_eq!(tag, "Foo");
                assert!(attributes.iter().any(|(k, _)| k == "key"));
                assert!(attributes.iter().any(|(k, _)| k == "title"));
            }
            other => panic!("expected JSXElement, got {other:?}"),
        }
    }

    #[test]
    fn test_fragment_empty_tag() {
        // jsxs(_Fragment, { children: [a] }) -> empty tag (renders <>...</>)
        let mut expr = modern_call(
            "jsxs",
            vec![
                Expression::Value(Value::Variable("_Fragment".into())),
                Expression::Object { properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("children".into()),
                    value: Expression::Array { elements: vec![Some(Expression::Value(Value::Variable("a".into())))] },
                }] },
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement { tag, children, .. } => {
                assert_eq!(tag, ""); // fragment
                assert_eq!(children.len(), 1);
            }
            other => panic!("expected JSXElement, got {other:?}"),
        }
    }
}
