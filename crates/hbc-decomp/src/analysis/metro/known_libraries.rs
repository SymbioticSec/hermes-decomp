// Recognise a handful of libraries by their complete export surface.
//
// A library module carries no source path and no default-export function
// name: React is `exports.createElement = …` forty times over, react-native's
// index is a getter per component. What identifies such a module is the set
// of names it exports, and only when every distinctive name of the library is
// present. A partial match names nothing: the export map of a module can be
// partial, and a module exporting `Component` alone is not React.

use std::collections::BTreeSet;

use crate::analysis::metro::registry::FactoryRoles;
use crate::ir::{AssignTarget, Binding, Constant, Expression, PropertyKey, Statement, Value};

// (specifier, every one of these keys must be exported)
const KNOWN: &[(&str, &[&str])] = &[
    (
        "react",
        &[
            "Component",
            "Fragment",
            "createContext",
            "createElement",
            "forwardRef",
            "isValidElement",
            "memo",
            "useEffect",
            "useRef",
            "useState",
        ],
    ),
    (
        "react-native",
        &[
            "ActivityIndicator",
            "Button",
            "Image",
            "NativeModules",
            "Platform",
            "ScrollView",
            "StyleSheet",
            "Text",
            "TextInput",
            "View",
        ],
    ),
    ("AssetRegistry", &["getAssetByID", "registerAsset"]),
];

// The library `stmts` is the factory of, if its export surface is complete.
pub(crate) fn recognize(stmts: &[Statement]) -> Option<&'static str> {
    let keys = export_key_set(stmts);
    if keys.is_empty() {
        return None;
    }
    KNOWN
        .iter()
        .find(|(_, required)| required.iter().all(|k| keys.contains(*k)))
        .map(|(name, _)| *name)
}

// Every export key of a factory body: `exports.K = …`,
// `Object.defineProperty(exports, "K", …)`, the keys of the object the
// module exports as default, and `defineProperty` calls on that object.
pub(crate) fn export_key_set(stmts: &[Statement]) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut default_objects: BTreeSet<String> = BTreeSet::new();
    let mut object_keys: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, .. } => {
                if let Expression::Object { properties } = value {
                    object_keys.insert(name.clone(), property_names(properties));
                }
            }
            Statement::Assign { target, value } => {
                match target {
                    AssignTarget::Binding(Binding::Variable(name)) => {
                        if let Expression::Object { properties } = value {
                            object_keys.insert(name.clone(), property_names(properties));
                        }
                    }
                    AssignTarget::Member { object, property } => {
                        if is_exports_object(object) && property != "__esModule" {
                            if property == "default" || is_module_object(object) {
                                match value {
                                    Expression::Value(Value::Binding(Binding::Variable(v))) => {
                                        default_objects.insert(v.clone());
                                    }
                                    Expression::Object { properties } => {
                                        keys.extend(property_names(properties));
                                    }
                                    _ => {}
                                }
                            } else {
                                keys.insert(property.clone());
                            }
                        }
                    }
                    _ => {}
                }
                if let Some((obj, key)) = define_property_call(value) {
                    record_define(obj, key, &mut keys, &mut object_keys);
                }
            }
            Statement::Expr(e) => {
                if let Some((obj, key)) = define_property_call(e) {
                    record_define(obj, key, &mut keys, &mut object_keys);
                }
            }
            _ => {}
        }
    }
    for obj in default_objects {
        if let Some(names) = object_keys.get(&obj) {
            keys.extend(names.iter().cloned());
        }
    }
    keys
}

fn record_define(
    obj: &Expression,
    key: String,
    keys: &mut BTreeSet<String>,
    object_keys: &mut std::collections::HashMap<String, Vec<String>>,
) {
    if is_exports_object(obj) {
        if key != "__esModule" {
            keys.insert(key);
        }
    } else if let Expression::Value(Value::Binding(Binding::Variable(v))) = obj {
        object_keys.entry(v.clone()).or_default().push(key);
    }
}

fn property_names(properties: &[crate::ir::ObjectProperty]) -> Vec<String> {
    properties
        .iter()
        .filter_map(|p| match &p.key {
            PropertyKey::Ident(s) | PropertyKey::String(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

// `exports`, `module.exports`, or the factory parameter holding either.
fn is_exports_object(e: &Expression) -> bool {
    match e {
        Expression::Value(Value::Binding(Binding::Variable(n))) => {
            FactoryRoles::matches_exports_name(n) || FactoryRoles::matches_module_name(n)
        }
        Expression::Value(Value::Parameter(idx)) => {
            FactoryRoles::is_exports_idx(*idx) || FactoryRoles::is_module_idx(*idx)
        }
        Expression::Member {
            object,
            property: PropertyKey::Ident(p),
            ..
        } => p == "exports" && is_module_object(object),
        _ => false,
    }
}

fn is_module_object(e: &Expression) -> bool {
    match e {
        Expression::Value(Value::Binding(Binding::Variable(n))) => {
            FactoryRoles::matches_module_name(n)
        }
        Expression::Value(Value::Parameter(idx)) => FactoryRoles::is_module_idx(*idx),
        _ => false,
    }
}

// `Object.defineProperty(obj, "key", …)`: the object and the key. Hermes
// may pass the receiver `Object` as a first argument, so the string key is
// looked for after the object argument.
fn define_property_call(e: &Expression) -> Option<(&Expression, String)> {
    let Expression::Call { callee, arguments } = e else {
        return None;
    };
    let is_define = match callee.as_ref() {
        Expression::Member {
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } => p == "defineProperty",
        _ => false,
    };
    if !is_define {
        return None;
    }
    for (i, arg) in arguments.iter().enumerate() {
        if let Expression::Value(Value::Constant(Constant::String(key))) = arg {
            if i >= 1 {
                return Some((&arguments[i - 1], key.clone()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export_assign(key: &str) -> Statement {
        Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Binding(Binding::Variable("exports".into()))),
                property: key.into(),
            },
            value: Expression::constant(Constant::Integer(1)),
        }
    }

    #[test]
    fn react_is_recognised_only_with_its_whole_surface() {
        let all: Vec<Statement> = KNOWN[0].1.iter().map(|k| export_assign(k)).collect();
        assert_eq!(recognize(&all), Some("react"));
        let partial: Vec<Statement> = KNOWN[0].1[..3].iter().map(|k| export_assign(k)).collect();
        assert_eq!(recognize(&partial), None);
    }

    #[test]
    fn getters_on_the_default_object_count_as_exports() {
        // let obj = {}; Object.defineProperty(obj, "View", …); module.exports = obj
        let mut stmts = vec![Statement::Let {
            name: "obj".into(),
            value: Expression::Object { properties: vec![] },
            kind: crate::ir::VarKind::Let,
        }];
        for k in KNOWN[1].1 {
            stmts.push(Statement::Expr(Expression::Call {
                callee: Box::new(Expression::member(
                    Expression::Value(Value::Binding(Binding::Variable("Object".into()))),
                    "defineProperty",
                )),
                arguments: vec![
                    Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
                    Expression::constant(Constant::String((*k).into())),
                    Expression::Object { properties: vec![] },
                ],
            }));
        }
        stmts.push(Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Binding(Binding::Variable("module".into()))),
                property: "exports".into(),
            },
            value: Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
        });
        assert_eq!(recognize(&stmts), Some("react-native"));
    }
}
