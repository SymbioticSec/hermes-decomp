// Statement/expression walk for env stores and nested Function edges.
use std::collections::BTreeMap;
use crate::ir::{AssignTarget, Expression, Statement};
use super::super::info::{ClosureInfo, ClosureSlotValue};
use super::ClosureContext;

impl ClosureContext {
    pub(super) fn analyze_stmt_context(
        &mut self,
        current_fn: u32,
        stmt: &Statement,
        info: &mut ClosureInfo,
        reg_values: &mut BTreeMap<u32, ClosureSlotValue>,
        named_values: &mut BTreeMap<String, ClosureSlotValue>,
        deferred: &mut Vec<(u32, u32, u32, ClosureSlotValue)>,
    ) {
        match stmt {
            Statement::Assign { target, value } => {
                self.track_nested_functions(current_fn, value);

                if let Expression::Function { id, name, .. } = value {
                    if let AssignTarget::Register(r) = target {
                        reg_values.insert(
                            *r,
                            ClosureSlotValue::Function {
                                id: id.0,
                                name: name.clone(),
                            },
                        );
                    }
                    if let AssignTarget::Variable(vname) = target {
                        named_values.insert(
                            vname.clone(),
                            ClosureSlotValue::Function {
                                id: id.0,
                                name: name.clone().or_else(|| Some(vname.clone())),
                            },
                        );
                    }
                }

                // Track through register copies (r5 = r3) so later env stores see
                // the origin (require / function / named binding).
                if let AssignTarget::Register(r) = target {
                    if let Some(val) = Self::resolve_store_value(value, reg_values, named_values) {
                        reg_values.insert(*r, val);
                    }
                }

                if let AssignTarget::Variable(name) = target {
                    if let Some(val) = Self::resolve_store_value(value, reg_values, named_values) {
                        named_values.insert(name.clone(), val);
                    }
                }

                if let AssignTarget::ClosureVar { slot, level } = target {
                    if let Some(val) = Self::resolve_store_value(value, reg_values, named_values) {
                        if *level == 0 {
                            info.store_slot(*slot, val);
                        } else {
                            // Nested body writing into a parent/grandparent env.
                            deferred.push((current_fn, *level, *slot, val));
                        }
                    }
                }
            }
            Statement::Let { value, name, .. } => {
                self.track_nested_functions(current_fn, value);
                if let Expression::Function { id, name: fn_name, .. } = value {
                    // Prefer the binding name when the function expression is anonymous.
                    if fn_name.is_none() {
                        self.add_function_name(id.0, name.clone());
                    }
                    named_values.insert(
                        name.clone(),
                        ClosureSlotValue::Function {
                            id: id.0,
                            name: fn_name.clone().or_else(|| Some(name.clone())),
                        },
                    );
                } else if let Some(val) =
                    Self::resolve_store_value(value, reg_values, named_values)
                {
                    named_values.insert(name.clone(), val);
                }
            }
            Statement::Return(Some(value)) | Statement::Expr(value) | Statement::Throw(value) => {
                self.track_nested_functions(current_fn, value);
            }
            Statement::Delete { target, .. } => {
                self.track_nested_functions(current_fn, target);
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                self.track_nested_functions(current_fn, condition);
                for s in then_body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
                for s in else_body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
            }
            Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
                self.track_nested_functions(current_fn, condition);
                for s in body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
            }
            Statement::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.analyze_stmt_context(current_fn, i, info, reg_values, named_values, deferred);
                }
                if let Some(c) = condition {
                    self.track_nested_functions(current_fn, c);
                }
                if let Some(u) = update {
                    self.analyze_stmt_context(current_fn, u, info, reg_values, named_values, deferred);
                }
                for s in body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
            }
            Statement::ForIn {
                object, body, ..
            } => {
                self.track_nested_functions(current_fn, object);
                for s in body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
            }
            Statement::ForOf {
                iterable, body, ..
            } => {
                self.track_nested_functions(current_fn, iterable);
                for s in body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
            }
            Statement::Block(inner) => {
                for s in inner {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                for s in try_body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
                for s in catch_body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
                for s in finally_body {
                    self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                }
            }
            Statement::Switch {
                discriminant,
                cases,
                default,
            } => {
                self.track_nested_functions(current_fn, discriminant);
                for (val, body) in cases {
                    self.track_nested_functions(current_fn, val);
                    for s in body {
                        self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                    }
                }
                if let Some(d) = default {
                    for s in d {
                        self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                    }
                }
            }
            Statement::Class {
                super_class,
                constructor,
                methods,
                ..
            } => {
                if let Some(sc) = super_class {
                    self.track_nested_functions(current_fn, sc);
                }
                if let Some(c) = constructor {
                    self.analyze_stmt_context(current_fn, c, info, reg_values, named_values, deferred);
                }
                for m in methods {
                    self.track_nested_functions(current_fn, &m.value);
                    if let Some(body) = &m.body {
                        for s in body {
                            self.analyze_stmt_context(current_fn, s, info, reg_values, named_values, deferred);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Track Function expressions nested in values: bare `function*`, or
    // CreateGenerator's `(function*(){})()` call form.
    pub(super) fn track_nested_functions(&mut self, parent_fn: u32, expr: &Expression) {
        match expr {
            Expression::Function {
                id,
                name,
                is_async,
                is_generator,
                ..
            } => {
                self.add_child(parent_fn, id.0);
                if let Some(n) = name {
                    self.add_function_name(id.0, n.clone());
                }
                if *is_async {
                    self.mark_async(id.0);
                }
                if *is_generator {
                    self.mark_generator(id.0);
                }
            }
            Expression::Call { callee, arguments } => {
                self.track_nested_functions(parent_fn, callee);
                for arg in arguments {
                    self.track_nested_functions(parent_fn, arg);
                }
            }
            Expression::Member { object, property, .. } => {
                self.track_nested_functions(parent_fn, object);
                if let crate::ir::PropertyKey::Computed(e) = property {
                    self.track_nested_functions(parent_fn, e);
                }
            }
            Expression::New { callee, arguments } => {
                self.track_nested_functions(parent_fn, callee);
                for arg in arguments {
                    self.track_nested_functions(parent_fn, arg);
                }
            }
            Expression::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                self.track_nested_functions(parent_fn, condition);
                self.track_nested_functions(parent_fn, then_expr);
                self.track_nested_functions(parent_fn, else_expr);
            }
            Expression::Assignment { target, value } => {
                self.track_nested_functions(parent_fn, target);
                self.track_nested_functions(parent_fn, value);
            }
            Expression::Array { elements } => {
                for e in elements.iter().flatten() {
                    self.track_nested_functions(parent_fn, e);
                }
            }
            Expression::Object { properties } => {
                for p in properties {
                    self.track_nested_functions(parent_fn, &p.value);
                    if let crate::ir::PropertyKey::Computed(e) = &p.key {
                        self.track_nested_functions(parent_fn, e);
                    }
                }
            }
            Expression::Unary { operand, .. }
            | Expression::Spread(operand)
            | Expression::Await(operand) => {
                self.track_nested_functions(parent_fn, operand);
            }
            Expression::Binary { left, right, .. } => {
                self.track_nested_functions(parent_fn, left);
                self.track_nested_functions(parent_fn, right);
            }
            Expression::Yield { value, .. } => {
                self.track_nested_functions(parent_fn, value);
            }
            Expression::TemplateLiteral { expressions, .. } => {
                for e in expressions {
                    self.track_nested_functions(parent_fn, e);
                }
            }
            _ => {}
        }
    }
}
