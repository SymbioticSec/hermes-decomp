use super::{Codegen, DescriptorInfo, is_exports_like};

impl Codegen {
    // Try to extract an import statement from a value expression.
    // Returns `Some("import x from \"ModName\"")` if the pattern matches.
    pub(super) fn try_import_from_expr(&self, var_name: &str, value: &crate::ir::Expression) -> Option<String> {
        use crate::ir::Expression;

        // Pattern 1: require(N) or arg1(dependencyMap[N]) -> import var from "ModName"
        if let Some(mod_name) = self.resolve_require_module(value) {
            return Some(format!("import {var_name} from \"{mod_name}\";"));
        }

        // Pattern 2: require(N).prop or arg1(dependencyMap[N]).prop -> import { prop as var } from "ModName"
        if let Expression::Member { object, property, .. } = value {
            if let Some(mod_name) = self.resolve_require_module(object) {
                let prop = crate::ir::expr::display::format_key(property);
                if prop == var_name {
                    return Some(format!("import {{ {prop} }} from \"{mod_name}\";"));
                } else {
                    return Some(format!("import {{ {prop} as {var_name} }} from \"{mod_name}\";"));
                }
            }
        }

        // Pattern 3: wrapper(require(N)) -> import var from "ModName"
        // Handles _interopDefault, _interopRequireDefault, or any wrapper function around require
        if let Expression::Call { arguments, .. } = value {
            for arg in Self::effective_args(arguments) {
                if let Some(mod_name) = self.resolve_require_module(arg) {
                    return Some(format!("import {var_name} from \"{mod_name}\";"));
                }
            }
        }

        // Pattern 4: wrapper(require(N)).prop -> import { prop as var } from "ModName"
        if let Expression::Member { object, property, .. } = value {
            if let Expression::Call { arguments, .. } = object.as_ref() {
                for arg in Self::effective_args(arguments) {
                    if let Some(mod_name) = self.resolve_require_module(arg) {
                        let prop = crate::ir::expr::display::format_key(property);
                        if prop == var_name {
                            return Some(format!("import {{ {prop} }} from \"{mod_name}\";"));
                        } else {
                            return Some(format!("import {{ {prop} as {var_name} }} from \"{mod_name}\";"));
                        }
                    }
                }
            }
        }

        None
    }

    // Try to detect a side-effect import from a bare expression statement.
    // e.g. `require(N)` or `arg1(dependencyMap[N])` as a standalone statement.
    pub(super) fn try_side_effect_import(&self, expr: &crate::ir::Expression) -> Option<String> {
        if let Some(mod_name) = self.resolve_require_module(expr) {
            return Some(format!("import \"{mod_name}\";"));
        }
        None
    }

    // Try to detect Object.defineProperty(exports, "name", { get: ... }) export patterns.
    // Returns `Some("export const name = ...;")` if the pattern matches.
    // Try to convert defineProperty(exports, "name", descriptor) to ESM export.
    // Uses descriptor_vars map to resolve variable-held descriptors.
    pub(super) fn try_define_property_export_with_descriptors(
        &self,
        expr: &crate::ir::Expression,
        descriptor_vars: &std::collections::HashMap<String, DescriptorInfo>,
    ) -> Option<String> {
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

        // Determine the argument layout:
        // Layout A (3 args): defineProperty(exports, "name", descriptor)
        //   - when effective_args stripped the undefined this-arg
        // Layout B (4 args): defineProperty(this, exports, "name", descriptor)
        //   - when the this-arg is Object/globalThis.Object (not undefined, so not stripped)
        let (target_idx, name_idx, desc_idx) = if args.len() == 3 {
            (0, 1, 2)
        } else if args.len() >= 4 {
            // Check if args[0] is exports-like (Layout A with extra args) or not (Layout B)
            let first_str = self.generate_expr(&args[0]);
            if is_exports_like(&first_str) {
                (0, 1, 2)
            } else {
                (1, 2, 3)
            }
        } else {
            return None;
        };

        // Target arg should be exports-like
        let target_str = self.generate_expr(&args[target_idx]);
        if !is_exports_like(&target_str) {
            return None;
        }

        // Name arg is the property name
        let prop_name = match &args[name_idx] {
            Expression::Value(Value::Constant(Constant::String(s))) => s.clone(),
            _ => return None,
        };

        // Skip __esModule
        if prop_name == "__esModule" {
            return None;
        }

        // Try to extract the exported value from the descriptor
        let descriptor = &args[desc_idx];
        let exported_value = self.resolve_descriptor_value(descriptor, descriptor_vars);

        if prop_name == "default" {
            if let Some(val) = exported_value {
                Some(format!("export default {val};"))
            } else {
                Some("export default undefined;".to_string())
            }
        } else if let Some(val) = exported_value {
            // Avoid `export const X = X;` -- use `export { X }` instead
            if val == prop_name {
                Some(format!("export {{ {prop_name} }};"))
            } else {
                Some(format!("export const {prop_name} = {val};"))
            }
        } else {
            Some(format!("export const {prop_name} = undefined;"))
        }
    }
}
