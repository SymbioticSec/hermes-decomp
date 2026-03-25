use super::registry::{MetroModule, MetroRegistry};
use crate::ir::{Expression, Statement, Value};
use std::collections::HashMap;

pub struct MetroDetector;

impl MetroDetector {
    // Metro uses `__d(undefined, factory, moduleId, dependencyMap)` to register modules.
    // The __d function is typically stored in a variable like `r0 = __r.__d`.
    pub fn analyze_statements(statements: &[Statement], registry: &mut MetroRegistry) {
        let mut reg_functions: HashMap<String, u32> = HashMap::new();
        let mut reg_arrays: HashMap<String, Vec<u32>> = HashMap::new();
        let mut reg_integers: HashMap<String, u32> = HashMap::new();

        for stmt in statements {
            Self::analyze_stmt(
                stmt,
                &mut reg_functions,
                &mut reg_arrays,
                &mut reg_integers,
                registry,
            );
        }
    }

    fn analyze_stmt(
        stmt: &Statement,
        reg_functions: &mut HashMap<String, u32>,
        reg_arrays: &mut HashMap<String, Vec<u32>>,
        reg_integers: &mut HashMap<String, u32>,
        registry: &mut MetroRegistry,
    ) {
        match stmt {
            Statement::Assign { target, value } => {
                let var_name = match target {
                    crate::ir::AssignTarget::Register(r) => Some(format!("r{r}")),
                    crate::ir::AssignTarget::Variable(n) => Some(n.clone()),
                    _ => None,
                };

                if let Some(name) = var_name {
                    if let Expression::Function { id, .. } = value {
                        reg_functions.insert(name.clone(), id.0);
                    }
                    if let Some(deps) = extract_array_of_integers(value, Some(reg_integers)) {
                        reg_arrays.insert(name.clone(), deps);
                    }
                    if let Some(n) = extract_integer(value) {
                        reg_integers.insert(name.clone(), n);
                    }
                }

                Self::check_for_d_call(value, reg_functions, reg_arrays, reg_integers, registry);
            }
            Statement::Expr(expr) => {
                Self::check_for_d_call(expr, reg_functions, reg_arrays, reg_integers, registry);
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                for s in then_body {
                    Self::analyze_stmt(s, reg_functions, reg_arrays, reg_integers, registry);
                }
                for s in else_body {
                    Self::analyze_stmt(s, reg_functions, reg_arrays, reg_integers, registry);
                }
            }
            Statement::While { body, .. } => {
                for s in body {
                    Self::analyze_stmt(s, reg_functions, reg_arrays, reg_integers, registry);
                }
            }
            Statement::For { body, .. } => {
                for s in body {
                    Self::analyze_stmt(s, reg_functions, reg_arrays, reg_integers, registry);
                }
            }
            Statement::Block(inner) => {
                for s in inner {
                    Self::analyze_stmt(s, reg_functions, reg_arrays, reg_integers, registry);
                }
            }
            _ => {}
        }
    }

    fn check_for_d_call(
        expr: &Expression,
        reg_functions: &HashMap<String, u32>,
        reg_arrays: &HashMap<String, Vec<u32>>,
        reg_integers: &HashMap<String, u32>,
        registry: &mut MetroRegistry,
    ) {
        if let Expression::Call {
            callee: _,
            arguments,
        } = expr
        {
            // Debug print
            if arguments.len() >= 3 {
                // println!("DEBUG: checking call with {} args", arguments.len());
            }

            // Metro format: __d(undefined, factory, moduleId, deps)
            // Arguments: [0] = undefined/context, [1] = function, [2] = moduleId, [3] = deps
            if arguments.len() == 4 {
                // Get function ID - either directly or via register/variable lookup
                let function_id = match &arguments[1] {
                    Expression::Function { id, .. } => Some(id.0),
                    Expression::Value(Value::Register(r)) => {
                        reg_functions.get(&format!("r{r}")).copied()
                    }
                    Expression::Value(Value::Variable(n)) => reg_functions.get(n).copied(),
                    _ => None,
                };

                // Get module ID - either directly or via register/variable lookup
                let module_id = match &arguments[2] {
                    Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) => {
                        Some(*n as u32)
                    }
                    Expression::Value(Value::Constant(crate::ir::Constant::Number(n))) => {
                        Some(*n as u32)
                    }
                    Expression::Value(Value::Register(r)) => {
                        reg_integers.get(&format!("r{r}")).copied()
                    }
                    Expression::Value(Value::Variable(n)) => reg_integers.get(n).copied(),
                    _ => None,
                };

                // Get dependencies - either directly or via register/variable lookup
                let dependencies = match &arguments[3] {
                    Expression::Array { .. } => {
                        extract_array_of_integers(&arguments[3], Some(reg_integers))
                            .unwrap_or_default()
                    }
                    Expression::Value(Value::Register(r)) => reg_arrays
                        .get(&format!("r{r}"))
                        .cloned()
                        .unwrap_or_default(),
                    Expression::Value(Value::Variable(n)) => {
                        reg_arrays.get(n).cloned().unwrap_or_default()
                    }
                    _ => Vec::new(),
                };

                // Register the module if we have function and module IDs
                if let (Some(func_id), Some(mod_id)) = (function_id, module_id) {
                    let inferred_name = match &arguments[1] {
                        Expression::Function { name: Some(n), .. } if is_meaningful_name(n) => {
                            Some(n.clone())
                        }
                        _ => None,
                    };

                    let module = MetroModule {
                        module_id: mod_id,
                        function_id: func_id,
                        name: inferred_name,
                        dependencies,
                        exports: HashMap::new(),
                        roles: crate::analysis::metro::registry::FactoryRoles::standard(),
                    };
                    registry.function_to_module.insert(func_id, mod_id);
                    registry.factories.insert(func_id, module.clone());
                    registry.modules.insert(mod_id, module);
                }
            }
        }
    }
}

fn extract_integer(expr: &Expression) -> Option<u32> {
    match expr {
        Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) => Some(*n as u32),
        Expression::Value(Value::Constant(crate::ir::Constant::Number(n))) => Some(*n as u32),
        _ => None,
    }
}

fn extract_array_of_integers(
    expr: &Expression,
    reg_integers: Option<&HashMap<String, u32>>,
) -> Option<Vec<u32>> {
    if let Expression::Array { elements } = expr {
        let values: Vec<u32> = elements
            .iter()
            .flatten()
            .filter_map(|e| {
                if let Some(n) = extract_integer(e) {
                    return Some(n);
                }
                // Try resolving register/variable
                if let Some(map) = reg_integers {
                    match e {
                        Expression::Value(Value::Register(r)) => {
                            let key = format!("r{r}");
                            let val = map.get(&key).copied();
                            val
                        }
                        Expression::Value(Value::Variable(n)) => map.get(n).copied(),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect();
        // Return explicit empty vector if array is empty but valid (though Metro Usually has deps)
        // But the original logic returned None if parsed values were empty.
        // If the array had elements but we failed to parse them, we get empty.
        // If array was empty [] -> we get empty.
        // Metro deps [] is valid.
        return Some(values);
    }
    None
}

pub(crate) fn is_meaningful_name(name: &str) -> bool {
    // Reject purely numeric names
    if name.chars().all(|c| c.is_ascii_digit()) { return false; }
    // Reject f1234 pattern (decompiler-generated function names)
    if name.starts_with('f') && name.len() > 1 && name[1..].chars().all(|c| c.is_ascii_digit()) { return false; }
    // Reject obviously generic names (shared core)
    if super::is_obviously_generic(name) { return false; }
    true
}
