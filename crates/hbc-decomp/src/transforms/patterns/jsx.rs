// Reconstructs JSXElement nodes from React.createElement and _jsx / _jsxs calls.
//
// 1. Fold `obj.prop = v` into the object literal.
// 2. Resolve a props variable one hop.
// 3. Lower the factory call to a JSXElement.

mod fold;
mod props;

use crate::ir::{
    Binding, Constant, Expression, MutVisitor, ObjectProperty, PropertyKey, Statement, Value,
};

pub fn reconstruct_jsx(mut stmts: Vec<Statement>) -> Vec<Statement> {
    stmts = fold::fold_prop_assignments(stmts);
    stmts = props::resolve_prop_object_vars(stmts);
    JSXReconstructor::new().visit_statement_list(&mut stmts);
    stmts
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
        Expression::Value(Value::Binding(Binding::Variable(n))) => n.as_str(),
        _ => return None,
    };
    // Strip leading underscores and common runtime prefixes.
    let stripped = raw.strip_prefix('_').unwrap_or(raw);
    Some(stripped)
}

fn is_jsx_call(callee: &Expression) -> bool {
    matches!(
        jsx_factory_name(callee),
        Some("createElement" | "jsx" | "jsxs" | "jsxDEV" | "jsxsDEV" | "jsxDev" | "jsxsDev")
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
        Expression::Value(Value::Binding(Binding::Variable(v))) => v.clone(),
        Expression::Member {
            object, property, ..
        } => {
            if let (
                Expression::Value(Value::Binding(Binding::Variable(obj_name))),
                PropertyKey::Ident(prop_name),
            ) = (object.as_ref(), property)
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
#[path = "jsx/tests.rs"]
mod tests;
