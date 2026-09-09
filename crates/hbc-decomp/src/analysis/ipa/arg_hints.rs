// Derive a parameter-naming hint from a call-site argument, following local
// register/variable definitions.
//
// IPA runs early, before the pipeline inlines temporaries, so a call argument is
// often an intermediate register rather than the source expression. For a call
// like `loginWithToken(email, first.join(""))` the second argument reaches IPA as
// `r5` where `r5 = first.join("")`; without following that definition the hint
// `first` is lost and the parameter stays `argN`. Resolving through the local
// definitions recovers it.

use std::collections::HashMap;

use crate::ir::{Constant, Expression, PropertyKey, Value};

use super::inference::is_generic_name;
use super::resolution::{extract_name_from_callee, extract_object_name_from_method_call};

// Bounds the def-chasing so a cyclic definition (`r5 = r6; r6 = r5`) terminates.
const MAX_RESOLVE_DEPTH: u8 = 4;

pub(super) fn hint_from_arg(
    arg: &Expression,
    value_defs: &HashMap<String, Expression>,
) -> Option<String> {
    hint_inner(arg, value_defs, MAX_RESOLVE_DEPTH)
}

fn hint_inner(
    arg: &Expression,
    value_defs: &HashMap<String, Expression>,
    depth: u8,
) -> Option<String> {
    match arg {
        Expression::Value(Value::Variable(name)) => {
            if !is_generic_name(name) {
                return Some(name.clone());
            }
            resolve(name, value_defs, depth)
        }
        Expression::Value(Value::Register(r)) => resolve(&format!("r{r}"), value_defs, depth),
        Expression::Value(Value::Constant(Constant::String(s))) => {
            if !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_') {
                Some(s.clone())
            } else {
                None
            }
        }
        Expression::Member { object, property, .. } => {
            // `obj.prop` passed directly names the parameter after the property.
            if let PropertyKey::String(p) | PropertyKey::Ident(p) = property {
                if !is_generic_name(p) {
                    return Some(p.clone());
                }
            }
            object_hint(object, value_defs, depth)
        }
        // A call argument: a resolvable callee noun (`getEmail()` -> email) or the
        // receiver of a NON transforming method (`user.getName()` -> user). The
        // receiver of a transforming method (`x.join("")`, `x.map(...)`) is
        // deliberately NOT used: the argument is the transformed value, not `x`, so
        // naming the parameter after `x` would be a guess.
        Expression::Call { callee, .. } => extract_name_from_callee(callee)
            .or_else(|| extract_object_name_from_method_call(callee)),
        _ => None,
    }
}

// Resolve a register/variable name to its local definition and re-derive the hint.
fn resolve(key: &str, value_defs: &HashMap<String, Expression>, depth: u8) -> Option<String> {
    if depth == 0 {
        return None;
    }
    let def = value_defs.get(key)?;
    hint_inner(def, value_defs, depth - 1)
}

// The base object of a member access, resolving it when it is a register or a
// generic temporary (`tmp.email` where `tmp` was defined as `user`).
fn object_hint(
    object: &Expression,
    value_defs: &HashMap<String, Expression>,
    depth: u8,
) -> Option<String> {
    match object {
        Expression::Value(Value::Variable(name)) if !is_generic_name(name) => Some(name.clone()),
        Expression::Value(Value::Variable(name)) => resolve(name, value_defs, depth),
        Expression::Value(Value::Register(r)) => resolve(&format!("r{r}"), value_defs, depth),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Expression, PropertyKey, Value};

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Variable(n.to_string()))
    }

    fn method_call(obj: Expression, method: &str) -> Expression {
        Expression::Call {
            callee: Box::new(Expression::Member {
                object: Box::new(obj),
                property: PropertyKey::Ident(method.to_string()),
                optional: false,
            }),
            arguments: vec![],
        }
    }

    #[test]
    fn transform_method_receiver_is_not_named() {
        // `first.join("")` passed as an argument: the value is the joined string, not
        // the array `first`, so naming the parameter after the receiver is rejected.
        let defs = HashMap::new();
        let arg = method_call(var("first"), "join");
        assert_eq!(hint_from_arg(&arg, &defs), None);
    }

    #[test]
    fn transformed_value_yields_no_hint() {
        // r5 = first.join(""); f(r5) -> the parameter holds the joined string, NOT the
        // array `first`, so no name is guessed from the transform receiver.
        let mut defs = HashMap::new();
        defs.insert("r5".to_string(), method_call(var("first"), "join"));
        let arg = Expression::Value(Value::Register(5));
        assert_eq!(hint_from_arg(&arg, &defs), None);
    }

    #[test]
    fn resolves_generic_variable_to_source() {
        // tmp = user.email; f(tmp) -> "email".
        let mut defs = HashMap::new();
        defs.insert(
            "tmp".to_string(),
            Expression::Member {
                object: Box::new(var("user")),
                property: PropertyKey::Ident("email".to_string()),
                optional: false,
            },
        );
        let arg = var("tmp");
        assert_eq!(hint_from_arg(&arg, &defs), Some("email".to_string()));
    }

    #[test]
    fn generic_without_definition_yields_none() {
        let defs = HashMap::new();
        assert_eq!(hint_from_arg(&Expression::Value(Value::Register(9)), &defs), None);
    }
}
