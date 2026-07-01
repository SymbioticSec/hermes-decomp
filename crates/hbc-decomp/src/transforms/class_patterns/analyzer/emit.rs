use crate::ir::{Statement, Expression, ClassMethod, MethodKind, PropertyKey, Value};
use crate::transforms::class_patterns::builder::ClassBuilder;
use super::ClassAnalyzer;

// Recognize a Hermes-synthesized *default* constructor body, which the source
// did not write and which should not be emitted (the class then has no explicit
// `constructor`). Two shapes (HBC >=97):
//   * Base default:    `return Object.create(new.target.prototype);`
//     (GetNewTarget; .prototype; NewObjectWithParent; Ret)
//   * Derived default: forwards arguments to the super constructor via
//     `HermesBuiltin.applyArguments(...)` (CreateThisForSuper + applyArguments).
fn is_synthesized_default_ctor(body: &Option<Vec<Statement>>) -> bool {
    let Some(stmts) = body else { return false };
    // Find the (single) return value, ignoring intermediate temp assignments.
    let ret_expr = stmts.iter().find_map(|s| match s {
        Statement::Return(Some(e)) => Some(e),
        _ => None,
    });
    let Some(expr) = ret_expr else { return false };
    is_object_create_new_target(expr) || calls_apply_arguments(expr)
}

// `Object.create(new.target.prototype)` — the synthesized base constructor.
fn is_object_create_new_target(expr: &Expression) -> bool {
    let Expression::Call { callee, arguments } = expr else { return false };
    let is_create = matches!(callee.as_ref(),
        Expression::Member { property: PropertyKey::Ident(p) | PropertyKey::String(p), .. } if p == "create");
    if !is_create {
        return false;
    }
    // Single argument: `new.target.prototype`.
    matches!(arguments.first(), Some(Expression::Member {
        object, property: PropertyKey::Ident(p) | PropertyKey::String(p), ..
    }) if p == "prototype" && matches!(object.as_ref(), Expression::Value(Value::NewTarget)))
}

// Any call to `…applyArguments` (the derived default ctor's super-forwarding).
fn calls_apply_arguments(expr: &Expression) -> bool {
    if let Expression::Call { callee, .. } = expr {
        return matches!(callee.as_ref(),
            Expression::Member { property: PropertyKey::Ident(p) | PropertyKey::String(p), .. }
                if p == "applyArguments");
    }
    false
}

// The display name of a constructor closure (`function Animal() {}` -> "Animal").
fn function_name(expr: &Expression) -> Option<String> {
    if let Expression::Function { name: Some(n), .. } = expr {
        if !n.is_empty() {
            return Some(n.clone());
        }
    }
    None
}

// Visitor that flags any use of `super` (Value::Super) in an IR subtree.
struct SuperFinder {
    found: bool,
}

impl<'a> crate::ir::Visitor<'a> for SuperFinder {
    fn visit_expression(&mut self, expr: &'a Expression) {
        if matches!(expr, Expression::Value(Value::Super)) {
            self.found = true;
        }
        self.walk_expression(expr);
    }
}

// A real source class name, not a register placeholder like "r10000".
pub(super) fn is_real_class_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // Reject register placeholders like "r10000".
    let is_register = name.starts_with('r') && name[1..].chars().all(|c| c.is_ascii_digit());
    !is_register
}

impl<'a> ClassAnalyzer<'a> {
    pub(super) fn register_constructor(&mut self, name: &str, value: Expression, idx: usize) {
        let body = self.fetch_body(&value);
        let params = self.fetch_params(&value);
        // The class display name comes from the constructor *function* name (e.g.
        // `r10000 = function Animal() {}`), not the register the closure lands in.
        let display = function_name(&value);
        let builder = self.classes.entry(name.to_string()).or_insert_with(|| ClassBuilder {
            name: name.to_string(),
            ..Default::default()
        });
        if let Some(display) = display {
            if is_real_class_name(&display) {
                builder.name = display;
            }
        }
        builder.constructor = Some(value);
        builder.constructor_body = body;
        builder.constructor_params = params;
        self.consume(name, idx);
    }

    pub(super) fn add_method(&mut self, class_name: &str, method_name: String, value: Expression, is_static: bool, kind: MethodKind, idx: usize) {
        let body = self.fetch_body(&value);
        let params = self.fetch_params(&value);
        let builder = self.classes.entry(class_name.to_string()).or_insert_with(|| ClassBuilder {
            name: class_name.to_string(),
            ..Default::default()
        });
        builder.methods.push(ClassMethod {
            key: method_name,
            value,
            body,
            is_static,
            kind,
            params,
        });
        self.consume(class_name, idx);
    }

    // User parameter names (`this` excluded) for the function backing a method.
    pub(super) fn fetch_params(&self, expr: &Expression) -> Vec<String> {
        if let Expression::Function { id, .. } = expr {
            let count = self
                .file
                .function_headers
                .get(id.0 as usize)
                .map(|h| h.param_count())
                .unwrap_or(0);
            let user = count.saturating_sub(1);
            (0..user).map(|i| format!("arg{i}")).collect()
        } else {
            Vec::new()
        }
    }

    // True if the function backing `value` uses the ES6 `super` keyword. Such a
    // method must be emitted *inside* a class body (super is invalid elsewhere).
    pub(super) fn body_uses_super(&self, value: &Expression) -> bool {
        let Some(body) = self.fetch_body(value) else { return false };
        let mut finder = SuperFinder { found: false };
        for s in &body {
            crate::ir::Visitor::visit_statement(&mut finder, s);
        }
        finder.found
    }

    pub(super) fn fetch_body(&self, expr: &Expression) -> Option<Vec<Statement>> {
        if let Expression::Function { id, .. } = expr {
            let mut body = crate::generate_ir(self.file, self.format, id.0, self.options, self.closure_ctx, true)
                .map_err(|e| log::debug!("[class_patterns] IR gen failed for func {}: {e}", id.0))
                .ok()?;
            // Method/constructor bodies emitted here bypass the whole-program
            // `strip_hermes_this` pass (it does not recurse into Statement::Class),
            // so apply it locally — otherwise super calls render as
            // `super.who(this)` instead of `super.who()`.
            crate::transforms::strip_hermes_this(&mut body);
            Some(body)
        } else {
            None
        }
    }

    pub(super) fn build_class(&self, builder: &ClassBuilder) -> Statement {
        let mut methods = Vec::new();

        // Add constructor first if present — but omit the compiler-synthesized
        // default constructor (the source had none), so we emit a class with no
        // explicit `constructor`.
        if let Some(ref ctor) = builder.constructor {
            if !is_synthesized_default_ctor(&builder.constructor_body) {
                methods.push(ClassMethod {
                    key: "constructor".to_string(),
                    value: ctor.clone(),
                    body: builder.constructor_body.clone(),
                    is_static: false,
                    kind: MethodKind::Constructor,
                    params: builder.constructor_params.clone(),
                });
            }
        }

        // Add other methods
        methods.extend(builder.methods.clone());

        Statement::Class {
            name: builder.name.clone(),
            super_class: builder.super_class.clone(),
            constructor: None,
            methods,
        }
    }

    pub(super) fn transform_recursive(&mut self, stmt: Statement) -> Statement {
        match stmt {
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: self.analyze(then_body),
                else_body: self.analyze(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition,
                body: self.analyze(body),
            },
            Statement::For { init, condition, update, body } => Statement::For {
                init,
                condition,
                update,
                body: self.analyze(body),
            },
            Statement::ForOf { variable, iterable, body } => Statement::ForOf {
                variable,
                iterable,
                body: self.analyze(body),
            },
            Statement::ForIn { variable, object, body } => Statement::ForIn {
                variable,
                object,
                body: self.analyze(body),
            },
            Statement::Block(inner) => Statement::Block(self.analyze(inner)),
            Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => Statement::TryCatch {
                try_body: self.analyze(try_body),
                catch_param,
                catch_body: self.analyze(catch_body),
                finally_body: self.analyze(finally_body),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Value;

    fn member(obj: Expression, prop: &str) -> Expression {
        Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Ident(prop.to_string()),
            optional: false,
        }
    }

    #[test]
    fn detects_default_base_ctor() {
        // return Object.create(new.target.prototype);
        let body = Some(vec![Statement::Return(Some(Expression::Call {
            callee: Box::new(member(
                Expression::Value(Value::Variable("Object".into())),
                "create",
            )),
            arguments: vec![member(Expression::Value(Value::NewTarget), "prototype")],
        }))]);
        assert!(is_synthesized_default_ctor(&body));
    }

    #[test]
    fn detects_default_derived_ctor() {
        // return HermesBuiltin.applyArguments(...);
        let body = Some(vec![Statement::Return(Some(Expression::Call {
            callee: Box::new(member(
                Expression::Value(Value::Variable("HermesBuiltin".into())),
                "applyArguments",
            )),
            arguments: vec![Expression::Value(Value::NewTarget)],
        }))]);
        assert!(is_synthesized_default_ctor(&body));
    }

    #[test]
    fn keeps_user_ctor() {
        // this.x = arg0; (a real constructor body) is not a synthesized default.
        let body = Some(vec![
            Statement::Assign {
                target: crate::ir::AssignTarget::Member {
                    object: Expression::Value(Value::This),
                    property: "x".to_string(),
                },
                value: Expression::Value(Value::Parameter(0)),
            },
            Statement::Return(Some(Expression::Value(Value::This))),
        ]);
        assert!(!is_synthesized_default_ctor(&body));
        assert!(!is_synthesized_default_ctor(&None));
    }
}
