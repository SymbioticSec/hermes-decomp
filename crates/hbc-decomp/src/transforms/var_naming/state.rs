use super::suggestions::{
    get_function_name, name_for_call, name_for_instance, name_for_property,
    name_for_qualified_call, sanitize_name,
};
use crate::ir::{Expression, PropertyKey};
use std::collections::{BTreeMap, HashSet};

pub struct VariableNamer {
    // Map from original name (or "r{reg}") to inferred name
    pub inferred_names: BTreeMap<String, String>,
    // Track which names are already used to avoid duplicates
    used_names: HashSet<String>,
    // Counter for disambiguation
    name_counters: BTreeMap<String, u32>,
}

impl VariableNamer {
    pub fn new() -> Self {
        Self {
            inferred_names: BTreeMap::new(),
            used_names: HashSet::new(),
            name_counters: BTreeMap::new(),
        }
    }

    pub fn suggest_name(&mut self, key: &str, base_name: &str) {
        if self.inferred_names.contains_key(key) {
            return;
        }

        let name = self.get_unique_name(base_name);
        self.inferred_names.insert(key.to_string(), name);
    }


    fn get_unique_name(&mut self, base: &str) -> String {
        // Clean the base name
        let base = sanitize_name(base);

        if !self.used_names.contains(&base) {
            self.used_names.insert(base.clone());
            return base;
        }

        // Add a number suffix
        let counter = self.name_counters.entry(base.clone()).or_insert(1);
        loop {
            let name = format!("{base}{counter}");
            *counter += 1;
            if !self.used_names.contains(&name) {
                self.used_names.insert(name.clone());
                return name;
            }
        }
    }

}

// Infer a name from an expression (free function, no state needed).
pub fn infer_name_from_expr(expr: &Expression) -> Option<String> {
    match expr {
        // fetch(url) → response
        Expression::Call { callee, .. } => {
            // Check for qualified patterns like StyleSheet.create(), JSON.parse(), etc.
            if let Expression::Member {
                object,
                property: PropertyKey::Ident(method),
                ..
            } = &**callee {
                if let Some(obj_name) = get_function_name(object) {
                    let qualified = format!("{obj_name}.{method}");
                    if let Some(name) = name_for_qualified_call(&qualified) {
                        return Some(name);
                    }
                }
            }
            if let Some(func_name) = get_function_name(callee) {
                return Some(name_for_call(&func_name));
            }
        }

        // arr[0] → "first"
        Expression::Member {
            property: PropertyKey::Index(0),
            ..
        } => {
            return Some("first".to_string());
        }

        // obj.property → prefer raw property name when it's a clean identifier
        Expression::Member {
            property: PropertyKey::Ident(prop),
            ..
        } => {
            // Use raw property name if it's a clean identifier (2-20 chars, alphanumeric)
            if prop.len() >= 2
                && prop.len() <= 20
                && prop.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                return Some(sanitize_name(prop));
            }
            return Some(name_for_property(prop));
        }

        // new Constructor() → instance name
        Expression::New { callee, .. } => {
            if let Some(class_name) = get_function_name(callee) {
                return Some(name_for_instance(&class_name));
            }
        }

        // Array literals → items, arr, list
        Expression::Array { .. } => {
            return Some("items".to_string());
        }

        // Object literals → obj, config, options
        Expression::Object { .. } => {
            return Some("obj".to_string());
        }

        // Template literal → text
        Expression::TemplateLiteral { .. } => {
            return Some("text".to_string());
        }

        // Binary operations → sum, diff, product, etc.
        Expression::Binary { op, .. } => {
            use crate::ir::BinaryOp;
            let name = match op {
                BinaryOp::Add => "sum",
                BinaryOp::Sub => "diff",
                BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => "result",
                _ => return None,
            };
            return Some(name.to_string());
        }

        // Await expression → result of the awaited call
        Expression::Await(inner) => {
            return infer_name_from_expr(inner);
        }

        _ => {}
    }

    None
}
