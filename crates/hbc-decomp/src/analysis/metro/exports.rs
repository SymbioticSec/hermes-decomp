use super::registry::MetroModule;
use crate::ir::{extract_function_id, AssignTarget, Expression, PropertyKey, Statement, Value};
use std::collections::{BTreeMap, HashMap};

// Analyzes the exports of a Metro module to find exported functions.
//
// Uses expression tracing to handle:
// 1. Direct assignments: `exports.foo = func`
// 2. Module exports: `module.exports = { ... }`
// 3. ESM Getters: `Object.defineProperty(exports, "foo", { get: () => internal_func })`
pub struct ExportAnalyzer;

impl ExportAnalyzer {
    pub fn analyze(module: &mut MetroModule, functions: &BTreeMap<u32, Vec<Statement>>) {
        let stmts = match functions.get(&module.function_id) {
            Some(s) => s,
            None => return,
        };

        // 1. Build a map of definitions in the factory scope
        // This is a simplified "Reaching Definitions" that assumes single definition or last definition dominates.
        // For accurate analysis we would need the full CFG/DataFlow, but for top-level factory exports,
        // a linear scan is usually sufficient as exports are defined sequentially.
        let mut definitions = HashMap::new();
        for stmt in stmts {
            if let Statement::Assign { target, value } = stmt {
                if let AssignTarget::Variable(name) = target {
                    definitions.insert(name.clone(), value);
                } else if let AssignTarget::Register(r) = target {
                    definitions.insert(format!("r{r}"), value);
                }
            }
        }

        let tracer = ExpressionTracer {
            definitions: &definitions,
            functions,
        };

        for stmt in stmts {
            analyze_stmt(stmt, module, &tracer);
        }
    }
}

struct ExpressionTracer<'a> {
    definitions: &'a HashMap<String, &'a Expression>,
    functions: &'a BTreeMap<u32, Vec<Statement>>,
}

impl<'a> ExpressionTracer<'a> {
    fn resolve(&self, expr: &'a Expression) -> &'a Expression {
        // Depth limit prevents infinite loops from definition cycles (e.g., r5 = r6; r6 = r5)
        self.resolve_bounded(expr, 8)
    }

    fn resolve_bounded(&self, expr: &'a Expression, depth: u8) -> &'a Expression {
        if depth == 0 {
            return expr;
        }
        match expr {
            Expression::Value(Value::Variable(name)) => {
                if let Some(def) = self.definitions.get(name) {
                    self.resolve_bounded(def, depth - 1)
                } else {
                    expr
                }
            }
            Expression::Value(Value::Register(r)) => {
                let key = format!("r{r}");
                if let Some(def) = self.definitions.get(&key) {
                    self.resolve_bounded(def, depth - 1)
                } else {
                    expr
                }
            }
            _ => expr,
        }
    }

    fn find_function_id(&self, expr: &'a Expression) -> Option<u32> {
        let resolved = self.resolve(expr);
        if let Expression::Function { id, .. } = resolved {
            Some(id.0)
        } else {
            None
        }
    }
}

fn analyze_stmt(stmt: &Statement, module: &mut MetroModule, tracer: &ExpressionTracer) {
    match stmt {
        Statement::Assign { target, value } => {
            // Pattern: Object.defineProperty(exports, "name", { get: ... })
            if let Expression::Call { callee, arguments } = value {
                if is_define_property(callee) && arguments.len() >= 3 {
                    // Start from exports object (Arg 0)
                    let arg0 = tracer.resolve(&arguments[0]);
                    if is_exports_object(arg0) {
                        // Property Name (Arg 1)
                        let prop_name = if let Expression::Value(Value::Constant(
                            crate::ir::Constant::String(s),
                        )) = tracer.resolve(&arguments[1])
                        {
                            Some(s.clone())
                        } else {
                            None
                        };

                        if let Some(name) = prop_name {
                            // Descriptor (Arg 2)
                            let descriptor = tracer.resolve(&arguments[2]);
                            if let Expression::Object { properties } = descriptor {
                                analyze_descriptor(properties, name, &mut module.exports, tracer);
                            }
                        }
                    }
                }
            }

            // Pattern: exports.name = value
            if let Some((base, prop)) = get_base_and_prop(target) {
                // Check if base is exports
                // We don't have easy tracing for *targets*, so we check name heuristics
                if is_exports_name(&base) {
                    if let Some(fid) = tracer.find_function_id(value) {
                        module.exports.insert(prop, fid);
                    }
                } else if is_module_name(&base) && prop == "exports" {
                    // Pattern: module.exports = { ... }
                    analyze_module_exports_assign(value, &mut module.exports, tracer);
                }
            }

            // Pattern: module.exports.name = value
            if let AssignTarget::Member { object, property } = target {
                if let Expression::Member {
                    object: inner_obj,
                    property: inner_prop,
                    ..
                } = object
                {
                    // Check for module.exports
                    if let Some(base) = get_var_name(inner_obj) {
                        if is_module_name(&base) {
                            if let PropertyKey::String(s) = inner_prop {
                                if s == "exports" {
                                    if let Some(fid) = tracer.find_function_id(value) {
                                        module.exports.insert(property.clone(), fid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Statement::Block(inner) => {
            for s in inner {
                analyze_stmt(s, module, tracer);
            }
        }
        _ => {}
    }
}

fn analyze_descriptor(
    properties: &[crate::ir::ObjectProperty],
    export_name: String,
    exports: &mut HashMap<String, u32>,
    tracer: &ExpressionTracer,
) {
    for prop in properties {
        if let PropertyKey::String(key) = &prop.key {
            if key == "get" {
                // Found getter. Needs to be a function.
                if let Some(func_id) = extract_function_id(&prop.value) {
                    if let Some(getter_stmts) = tracer.functions.get(&func_id) {
                        // Analyze getter body for return statement
                        if let Some(returned_expr) = find_returned_expression(getter_stmts) {
                            // Trace the returned expression in the *factory* scope?
                            // No, the getter usually returns a variable captured from factory scope.
                            // Or it returns a property of a variable.

                            // Step 1: Identify the variable returned by getter
                            if let Some(var_name) = get_var_name(&returned_expr) {
                                // Step 2: Resolve this variable in the *Factory* definitions
                                if let Some(def) = tracer.definitions.get(&var_name) {
                                    if let Some(fid) = tracer.find_function_id(def) {
                                        exports.insert(export_name.clone(), fid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn find_returned_expression(stmts: &[Statement]) -> Option<Expression> {
    for stmt in stmts {
        match stmt {
            Statement::Return(Some(expr)) => return Some(expr.clone()),
            Statement::Block(inner) => {
                if let Some(e) = find_returned_expression(inner) {
                    return Some(e);
                }
            }
            _ => {}
        }
    }
    None
}

fn analyze_module_exports_assign(
    value: &Expression,
    exports: &mut HashMap<String, u32>,
    tracer: &ExpressionTracer,
) {
    let resolved = tracer.resolve(value);
    if let Expression::Object { properties } = resolved {
        for prop in properties {
            if let PropertyKey::String(key) = &prop.key {
                if let Some(fid) = tracer.find_function_id(&prop.value) {
                    exports.insert(key.clone(), fid);
                }
            }
        }
    }
}

// -- Helpers --

fn is_define_property(expr: &Expression) -> bool {
    if let Expression::Member {
        property: PropertyKey::String(s),
        ..
    } = expr
    {
        s == "defineProperty"
    } else {
        false
    }
}

fn is_exports_object(expr: &Expression) -> bool {
    if let Some(name) = get_var_name(expr) {
        is_exports_name(&name)
    } else {
        false
    }
}

fn is_exports_name(name: &str) -> bool {
    super::registry::FactoryRoles::standard().is_exports_param(name)
}

fn is_module_name(name: &str) -> bool {
    super::registry::FactoryRoles::standard().is_module_param(name)
}

fn get_base_and_prop(target: &AssignTarget) -> Option<(String, String)> {
    if let AssignTarget::Member { object, property } = target {
        if let Some(base) = get_var_name(object) {
            return Some((base, property.clone()));
        }
    }
    None
}

fn get_var_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Value(Value::Variable(n)) => Some(n.clone()),
        Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
        Expression::Value(Value::Parameter(idx)) => Some(format!("p{idx}")), // Normalized param name?
        // Note: Earlier pipeline/propagation normalization might have changed "argN" to "pN" or kept "argN".
        // We should check both or assume standard format. Old code checked "p2", "module".
        // Let's support "arg" prefix too just in case.
        _ => None,
    }
}

// extract_func_id moved to crate::ir::extract_function_id

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::FunctionId;
    use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};
    use std::collections::{HashMap, BTreeMap};

    fn make_func_expr(id: u32) -> Expression {
        Expression::Function {
            id: FunctionId(id),
            name: None,
            is_arrow: false,
            is_async: false,
            is_generator: false,
        }
    }

    #[test]
    fn test_export_assignments() {
        let mut stmts = Vec::new();

        // exports.foo = func(10)
        stmts.push(Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Variable("exports".into())),
                property: "foo".into(),
            },
            value: make_func_expr(10),
        });

        // module.exports.bar = func(20) - This pattern is now handled by the general member assignment
        stmts.push(Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Member {
                    object: Box::new(Expression::Value(Value::Variable("module".into()))),
                    property: PropertyKey::String("exports".into()),
                    optional: false,
                },
                property: "bar".into(),
            },
            value: make_func_expr(20),
        });

        // module.exports = { baz: func(30) }
        stmts.push(Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Variable("module".into())),
                property: "exports".into(),
            },
            value: Expression::Object {
                properties: vec![crate::ir::ObjectProperty {
                    key: PropertyKey::String("baz".into()),
                    value: make_func_expr(30),
                }],
            },
        });

        let mut module = MetroModule {
            module_id: 1,
            function_id: 100,
            name: None,
            dependencies: vec![],
            exports: HashMap::new(),
            roles: crate::analysis::metro::registry::FactoryRoles::standard(),
        };

        let mut functions = BTreeMap::new();
        functions.insert(100, stmts);

        ExportAnalyzer::analyze(&mut module, &functions);

        assert_eq!(module.exports.get("foo"), Some(&10));
        assert_eq!(module.exports.get("bar"), Some(&20));
        assert_eq!(module.exports.get("baz"), Some(&30));
    }
}
