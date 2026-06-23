use crate::ir::{Statement, Expression, ClassMethod, MethodKind};
use crate::transforms::class_patterns::builder::ClassBuilder;
use super::ClassAnalyzer;

impl<'a> ClassAnalyzer<'a> {
    pub(super) fn register_constructor(&mut self, name: &str, value: Expression, idx: usize) {
        let body = self.fetch_body(&value);
        let builder = self.classes.entry(name.to_string()).or_insert_with(|| ClassBuilder {
            name: name.to_string(),
            ..Default::default()
        });
        builder.constructor = Some(value);
        builder.constructor_body = body;
        self.consumed.insert(idx);
    }

    pub(super) fn add_method(&mut self, class_name: &str, method_name: String, value: Expression, is_static: bool, kind: MethodKind, idx: usize) {
        let body = self.fetch_body(&value);
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
        });
        self.consumed.insert(idx);
    }

    pub(super) fn fetch_body(&self, expr: &Expression) -> Option<Vec<Statement>> {
        if let Expression::Function { id, .. } = expr {
            crate::generate_ir(self.file, self.format, id.0, self.options, self.closure_ctx, true)
                .map_err(|e| log::debug!("[class_patterns] IR gen failed for func {}: {e}", id.0))
                .ok()
        } else {
            None
        }
    }

    pub(super) fn get_class_for_index(&self, _idx: usize) -> Option<String> {
        // This is a simplification - find the first class that has this index consumed
        // TODO: Track which indices belong to which class for proper ordering
        self.classes.keys().next().cloned()
    }

    pub(super) fn build_class(&self, builder: &ClassBuilder) -> Statement {
        let mut methods = Vec::new();

        // Add constructor first if present
        if let Some(ref ctor) = builder.constructor {
            methods.push(ClassMethod {
                key: "constructor".to_string(),
                value: ctor.clone(),
                body: builder.constructor_body.clone(),
                is_static: false,
                kind: MethodKind::Constructor,
            });
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
