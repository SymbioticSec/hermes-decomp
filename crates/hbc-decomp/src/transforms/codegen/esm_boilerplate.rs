use super::Codegen;
use crate::ir::Statement;

impl Codegen {
    // Check if a statement is an interopDefault check (if(!X) {... default ...} else {... __esModule ...})
    pub(super) fn is_interop_default_check(&self, stmt: &Statement) -> bool {
        if let Statement::If { then_body, else_body, .. } = stmt {
            // Check if either branch references __esModule or .default
            let debug_str = format!("{stmt:?}");
            if debug_str.contains("__esModule") || debug_str.contains("esModule") {
                return true;
            }
            // Check if it's an interop wrapper: if(!X) { obj.default = X } else { X.__esModule }
            if then_body.len() <= 4 && else_body.len() <= 4 {
                let has_default = debug_str.contains("\"default\"");
                if has_default {
                    return true;
                }
            }
        }
        false
    }

    // Check if a statement is __esModule boilerplate
    pub(super) fn is_esmodule_boilerplate_stmt(&self, stmt: &Statement) -> bool {
        match stmt {
            Statement::Expr(expr) => self.is_esmodule_boilerplate_expr(expr),
            Statement::Assign { value, .. } => self.is_esmodule_boilerplate_expr(value),
            _ => false,
        }
    }

    // Skip the `undefined` this-argument that Hermes prepends to call arguments.
    pub(super) fn effective_args(arguments: &[crate::ir::Expression]) -> &[crate::ir::Expression] {
        if arguments.len() >= 2
            && matches!(&arguments[0], crate::ir::Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))) {
                return &arguments[1..];
            }
        arguments
    }

    // Try to resolve a require(N) or arg1(dependencyMap[N]) call to a module name.
    pub(super) fn resolve_require_module(&self, expr: &crate::ir::Expression) -> Option<String> {
        use crate::ir::{Expression, Value, Constant};

        let (callee, arguments) = match expr {
            Expression::Call { callee, arguments } => (callee, arguments),
            _ => return None,
        };

        // In ESM mode, accept ANY callee as a potential require function
        // (factory params may not be renamed from arg1/arg2/etc.)
        if !self.esm_mode {
            let callee_str = self.generate_expr(callee);
            let roles = crate::analysis::metro::registry::FactoryRoles::standard();
            if !roles.is_require_param(&callee_str) {
                return None;
            }
        }

        // Get effective args (skip undefined this-binding)
        let args = Self::effective_args(arguments);
        let id_arg = args.first()?;

        // Case 1: require(INTEGER_CONSTANT) — direct module ID
        if let Expression::Value(Value::Constant(Constant::Integer(id))) = id_arg {
            return self.lookup_module_name(*id as u32, true);
        }

        // Case 2: arg1(dependencyMap[N]) — array index into dependency map
        if let Some(idx) = Self::extract_array_index(id_arg) {
            return self.lookup_module_name(idx, false);
        }

        None
    }

    // Extract the integer index from a member expression like `dependencyMap[0]`.
    // Handles both PropertyKey::Index(i64) and PropertyKey::Computed(Integer(N)).
    pub(super) fn extract_array_index(expr: &crate::ir::Expression) -> Option<u32> {
        use crate::ir::{Expression, Value, Constant, PropertyKey};

        match expr {
            Expression::Member { property: PropertyKey::Index(idx), .. } => {
                Some(*idx as u32)
            }
            Expression::Member { property: PropertyKey::Computed(key), .. } => {
                if let Expression::Value(Value::Constant(Constant::Integer(idx))) = key.as_ref() {
                    Some(*idx as u32)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    // Look up a module name by ID. If `is_absolute_id` is true, treats id as a module ID.
    // If false, treats it as a dependency array index and looks up via dep_names.
    pub(super) fn lookup_module_name(&self, id: u32, is_absolute_id: bool) -> Option<String> {
        if !is_absolute_id {
            // Index-based lookup via dep_names
            if let Some(ref dep_map) = self.dep_names {
                if let Some(name) = dep_map.get(&id) {
                    return Some(name.clone());
                }
            }
            return None;
        }

        // Absolute module ID lookup
        // Try dep_names first (index-based, for ESM mode)
        if let Some(ref dep_map) = self.dep_names {
            if let Some(name) = dep_map.get(&id) {
                return Some(name.clone());
            }
        }

        // Fallback to import_map (module_id based)
        if let Some(ref imp_map) = self.import_map {
            if let Some(name) = imp_map.get(&id) {
                return Some(name.clone());
            }
        }

        // Last resort: generic name
        Some(format!("module_{id}"))
    }

    // Check if an expression is __esModule boilerplate that should be skipped.
    pub(super) fn is_esmodule_boilerplate_expr(&self, expr: &crate::ir::Expression) -> bool {
        use crate::ir::{Expression, Value, Constant};

        // Pattern 1: Object.defineProperty(exports, "__esModule", ...)
        if let Expression::Call { callee, arguments } = expr {
            let callee_str = self.generate_expr(callee);
            if callee_str.contains("defineProperty") && arguments.len() >= 2 {
                // Check if second arg is the string "__esModule"
                if let Expression::Value(Value::Constant(Constant::String(s))) = &arguments[1] {
                    if s == "__esModule" {
                        return true;
                    }
                }
            }
        }

        // Pattern 2: Direct member access like exports.__esModule = true
        if let Expression::Member { property: crate::ir::PropertyKey::Ident(prop), .. } = expr {
            if prop == "__esModule" {
                return true;
            }
        }

        // Pattern 3: Assignments like Object2 = globalThis.Object (shallow check only)
        // Only check top-level variable/value — never recurse into function bodies
        if let Expression::Value(Value::Variable(name)) = expr {
            if name == "Object2" || name == "globalThisObject" {
                return true;
            }
        }

        false
    }

    // Check if an expression is an interop wrapper function definition.
    // These are boilerplate in ESM mode since imports already handle the unwrapping.
    pub(super) fn is_interop_wrapper_def(expr: &crate::ir::Expression) -> bool {
        if let crate::ir::Expression::Function { name: Some(name), .. } = expr {
            let n = name.to_lowercase();
            return n.contains("interop");
        }
        false
    }

    // Check if a Let/Assign defines a global alias (e.g. `const Object = globalThis.Object`).
    pub(super) fn is_global_alias_def(name: &str, value: &crate::ir::Expression) -> bool {
        use crate::ir::{Expression, Value, PropertyKey};

        // Pattern 1: const X = globalThis.X (member access on globalThis/Global)
        if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } = value {
            let is_global_obj = match object.as_ref() {
                Expression::Value(Value::Global) => true,
                Expression::Value(Value::Variable(v)) if v == "globalThis" => true,
                _ => false,
            };
            if is_global_obj {
                // Accept: name starts with the property (Object, Object2, Symbol, etc.)
                let base = prop.as_str();
                if name.starts_with(base) && name[base.len()..].chars().all(|c| c.is_ascii_digit()) {
                    return true;
                }
            }
        }

        // Pattern 1b: const prototype = globalThis.X.prototype (chained member on built-in)
        // Also handles: const valueOf = globalThis.Boolean.prototype.valueOf
        if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } = value {
            if Self::is_global_builtin_chain(object) {
                // Accept: name matches or starts with the last property
                if name.starts_with(prop.as_str()) && name[prop.len()..].chars().all(|c| c.is_ascii_digit()) {
                    return true;
                }
            }
        }

        // Pattern 2: const X = globalThis (direct Global value)
        // Skip known aliases: window, self, global, and mis-named "Object" etc.
        match value {
            Expression::Value(Value::Global) => {
                return true; // Any alias of globalThis is redundant
            }
            Expression::Value(Value::Variable(v)) if v == "globalThis" => {
                return true;
            }
            _ => {}
        }

        false
    }

    // Check if an expression is a chain of member accesses starting from a global built-in.
    // e.g., globalThis.Object, globalThis.Boolean.prototype, globalThis.ReferenceError.prototype
    pub(super) fn is_global_builtin_chain(expr: &crate::ir::Expression) -> bool {
        use crate::ir::{Expression, Value, PropertyKey};
        match expr {
            // Base case: globalThis.BuiltIn
            Expression::Member { object, property: PropertyKey::Ident(name), .. } => {
                let is_global = match object.as_ref() {
                    Expression::Value(Value::Global) => true,
                    Expression::Value(Value::Variable(v)) if v == "globalThis" => true,
                    _ => false,
                };
                if is_global && crate::ir::expr::display::is_builtin_global(name) {
                    return true;
                }
                // Recursive case: (some chain).property
                Self::is_global_builtin_chain(object)
            }
            _ => false,
        }
    }
}
