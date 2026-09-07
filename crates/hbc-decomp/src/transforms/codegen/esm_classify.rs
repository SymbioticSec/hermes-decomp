use super::{Codegen, EsmClassification, is_exports_like, is_module_like, sanitize_import_name};
use crate::ir::Statement;

impl Codegen {
    // Classify a single IR statement for ESM output.
    pub(super) fn classify_esm_stmt(
        &self,
        stmt: &Statement,
        rebound: &std::collections::HashSet<String>,
    ) -> EsmClassification {
        use crate::ir::{Expression, Value, Constant, AssignTarget};

        match stmt {
            // Skip: return undefined / return;
            Statement::Return(None) => EsmClassification::Skip,
            Statement::Return(Some(expr)) => {
                if matches!(expr, Expression::Value(Value::Constant(Constant::Undefined))) {
                    EsmClassification::Skip
                } else {
                    EsmClassification::Body
                }
            }

            // Let bindings: let x = require(N) or let x = _interopDefault(require(N))
            Statement::Let { name, value, .. } => {
                if let Some(cls) = self.import_classification(name, value, rebound) {
                    return cls;
                }
                // Skip interop wrapper function definitions
                if Self::is_interop_wrapper_def(value) {
                    return EsmClassification::Skip;
                }
                // Skip global object aliases (const Object = globalThis.Object)
                if Self::is_global_alias_def(name, value) {
                    return EsmClassification::Skip;
                }
                // Check for __esModule boilerplate
                if self.is_esmodule_boilerplate_expr(value) {
                    return EsmClassification::Skip;
                }
                EsmClassification::Body
            }

            // Assign: target = value
            Statement::Assign { target, value } => {
                // Import: variable = require(N) or variable = _interopDefault(require(N))
                if let AssignTarget::Variable(name) = target {
                    if let Some(cls) = self.import_classification(name, value, rebound) {
                        return cls;
                    }
                }

                // Skip interop wrapper function definitions
                if Self::is_interop_wrapper_def(value) {
                    return EsmClassification::Skip;
                }

                // Export: exports.name = value (or arg3.name = value, p3.name = value)
                if let AssignTarget::Member { object, property } = target {
                    let obj_str = self.generate_expr(object);
                    if is_exports_like(&obj_str) {
                        // Skip `exports.X = undefined`, Babel initialization noise
                        // The real export value is assigned later or via defineProperty
                        if matches!(value, Expression::Value(Value::Constant(Constant::Undefined))) {
                            return EsmClassification::Skip;
                        }
                        if property == "default" {
                            // Try to resolve re-exports: export default require(N) → export { default } from "mod"
                            if let Some(mod_name) = self.resolve_require_module(value) {
                                return EsmClassification::Export(
                                    format!("export {{ default }} from \"{mod_name}\";")
                                );
                            }
                            // Try require(N).prop → export { prop as default } from "mod"
                            if let Expression::Member { object, property: prop_key, .. } = value {
                                if let Some(mod_name) = self.resolve_require_module(object) {
                                    let prop = crate::ir::expr::display::format_key(prop_key);
                                    return EsmClassification::Export(
                                        format!("export {{ {prop} as default }} from \"{mod_name}\";")
                                    );
                                }
                            }
                            // Try require(N)(args) → import + export default call
                            if let Expression::Call { callee, arguments } = value {
                                if let Some(mod_name) = self.resolve_require_module(callee) {
                                    let import_name = sanitize_import_name(&mod_name);
                                    if !import_name.is_empty() {
                                        let call_str = self.format_call(&import_name, None, arguments, "");
                                        return EsmClassification::ImportAndExport(
                                            format!("import {import_name} from \"{mod_name}\";"),
                                            format!("export default {call_str};"),
                                        );
                                    }
                                }
                            }
                            return EsmClassification::Export(
                                format!("export default {};", self.generate_expr(value))
                            );
                        } else if property == "exports" {
                            // exports.exports = X is the CJS module.exports = X pattern
                            // via the exports parameter alias, treat as default export
                            return EsmClassification::Export(
                                format!("export default {};", self.generate_expr(value))
                            );
                        } else if property == "__esModule" {
                            return EsmClassification::Skip;
                        } else {
                            let val_str = self.generate_expr(value);
                            // Avoid `export const X = X;`, use `export { X }` instead
                            if val_str == *property {
                                return EsmClassification::Export(
                                    format!("export {{ {property} }};")
                                );
                            }
                            // `export const name = function name()` → `export function name()`
                            // Only for non-arrow functions, arrow functions don't use `function` keyword
                            if let crate::ir::Expression::Function { name: Some(fn_name), is_arrow: false, .. } = value {
                                if fn_name == property {
                                    return EsmClassification::Export(
                                        format!("export {val_str}")
                                    );
                                }
                            }
                            return EsmClassification::Export(
                                format!("export const {property} = {val_str};")
                            );
                        }
                    }
                    // module.exports = value (or arg2.exports, p2.exports)
                    if is_module_like(&obj_str) && property == "exports" {
                        // Try to resolve re-exports
                        if let Some(mod_name) = self.resolve_require_module(value) {
                            return EsmClassification::Export(
                                format!("export {{ default }} from \"{mod_name}\";")
                            );
                        }
                        // Try require(N).prop → export { prop as default } from "mod"
                        if let Expression::Member { object: inner_obj, property: prop_key, .. } = value {
                            if let Some(mod_name) = self.resolve_require_module(inner_obj) {
                                let prop = crate::ir::expr::display::format_key(prop_key);
                                return EsmClassification::Export(
                                    format!("export {{ {prop} as default }} from \"{mod_name}\";")
                                );
                            }
                        }
                        // Try require(N)(args) → import + export default call
                        if let Expression::Call { callee, arguments } = value {
                            if let Some(mod_name) = self.resolve_require_module(callee) {
                                let import_name = sanitize_import_name(&mod_name);
                                if !import_name.is_empty() {
                                    let call_str = self.format_call(&import_name, None, arguments, "");
                                    return EsmClassification::ImportAndExport(
                                        format!("import {import_name} from \"{mod_name}\";"),
                                        format!("export default {call_str};"),
                                    );
                                }
                            }
                        }
                        return EsmClassification::Export(
                            format!("export default {};", self.generate_expr(value))
                        );
                    }
                    // module.exports.__esModule = true (nested member target)
                    if obj_str.ends_with(".exports") && property == "__esModule" {
                        return EsmClassification::Skip;
                    }
                    // module.exports.default = module.exports (self-referential boilerplate)
                    if obj_str.ends_with(".exports") && property == "default" {
                        let val_str = self.generate_expr(value);
                        if val_str.ends_with(".exports") || val_str == "module.exports" {
                            return EsmClassification::Skip;
                        }
                    }
                }

                // __esModule boilerplate
                if self.is_esmodule_boilerplate_expr(value) {
                    return EsmClassification::Skip;
                }

                EsmClassification::Body
            }

            // Expr statements: defineProperty exports, side-effect imports, or __esModule boilerplate
            Statement::Expr(expr) => {
                if self.is_esmodule_boilerplate_expr(expr) {
                    EsmClassification::Skip
                } else if let Some(imp) = self.try_side_effect_import(expr) {
                    EsmClassification::Import(imp)
                } else {
                    EsmClassification::Body
                }
            }

            _ => EsmClassification::Body,
        }
    }
}

impl Codegen {
    // An import for `name = value`, kept valid when the module writes to `name`
    // again. An ESM import binding cannot be assigned to, and Hermes reuses one
    // register for both halves of the interop dance, so `invariant = require(31)`
    // followed by `invariant = interopResult` used to render as a write to the
    // import. Bind the import under its own name and alias it locally instead, so
    // the later write lands on a `let` the module actually owns.
    pub(super) fn import_classification(
        &self,
        name: &str,
        value: &crate::ir::Expression,
        rebound: &std::collections::HashSet<String>,
    ) -> Option<EsmClassification> {
        if !rebound.contains(name) {
            return self.try_import_from_expr(name, value).map(EsmClassification::Import);
        }
        let local = crate::util::sanitize_identifier(name);
        if local.is_empty() {
            return None;
        }
        let alias = format!("{local}_mod");
        let imp = self.try_import_from_expr(&alias, value)?;
        Some(EsmClassification::ImportAndBody(imp, format!("let {local} = {alias};\n")))
    }
}

#[cfg(test)]
mod rebound_import_tests {
    use super::super::{Codegen, CodegenOptions, EsmClassification};
    use crate::ir::{Constant, Expression, Value};
    use std::collections::HashSet;

    fn require_call(id: i32) -> Expression {
        Expression::Call {
            callee: Box::new(Expression::Value(Value::Variable("require".into()))),
            arguments: vec![Expression::Value(Value::Constant(Constant::Integer(id)))],
        }
    }

    #[test]
    fn a_name_written_once_stays_a_plain_import() {
        let cg = Codegen::new(CodegenOptions::default());
        let rebound = HashSet::new();
        match cg.import_classification("invariant", &require_call(31), &rebound) {
            Some(EsmClassification::Import(line)) => {
                assert!(line.starts_with("import invariant from "), "{line}");
            }
            _ => panic!("expected a plain import"),
        }
    }

    #[test]
    fn a_rebound_name_gets_its_own_binding_and_a_local_alias() {
        // Hermes reuses one register for both halves of the interop dance, so the
        // module writes to the name again after the load. An import binding cannot
        // be assigned to, so the import takes a private name and the module keeps a
        // local it owns.
        let cg = Codegen::new(CodegenOptions::default());
        let mut rebound = HashSet::new();
        rebound.insert("invariant".to_string());
        match cg.import_classification("invariant", &require_call(31), &rebound) {
            Some(EsmClassification::ImportAndBody(imp, body)) => {
                assert!(imp.starts_with("import invariant_mod from "), "{imp}");
                assert_eq!(body, "let invariant = invariant_mod;\n");
            }
            _ => panic!("expected an import plus a local alias"),
        }
    }
}
