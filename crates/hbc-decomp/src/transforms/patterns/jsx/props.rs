use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Binding, Expression, Statement, Value, Visitor,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Phase 1, 1-hop props resolution (same block, sequential)
// ---------------------------------------------------------------------------

// The object literals in scope for a props argument, and the names a jsx call
// took its props from. A definition that fed exactly one call is removed once
// the call carries the literal, otherwise it stays behind as a dead object
// that later reads as a stray `{ ... };` statement.
pub(super) struct PropScope {
    map: BTreeMap<String, Expression>,
    consumed: RefCell<Vec<String>>,
}

impl PropScope {
    fn get(&self, name: &str) -> Option<&Expression> {
        let found = self.map.get(name)?;
        self.consumed.borrow_mut().push(name.to_string());
        Some(found)
    }
}

pub(super) fn resolve_prop_object_vars(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut uses: BTreeMap<String, usize> = BTreeMap::new();
    count_variable_uses(&stmts, &mut uses);

    let mut out = Vec::with_capacity(stmts.len());
    let mut scope = PropScope {
        map: BTreeMap::new(),
        consumed: RefCell::new(Vec::new()),
    };
    let mut def_index: BTreeMap<String, usize> = BTreeMap::new();

    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, kind } => {
                let value = maybe_subst_call(value, &scope);
                if matches!(value, Expression::Object { .. }) {
                    scope.map.insert(name.clone(), value.clone());
                    def_index.insert(name.clone(), out.len());
                }
                out.push(Statement::Let { name, value, kind });
            }
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(name)),
                value,
            } => {
                let value = maybe_subst_call(value, &scope);
                if matches!(value, Expression::Object { .. }) {
                    scope.map.insert(name.clone(), value.clone());
                    def_index.insert(name.clone(), out.len());
                } else {
                    scope.map.remove(&name);
                    def_index.remove(&name);
                }
                out.push(Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(name)),
                    value,
                });
            }
            other => {
                let mut s = other;
                // Nested blocks get their own scope (fresh map via recursion).
                map_nested_bodies_mut(&mut s, resolve_prop_object_vars);
                // Also rewrite jsx calls at this level if Assign/Expr not caught above.
                rewrite_stmt_calls(&mut s, &scope);
                out.push(s);
            }
        }
    }

    let mut drop: Vec<usize> = scope
        .consumed
        .borrow()
        .iter()
        .filter(|name| uses.get(*name).copied().unwrap_or(0) == 1)
        .filter_map(|name| def_index.get(name).copied())
        .collect();
    drop.sort_unstable();
    drop.dedup();
    for idx in drop.into_iter().rev() {
        out.remove(idx);
    }
    out
}

fn count_variable_uses(stmts: &[Statement], uses: &mut BTreeMap<String, usize>) {
    struct C<'a>(&'a mut BTreeMap<String, usize>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(name))) = e {
                *self.0.entry(name.clone()).or_insert(0) += 1;
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(uses);
    for s in stmts {
        c.visit_statement(s);
    }
}

fn maybe_subst_call(mut expr: Expression, objects: &PropScope) -> Expression {
    subst_jsx_props_in_expr(&mut expr, objects);
    expr
}

fn rewrite_stmt_calls(stmt: &mut Statement, objects: &PropScope) {
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            subst_jsx_props_in_expr(e, objects);
        }
        Statement::Assign { value, .. } | Statement::Let { value, .. } => {
            subst_jsx_props_in_expr(value, objects);
        }
        _ => {}
    }
}

fn subst_jsx_props_in_expr(expr: &mut Expression, objects: &PropScope) {
    match expr {
        Expression::Call { callee, arguments }
            if super::is_jsx_call(callee) && arguments.len() >= 2 =>
        {
            if let Expression::Value(Value::Binding(Binding::Variable(name))) = &arguments[1] {
                if let Some(obj) = objects.get(name) {
                    arguments[1] = obj.clone();
                }
            }
            // Recurse into children args (classic createElement children may nest jsx)
            for a in arguments.iter_mut() {
                subst_jsx_props_in_expr(a, objects);
            }
            subst_jsx_props_in_expr(callee, objects);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            subst_jsx_props_in_expr(callee, objects);
            for a in arguments {
                subst_jsx_props_in_expr(a, objects);
            }
        }
        Expression::Binary { left, right, .. } => {
            subst_jsx_props_in_expr(left, objects);
            subst_jsx_props_in_expr(right, objects);
        }
        Expression::Unary { operand, .. }
        | Expression::Spread(operand)
        | Expression::Await(operand)
        | Expression::Yield { value: operand, .. } => subst_jsx_props_in_expr(operand, objects),
        Expression::Member { object, .. } => subst_jsx_props_in_expr(object, objects),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            subst_jsx_props_in_expr(condition, objects);
            subst_jsx_props_in_expr(then_expr, objects);
            subst_jsx_props_in_expr(else_expr, objects);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() {
                subst_jsx_props_in_expr(e, objects);
            }
        }
        Expression::Object { properties } => {
            for p in properties {
                subst_jsx_props_in_expr(&mut p.value, objects);
            }
        }
        Expression::Assignment { target, value } => {
            crate::ir::for_each_target_expression_mut(target, &mut |e| {
                subst_jsx_props_in_expr(e, objects)
            });
            subst_jsx_props_in_expr(value, objects);
        }
        Expression::JSXElement {
            attributes,
            children,
            ..
        } => {
            for (_, v) in attributes {
                subst_jsx_props_in_expr(v, objects);
            }
            for c in children {
                subst_jsx_props_in_expr(c, objects);
            }
        }
        _ => {}
    }
}
