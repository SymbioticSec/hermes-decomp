use crate::ir::{AssignTarget, Constant, Expression, MethodKind, PropertyKey, Value};

pub fn extract_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Value(Value::Variable(s)) => Some(s.clone()),
        Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
        _ => None,
    }
}

pub fn get_target_name(target: &AssignTarget) -> Option<String> {
    match target {
        AssignTarget::Variable(s) => Some(s.clone()),
        AssignTarget::Register(r) => Some(format!("r{r}")),
        _ => None,
    }
}

pub fn is_likely_class_name(name: &str) -> bool {
    // Class names typically start with uppercase and don't start with 'r' (register)
    name.chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
        && !name.starts_with('r')
}

pub fn is_create_class_call(callee: &Expression) -> bool {
    match callee {
        Expression::Value(Value::Variable(name)) => name == "_createClass" || name == "createClass",
        Expression::Member {
            property: PropertyKey::Ident(name),
            ..
        } => name == "_createClass" || name == "createClass",
        _ => false,
    }
}

pub fn is_set_prototype_of_call(callee: &Expression) -> bool {
    if let Expression::Member {
        object,
        property: PropertyKey::Ident(prop),
        ..
    } = callee
    {
        if prop == "setPrototypeOf" {
            if let Expression::Value(Value::Variable(obj_name)) = object.as_ref() {
                return obj_name == "Object";
            }
        }
    }
    // Also check for _setPrototypeOf helper
    if let Expression::Value(Value::Variable(name)) = callee {
        return name == "_setPrototypeOf" || name == "setPrototypeOf";
    }
    false
}

pub fn is_define_property_call(callee: &Expression) -> bool {
    if let Expression::Member {
        object,
        property: PropertyKey::Ident(prop),
        ..
    } = callee
    {
        if prop == "defineProperty" {
            if let Expression::Value(Value::Variable(obj_name)) = object.as_ref() {
                return obj_name == "Object";
            }
        }
    }
    false
}

// Extract methods from an array of { key, value } objects
pub fn extract_method_array(expr: &Expression) -> Option<Vec<(String, Expression, MethodKind)>> {
    if let Expression::Array { elements } = expr {
        let mut methods = Vec::new();
        for elem in elements.iter().flatten() {
            if let Expression::Object { properties } = elem {
                let mut key = None;
                let mut value = None;
                let mut kind = MethodKind::Method;

                for prop in properties {
                    match &prop.key {
                        PropertyKey::Ident(k) | PropertyKey::String(k) => {
                            if k == "key" {
                                if let Expression::Value(Value::Constant(Constant::String(s))) =
                                    &prop.value
                                {
                                    key = Some(s.clone());
                                } else if let Expression::Value(Value::Variable(s)) = &prop.value {
                                    key = Some(s.clone());
                                }
                            } else if k == "value" {
                                value = Some(prop.value.clone());
                            } else if k == "get" {
                                value = Some(prop.value.clone());
                                kind = MethodKind::Getter;
                            } else if k == "set" {
                                value = Some(prop.value.clone());
                                kind = MethodKind::Setter;
                            }
                        }
                        _ => {}
                    }
                }

                if let (Some(k), Some(v)) = (key, value) {
                    methods.push((k, v, kind));
                }
            }
        }
        if !methods.is_empty() {
            return Some(methods);
        }
    }
    None
}

// Extract inheritance info from setPrototypeOf(Foo.prototype, Bar.prototype)
pub fn extract_inheritance(target: &Expression, source: &Expression) -> Option<(String, String)> {
    // target should be Foo.prototype
    if let Expression::Member {
        object,
        property: PropertyKey::Ident(prop),
        ..
    } = target
    {
        if prop == "prototype" {
            if let Some(class_name) = extract_name(object) {
                // source should be Bar.prototype
                if let Expression::Member {
                    object: super_obj,
                    property: PropertyKey::Ident(super_prop),
                    ..
                } = source
                {
                    if super_prop == "prototype" {
                        if let Some(super_name) = extract_name(super_obj) {
                            return Some((class_name, super_name));
                        }
                    }
                }
            }
        }
    }
    None
}

// Extract getter/setter from defineProperty(Foo.prototype, "prop", { get: fn, set: fn })
pub fn extract_accessor_definition(
    target: &Expression,
    prop_name: &Expression,
    descriptor: &Expression,
) -> Option<(String, String, Option<Expression>, Option<Expression>)> {
    // target should be Foo.prototype
    let class_name = if let Expression::Member {
        object,
        property: PropertyKey::Ident(prop),
        ..
    } = target
    {
        if prop == "prototype" {
            extract_name(object)
        } else {
            None
        }
    } else {
        None
    }?;

    // prop_name should be a string
    let name = match prop_name {
        Expression::Value(Value::Constant(Constant::String(s))) => s.clone(),
        Expression::Value(Value::Variable(s)) => s.clone(),
        _ => return None,
    };

    // descriptor should be an object with get/set
    if let Expression::Object { properties } = descriptor {
        let mut getter = None;
        let mut setter = None;

        for prop in properties {
            match &prop.key {
                PropertyKey::Ident(k) | PropertyKey::String(k) => {
                    if k == "get" && matches!(&prop.value, Expression::Function { .. }) {
                        getter = Some(prop.value.clone());
                    } else if k == "set" && matches!(&prop.value, Expression::Function { .. }) {
                        setter = Some(prop.value.clone());
                    }
                }
                _ => {}
            }
        }

        if getter.is_some() || setter.is_some() {
            return Some((class_name, name, getter, setter));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_likely_class_name() {
        assert!(is_likely_class_name("MyClass"));
        assert!(is_likely_class_name("Foo"));
        assert!(!is_likely_class_name("myFunction"));
        assert!(!is_likely_class_name("r5")); // register
    }

    #[test]
    fn test_is_create_class_call() {
        let callee = Expression::Value(Value::Variable("_createClass".to_string()));
        assert!(is_create_class_call(&callee));

        let callee2 = Expression::Value(Value::Variable("somethingElse".to_string()));
        assert!(!is_create_class_call(&callee2));
    }
}
