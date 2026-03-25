use crate::ir::{Expression, MutVisitor, PropertyKey, Value, Constant};

// Reconstructs JSXElement nodes from React.createElement and _jsx / _jsxs calls.
pub struct JSXReconstructor;

impl JSXReconstructor {
    pub fn new() -> Self {
        Self
    }
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

// Checks if the callee expression represents a JSX factory function.
fn is_jsx_call(callee: &Expression) -> bool {
    match callee {
        // Match `React.createElement` or `_react.createElement`
        Expression::Member { property, .. } => {
            if let PropertyKey::Ident(prop_name) = property {
                if prop_name == "createElement" || prop_name == "jsx" || prop_name == "jsxs" || prop_name == "jsxDEV" {
                    return true;
                }
            }
            false
        }
        // Match modern JSX runtimes directly: `_jsx`, `_jsxs`, `_jsxDEV`, or direct `createElement` import
        Expression::Value(Value::Variable(name)) => {
            name == "_jsx" || name == "_jsxs" || name == "_jsxDEV" || name == "createElement"
        }
        _ => false,
    }
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

    let is_modern = match callee {
        Expression::Value(Value::Variable(name)) => name.starts_with("_jsx"),
        _ => false,
    };

    let mut jsx_attributes = Vec::new();
    let mut jsx_children = Vec::new();

    // 2. Parse Attributes and Children
    if is_modern {
        // Modern JSX: _jsx("div", { className: "foo", children: "bar" })
        if arguments.len() >= 2 {
            if let Expression::Object { properties } = &arguments[1] {
                for prop in properties {
                    match &prop.key {
                        PropertyKey::Ident(k) | PropertyKey::String(k) => {
                            if k == "children" {
                                // Extract children
                                if let Expression::Array { elements } = &prop.value {
                                    jsx_children.extend(elements.iter().flatten().cloned());
                                } else {
                                    jsx_children.push(prop.value.clone());
                                }
                            } else {
                                jsx_attributes.push((k.clone(), prop.value.clone()));
                            }
                        }
                        _ => {} // Ignore computed keys in JSX props for now
                    }
                }
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
}
