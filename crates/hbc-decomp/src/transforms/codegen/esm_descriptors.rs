use super::{Codegen, DescriptorInfo, EsmClassification, is_exports_like};
use crate::ir::Statement;

impl Codegen {
    // Resolve the exported value from a defineProperty descriptor.
    // Handles both inline descriptors and variable references.
    pub(super) fn resolve_descriptor_value(
        &self,
        descriptor: &crate::ir::Expression,
        descriptor_vars: &std::collections::HashMap<String, DescriptorInfo>,
    ) -> Option<String> {
        use crate::ir::{Expression, Value};

        // Case 1: Descriptor is a variable reference → lookup in descriptor_vars
        match descriptor {
            Expression::Value(Value::Variable(name)) => {
                if let Some(info) = descriptor_vars.get(name) {
                    return info.getter_return.clone().or(info.value_prop.clone());
                }
            }
            Expression::Value(Value::Register(r)) => {
                let key = format!("r{r}");
                if let Some(info) = descriptor_vars.get(&key) {
                    return info.getter_return.clone().or(info.value_prop.clone());
                }
            }
            _ => {}
        }

        // Case 2: Descriptor is an inline object
        if let Some(info) = self.extract_descriptor_info(descriptor) {
            return info.getter_return.or(info.value_prop);
        }

        None
    }

    // Extract descriptor info from an object expression (looking for get/value properties).
    pub(super) fn extract_descriptor_info(&self, expr: &crate::ir::Expression) -> Option<DescriptorInfo> {
        use crate::ir::{Expression, PropertyKey};

        let properties = match expr {
            Expression::Object { properties } => properties,
            _ => return None,
        };

        let mut getter_return = None;
        let mut value_prop = None;

        for prop in properties {
            let key_name = match &prop.key {
                PropertyKey::Ident(k) | PropertyKey::String(k) => k.as_str(),
                _ => continue,
            };

            match key_name {
                "get" => {
                    // Extract return value from getter function
                    if let Expression::Function { id, .. } = &prop.value {
                        // Try to get the rendered body from inline_bodies
                        {
                            if let Some(rendered) = self.inline_bodies.get(&id.0) {
                                // Try to extract the returned value from the rendered body
                                let trimmed = rendered.trim();
                                // Common: (arg0) => someValue  or  function get() { return someValue; }
                                if let Some(arrow_val) = trimmed.strip_prefix("(")
                                    .and_then(|s| s.find("=> "))
                                    .and_then(|_| trimmed.find("=> "))
                                    .map(|pos| trimmed[pos + 3..].to_string())
                                {
                                    getter_return = Some(arrow_val);
                                } else if let Some(ret_pos) = trimmed.find("return ") {
                                    let after_return = &trimmed[ret_pos + 7..];
                                    if let Some(semi_pos) = after_return.find(';') {
                                        getter_return = Some(after_return[..semi_pos].trim().to_string());
                                    } else {
                                        // May end with } or newline
                                        let val = after_return.trim().trim_end_matches('}').trim().trim_end_matches('\n').trim();
                                        if !val.is_empty() {
                                            getter_return = Some(val.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                "value" => {
                    value_prop = Some(self.generate_expr(&prop.value));
                }
                _ => {} // skip enumerable, configurable, writable, set
            }
        }

        if getter_return.is_some() || value_prop.is_some() {
            Some(DescriptorInfo { getter_return, value_prop })
        } else {
            None
        }
    }

    // Get the variable name of the descriptor argument in a defineProperty call.
    pub(super) fn get_define_property_descriptor_var(&self, expr: &crate::ir::Expression) -> Option<String> {
        use crate::ir::{Expression, Value, Constant};

        let (callee, arguments) = match expr {
            Expression::Call { callee, arguments } => (callee, arguments),
            _ => return None,
        };

        let callee_str = self.generate_expr(callee);
        if !callee_str.contains("defineProperty") {
            return None;
        }

        let args = Self::effective_args(arguments);

        // Same layout detection as try_define_property_export_with_descriptors
        let (target_idx, name_idx, desc_idx) = if args.len() == 3 {
            (0, 1, 2)
        } else if args.len() >= 4 {
            let first_str = self.generate_expr(&args[0]);
            if is_exports_like(&first_str) {
                (0, 1, 2)
            } else {
                (1, 2, 3)
            }
        } else {
            return None;
        };

        // Check target is exports-like
        let target_str = self.generate_expr(&args[target_idx]);
        if !is_exports_like(&target_str) {
            return None;
        }

        // Check prop name is not __esModule
        if let Expression::Value(Value::Constant(Constant::String(s))) = &args[name_idx] {
            if s == "__esModule" {
                return None;
            }
        }

        // Return the variable name of the descriptor
        match &args[desc_idx] {
            Expression::Value(Value::Variable(name)) => Some(name.clone()),
            Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
            _ => None, // inline descriptor, not a variable
        }
    }

    // Classify an ESM statement with descriptor context for defineProperty resolution.
    pub(super) fn classify_esm_stmt_with_descriptors(
        &self,
        stmt: &Statement,
        descriptor_vars: &std::collections::HashMap<String, DescriptorInfo>,
    ) -> EsmClassification {
        // First try the Expr handler with descriptors for defineProperty
        if let Statement::Expr(expr) = stmt {
            if self.is_esmodule_boilerplate_expr(expr) {
                return EsmClassification::Skip;
            }
            if let Some(exp) = self.try_define_property_export_with_descriptors(expr, descriptor_vars) {
                return EsmClassification::Export(exp);
            }
            if let Some(imp) = self.try_side_effect_import(expr) {
                return EsmClassification::Import(imp);
            }
        }

        // Fall back to standard classification for everything else
        self.classify_esm_stmt(stmt)
    }

    // Detect re-export modules: modules that just re-export everything from another module.
    // Pattern: import X; X = Object.keys(X); X.forEach(key => { defineProperty(exports, key, ...) })
    // Returns `Some("export * from \"module\";")` if the pattern matches.
    pub(super) fn detect_reexport_module(&self, statements: &[Statement]) -> Option<String> {
        use crate::ir::{Expression, Value, Constant, AssignTarget};

        // Filter out noise: return undefined, __esModule boilerplate, comments
        let meaningful: Vec<&Statement> = statements.iter().filter(|s| {
            !matches!(s, Statement::Return(None))
                && !matches!(s, Statement::Return(Some(Expression::Value(Value::Constant(Constant::Undefined)))))
                && !matches!(s, Statement::Comment(_))
                && !self.is_esmodule_boilerplate_stmt(s)
                && !self.is_interop_default_check(s)
        }).collect();

        // A re-export module typically has 3-5 meaningful statements
        if meaningful.len() < 2 || meaningful.len() > 8 {
            return None;
        }

        // Find the import statement and the module name
        let mut import_var = None;
        let mut module_name = None;

        for stmt in &meaningful {
            match stmt {
                Statement::Let { name, value, .. } => {
                    if let Some(mod_name) = self.resolve_require_module(value) {
                        import_var = Some(name.clone());
                        module_name = Some(mod_name);
                    }
                    if module_name.is_none() {
                        if let Expression::Call { arguments, .. } = value {
                            for arg in Self::effective_args(arguments) {
                                if let Some(mod_name) = self.resolve_require_module(arg) {
                                    import_var = Some(name.clone());
                                    module_name = Some(mod_name);
                                    break;
                                }
                            }
                        }
                    }
                }
                Statement::Assign { target: AssignTarget::Variable(name), value } => {
                    if let Some(mod_name) = self.resolve_require_module(value) {
                        import_var = Some(name.clone());
                        module_name = Some(mod_name);
                    }
                    if module_name.is_none() {
                        if let Expression::Call { arguments, .. } = value {
                            for arg in Self::effective_args(arguments) {
                                if let Some(mod_name) = self.resolve_require_module(arg) {
                                    import_var = Some(name.clone());
                                    module_name = Some(mod_name);
                                    break;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let _import_var = import_var?;
        let module_name = module_name?;

        // Check that there's an Object.keys assignment AND a forEach call
        let has_object_keys = meaningful.iter().any(|s| {
            let expr = match s {
                Statement::Assign { value: expr, .. } => expr,
                Statement::Let { value: expr, .. } => expr,
                Statement::Expr(expr) => expr,
                _ => return false,
            };
            self.is_object_keys_call(expr)
        });

        let has_foreach = meaningful.iter().any(|s| {
            let expr = match s {
                Statement::Assign { value: expr, .. } => expr,
                Statement::Let { value: expr, .. } => expr,
                Statement::Expr(expr) => expr,
                _ => return false,
            };
            self.is_foreach_call(expr)
        });

        if has_object_keys && has_foreach {
            return Some(format!("export * from \"{module_name}\";"));
        }

        None
    }

    // Check if an expression is Object.keys(X) or X.keys(X)
    pub(super) fn is_object_keys_call(&self, expr: &crate::ir::Expression) -> bool {
        use crate::ir::{Expression, PropertyKey};

        if let Expression::Call { callee, .. } = expr {
            if let Expression::Member { property: PropertyKey::Ident(name), .. } = callee.as_ref() {
                return name == "keys";
            }
        }
        false
    }

    // Extract the source variable from Object.keys(SRC) or SRC.keys(SRC)
    pub(super) fn extract_object_keys_source(&self, expr: &crate::ir::Expression) -> Option<String> {
        use crate::ir::{Expression, PropertyKey, Value};

        if let Expression::Call { callee, arguments } = expr {
            if let Expression::Member { property: PropertyKey::Ident(name), .. } = callee.as_ref() {
                if name == "keys" {
                    // The argument to keys() is the source object
                    let args = Self::effective_args(arguments);
                    if let Some(arg) = args.first() {
                        if let Expression::Value(Value::Variable(var_name)) = arg {
                            return Some(var_name.clone());
                        }
                    }
                }
            }
        }
        None
    }

    // Check if an expression is X.forEach(callback)
    pub(super) fn is_foreach_call(&self, expr: &crate::ir::Expression) -> bool {
        use crate::ir::{Expression, PropertyKey};

        if let Expression::Call { callee, .. } = expr {
            if let Expression::Member { property: PropertyKey::Ident(name), .. } = callee.as_ref() {
                return name == "forEach";
            }
        }
        false
    }

    // Check if an expression is VAR.forEach(callback) where VAR matches the given variable name.
    pub(super) fn is_foreach_on_var(&self, expr: &crate::ir::Expression, var_name: &str) -> bool {
        use crate::ir::{Expression, PropertyKey, Value};

        if let Expression::Call { callee, .. } = expr {
            if let Expression::Member { object, property: PropertyKey::Ident(method), .. } = callee.as_ref() {
                if method == "forEach" {
                    if let Expression::Value(Value::Variable(name)) = object.as_ref() {
                        return name == var_name;
                    }
                }
            }
        }
        false
    }
}
