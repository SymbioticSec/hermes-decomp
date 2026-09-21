use super::{AssignTarget, Expression, PropertyKey, Statement};

pub trait Visitor<'a> {
    fn visit_statement(&mut self, stmt: &'a Statement) {
        self.walk_statement(stmt);
    }

    fn visit_expression(&mut self, expr: &'a Expression) {
        self.walk_expression(expr);
    }

    fn visit_assign_target(&mut self, target: &'a AssignTarget) {
        self.walk_assign_target(target);
    }

    // A name this statement introduces: a `let`, a loop head, a catch parameter,
    // a class or one of its method parameters. These were the only write
    // positions no hook ever saw, so a pass wanting the names a body declares had
    // to re-walk the tree itself, and each one that did missed a different subset.
    fn visit_binding_def(&mut self, _name: &'a str) {}

    fn walk_statement(&mut self, stmt: &'a Statement) {
        match stmt {
            Statement::Expr(e) => self.visit_expression(e),
            Statement::Let { name, value, .. } => {
                self.visit_binding_def(name);
                self.visit_expression(value);
            }
            Statement::Assign { target, value } => {
                self.visit_assign_target(target);
                self.visit_expression(value);
            }
            Statement::Delete { target, .. } => self.visit_expression(target),
            Statement::Return(Some(e)) => self.visit_expression(e),
            Statement::Throw(e) => self.visit_expression(e),
            Statement::CondGoto { condition, .. } => self.visit_expression(condition),
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                self.visit_expression(condition);
                for s in then_body {
                    self.visit_statement(s);
                }
                for s in else_body {
                    self.visit_statement(s);
                }
            }
            Statement::While { condition, body } => {
                self.visit_expression(condition);
                for s in body {
                    self.visit_statement(s);
                }
            }
            Statement::DoWhile { body, condition } => {
                for s in body {
                    self.visit_statement(s);
                }
                self.visit_expression(condition);
            }
            Statement::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.visit_statement(i);
                }
                if let Some(c) = condition {
                    self.visit_expression(c);
                }
                if let Some(u) = update {
                    self.visit_statement(u);
                }
                for s in body {
                    self.visit_statement(s);
                }
            }
            Statement::Switch {
                discriminant,
                cases,
                default,
            } => {
                self.visit_expression(discriminant);
                for (case_val, block) in cases {
                    self.visit_expression(case_val);
                    for s in block {
                        self.visit_statement(s);
                    }
                }
                if let Some(block) = default {
                    for s in block {
                        self.visit_statement(s);
                    }
                }
            }
            Statement::Block(stmts) => {
                for s in stmts {
                    self.visit_statement(s);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                for s in try_body {
                    self.visit_statement(s);
                }
                if let Some(name) = catch_param {
                    self.visit_binding_def(name);
                }
                for s in catch_body {
                    self.visit_statement(s);
                }
                for s in finally_body {
                    self.visit_statement(s);
                }
            }
            Statement::ForIn { variable, object, body } => {
                self.visit_binding_def(variable);
                self.visit_expression(object);
                for s in body {
                    self.visit_statement(s);
                }
            }
            Statement::ForOf { variable, iterable, body } => {
                self.visit_binding_def(variable);
                self.visit_expression(iterable);
                for s in body {
                    self.visit_statement(s);
                }
            }
            // The names a class introduces are reported, its body is not walked.
            // Descending into `super_class` was tried and renamed `extends
            // _default` to `extends r10023` on the reference bundle: the passes
            // that rewrite expressions were written on the assumption that a class
            // body is out of reach, and several of them are wrong inside one.
            Statement::Class { name, methods, .. } => {
                self.visit_binding_def(name);
                for method in methods {
                    for param in &method.params {
                        self.visit_binding_def(param);
                    }
                }
            }
            _ => {}
        }
    }

    fn walk_expression(&mut self, expr: &'a Expression) {
        match expr {
            Expression::Binary { left, right, .. } => {
                self.visit_expression(left);
                self.visit_expression(right);
            }
            Expression::Unary { operand, .. } => {
                self.visit_expression(operand);
            }
            Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
                self.visit_expression(callee);
                for arg in arguments {
                    self.visit_expression(arg);
                }
            }
            Expression::Member {
                object, property, ..
            } => {
                self.visit_expression(object);
                if let PropertyKey::Computed(key) = property {
                    self.visit_expression(key);
                }
            }
            Expression::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                self.visit_expression(condition);
                self.visit_expression(then_expr);
                self.visit_expression(else_expr);
            }
            Expression::Object { properties } => {
                for prop in properties {
                    if let PropertyKey::Computed(key) = &prop.key {
                        self.visit_expression(key);
                    }
                    self.visit_expression(&prop.value);
                }
            }
            Expression::Array { elements } => {
                for e in elements.iter().flatten() {
                    self.visit_expression(e);
                }
            }
            Expression::Assignment { target, value } => {
                self.visit_assign_target(target);
                self.visit_expression(value);
            }
            Expression::Spread(e) | Expression::Await(e) => {
                self.visit_expression(e);
            }
            Expression::Yield { value, .. } => {
                self.visit_expression(value);
            }
            Expression::TemplateLiteral { expressions, .. } => {
                for e in expressions {
                    self.visit_expression(e);
                }
            }
            Expression::JSXElement {
                attributes,
                children,
                ..
            } => {
                for (_, expr) in attributes {
                    self.visit_expression(expr);
                }
                for child in children {
                    self.visit_expression(child);
                }
            }
            Expression::Value(_)
            | Expression::RegExp { .. }
            | Expression::Function { .. }
            | Expression::Unknown { .. } => {}
        }
    }

    fn walk_assign_target(&mut self, target: &'a AssignTarget) {
        match target {
            AssignTarget::Member { object, .. } => {
                self.visit_expression(object);
            }
            AssignTarget::Index { object, key } => {
                self.visit_expression(object);
                self.visit_expression(key);
            }
            AssignTarget::DestructuringArray(arr) => {
                for item in arr.iter().flatten() {
                    self.visit_assign_target(&item.0);
                    if let Some(def_val) = &item.1 {
                        self.visit_expression(def_val);
                    }
                }
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                for item in elements.iter().flatten() {
                    self.visit_assign_target(&item.0);
                    if let Some(def_val) = &item.1 {
                        self.visit_expression(def_val);
                    }
                }
                self.visit_assign_target(rest);
            }
            AssignTarget::DestructuringObject(props) => {
                for (_, t, def_val) in props {
                    self.visit_assign_target(t);
                    if let Some(d) = def_val {
                        self.visit_expression(d);
                    }
                }
            }
            AssignTarget::DestructuringObjectRest { properties, rest } => {
                for (_, t, def_val) in properties {
                    self.visit_assign_target(t);
                    if let Some(d) = def_val {
                        self.visit_expression(d);
                    }
                }
                self.visit_assign_target(rest);
            }
            AssignTarget::Rest(t) => self.visit_assign_target(t),
            // A binding is a leaf, there is nothing under it to walk. Spelt out
            // rather than swallowed by a catch all, so a variant added later is a
            // compile error here instead of a silently unvisited place.
            AssignTarget::Binding(_) => {}
        }
    }
}

pub trait MutVisitor {
    fn visit_statement(&mut self, stmt: &mut Statement) {
        self.walk_statement(stmt);
    }
    fn visit_statement_list(&mut self, stmts: &mut Vec<Statement>) {
        self.walk_statement_list(stmts);
    }

    fn walk_statement_list(&mut self, stmts: &mut Vec<Statement>) {
        for s in stmts.iter_mut() {
            self.visit_statement(s);
        }
    }

    fn visit_expression(&mut self, expr: &mut Expression) {
        self.walk_expression(expr);
    }

    fn visit_assign_target(&mut self, target: &mut AssignTarget) {
        self.walk_assign_target(target);
    }

    // The mutable counterpart of `Visitor::visit_binding_def`, so a renaming pass
    // can reach a declared name the same way it reaches a written one.
    fn visit_binding_def(&mut self, _name: &mut String) {}

    fn walk_statement(&mut self, stmt: &mut Statement) {
        match stmt {
            Statement::Expr(e) => self.visit_expression(e),
            Statement::Let { name, value, .. } => {
                self.visit_binding_def(name);
                self.visit_expression(value);
            }
            Statement::Assign { target, value } => {
                self.visit_assign_target(target);
                self.visit_expression(value);
            }
            Statement::Delete { target, .. } => self.visit_expression(target),
            Statement::Return(Some(e)) => self.visit_expression(e),
            Statement::Throw(e) => self.visit_expression(e),
            Statement::CondGoto { condition, .. } => self.visit_expression(condition),
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                self.visit_expression(condition);
                self.visit_statement_list(then_body);
                self.visit_statement_list(else_body);
            }
            Statement::While { condition, body } => {
                self.visit_expression(condition);
                self.visit_statement_list(body);
            }
            Statement::DoWhile { body, condition } => {
                self.visit_statement_list(body);
                self.visit_expression(condition);
            }
            Statement::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.visit_statement(i);
                }
                if let Some(c) = condition {
                    self.visit_expression(c);
                }
                if let Some(u) = update {
                    self.visit_statement(u);
                }
                self.visit_statement_list(body);
            }
            Statement::Switch {
                discriminant,
                cases,
                default,
            } => {
                self.visit_expression(discriminant);
                for (case_val, block) in cases {
                    self.visit_expression(case_val);
                    self.visit_statement_list(block);
                }
                if let Some(block) = default {
                    self.visit_statement_list(block);
                }
            }
            Statement::Block(stmts) => {
                self.visit_statement_list(stmts);
            }
            Statement::TryCatch {
                try_body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                self.visit_statement_list(try_body);
                if let Some(name) = catch_param {
                    self.visit_binding_def(name);
                }
                self.visit_statement_list(catch_body);
                self.visit_statement_list(finally_body);
            }
            Statement::ForIn { variable, object, body } => {
                self.visit_binding_def(variable);
                self.visit_expression(object);
                self.visit_statement_list(body);
            }
            Statement::ForOf { variable, iterable, body } => {
                self.visit_binding_def(variable);
                self.visit_expression(iterable);
                self.visit_statement_list(body);
            }
            // See the read only walker: the names are reported, the body is not
            // walked.
            Statement::Class { name, methods, .. } => {
                self.visit_binding_def(name);
                for method in methods.iter_mut() {
                    for param in method.params.iter_mut() {
                        self.visit_binding_def(param);
                    }
                }
            }
            Statement::Return(None)
            | Statement::Debugger
            | Statement::Comment(_)
            | Statement::Break(_)
            | Statement::Continue(_)
            | Statement::Goto(_) => {}
        }
    }

    fn walk_expression(&mut self, expr: &mut Expression) {
        match expr {
            Expression::Binary { left, right, .. } => {
                self.visit_expression(left);
                self.visit_expression(right);
            }
            Expression::Unary { operand, .. } => {
                self.visit_expression(operand);
            }
            Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
                self.visit_expression(callee);
                for arg in arguments {
                    self.visit_expression(arg);
                }
            }
            Expression::Member {
                object, property, ..
            } => {
                self.visit_expression(object);
                if let PropertyKey::Computed(key) = property {
                    self.visit_expression(key);
                }
            }
            Expression::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                self.visit_expression(condition);
                self.visit_expression(then_expr);
                self.visit_expression(else_expr);
            }
            Expression::Object { properties } => {
                for prop in properties {
                    if let PropertyKey::Computed(key) = &mut prop.key {
                        self.visit_expression(key);
                    }
                    self.visit_expression(&mut prop.value);
                }
            }
            Expression::Array { elements } => {
                for e in elements.iter_mut().flatten() {
                    self.visit_expression(e);
                }
            }
            Expression::Assignment { target, value } => {
                self.visit_assign_target(target);
                self.visit_expression(value);
            }
            Expression::Spread(e) | Expression::Await(e) => {
                self.visit_expression(e);
            }
            Expression::Yield { value, .. } => {
                self.visit_expression(value);
            }
            Expression::TemplateLiteral { expressions, .. } => {
                for e in expressions {
                    self.visit_expression(e);
                }
            }
            Expression::JSXElement {
                attributes,
                children,
                ..
            } => {
                for (_, expr) in attributes {
                    self.visit_expression(expr);
                }
                for child in children {
                    self.visit_expression(child);
                }
            }
            Expression::Value(_)
            | Expression::RegExp { .. }
            | Expression::Function { .. }
            | Expression::Unknown { .. } => {}
        }
    }

    fn walk_assign_target(&mut self, target: &mut AssignTarget) {
        match target {
            AssignTarget::Member { object, .. } => {
                self.visit_expression(object);
            }
            AssignTarget::Index { object, key } => {
                self.visit_expression(object);
                self.visit_expression(key);
            }
            AssignTarget::DestructuringArray(arr) => {
                for item in arr.iter_mut().flatten() {
                    self.visit_assign_target(&mut item.0);
                    if let Some(def_val) = &mut item.1 {
                        self.visit_expression(def_val);
                    }
                }
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                for item in elements.iter_mut().flatten() {
                    self.visit_assign_target(&mut item.0);
                    if let Some(def_val) = &mut item.1 {
                        self.visit_expression(def_val);
                    }
                }
                self.visit_assign_target(rest);
            }
            AssignTarget::DestructuringObject(props) => {
                for (_, t, def_val) in props {
                    self.visit_assign_target(t);
                    if let Some(d) = def_val {
                        self.visit_expression(d);
                    }
                }
            }
            AssignTarget::DestructuringObjectRest { properties, rest } => {
                for (_, t, def_val) in properties {
                    self.visit_assign_target(t);
                    if let Some(d) = def_val {
                        self.visit_expression(d);
                    }
                }
                self.visit_assign_target(rest);
            }
            AssignTarget::Rest(t) => self.visit_assign_target(t),
            AssignTarget::Binding(_) => {}
        }
    }
}

#[cfg(test)]
mod write_target_tests {
    use super::{MutVisitor, Visitor};
    use crate::ir::{AssignTarget, Binding, Constant, Expression, Statement, Value};

    fn assignment_expression() -> Expression {
        Expression::Assignment {
            target: Box::new(AssignTarget::Binding(Binding::Register(3))),
            value: Box::new(Expression::constant(Constant::Integer(1))),
        }
    }

    // An assignment used as an expression is still a write. Routing its target
    // through `visit_assign_target` is what lets one hook see every write in a
    // body: before this, three separate passes each carried their own copy of the
    // unwrap to count those definitions.
    #[test]
    fn a_write_in_expression_position_reaches_the_target_hook() {
        struct Count {
            targets: Vec<Binding>,
        }
        impl<'a> Visitor<'a> for Count {
            fn visit_assign_target(&mut self, t: &'a AssignTarget) {
                if let AssignTarget::Binding(b) = t {
                    self.targets.push(b.clone());
                }
                self.walk_assign_target(t);
            }
        }
        let mut count = Count {
            targets: Vec::new(),
        };
        count.visit_statement(&Statement::Expr(assignment_expression()));
        assert_eq!(count.targets, vec![Binding::Register(3)]);
    }

    #[test]
    fn the_mutable_walk_reaches_it_too() {
        struct Bump;
        impl MutVisitor for Bump {
            fn visit_assign_target(&mut self, t: &mut AssignTarget) {
                if let AssignTarget::Binding(Binding::Register(r)) = t {
                    *r += 1;
                }
                self.walk_assign_target(t);
            }
        }
        let mut stmt = Statement::Expr(assignment_expression());
        Bump.visit_statement(&mut stmt);
        match &stmt {
            Statement::Expr(Expression::Assignment { target, .. }) => {
                assert_eq!(**target, AssignTarget::Binding(Binding::Register(4)));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // The target of an expression assignment is not an expression, so the
    // expression hook must not see it as a value being read.
    #[test]
    fn the_target_is_not_reported_as_a_read() {
        struct Reads(Vec<String>);
        impl<'a> Visitor<'a> for Reads {
            fn visit_expression(&mut self, e: &'a Expression) {
                if let Expression::Value(Value::Binding(b)) = e {
                    self.0.push(b.to_string());
                }
                self.walk_expression(e);
            }
        }
        let mut reads = Reads(Vec::new());
        reads.visit_statement(&Statement::Expr(assignment_expression()));
        assert!(reads.0.is_empty(), "a place is not a value: {:?}", reads.0);
    }
}

#[cfg(test)]
mod binding_def_tests {
    use super::{MutVisitor, Visitor};
    use crate::ir::{ClassMethod, Constant, Expression, Statement, VarKind};

    fn body() -> Vec<Statement> {
        vec![
            Statement::Let {
                name: "declared".into(),
                value: Expression::constant(Constant::Integer(1)),
                kind: VarKind::Let,
            },
            Statement::ForOf {
                variable: "item".into(),
                iterable: Expression::constant(Constant::Integer(2)),
                body: vec![],
            },
            Statement::ForIn {
                variable: "key".into(),
                object: Expression::constant(Constant::Integer(3)),
                body: vec![],
            },
            Statement::TryCatch {
                try_body: vec![],
                catch_param: Some("err".into()),
                catch_body: vec![],
                finally_body: vec![],
            },
            Statement::Class {
                name: "Widget".into(),
                super_class: None,
                constructor: None,
                methods: vec![ClassMethod {
                    key: "render".into(),
                    value: Expression::constant(Constant::Integer(4)),
                    body: None,
                    is_static: false,
                    kind: crate::ir::MethodKind::Method,
                    params: vec!["props".into()],
                }],
            },
        ]
    }

    // Every one of these positions introduces a name and none of them was
    // reachable from a visitor before, so a pass that wanted the names a body
    // declares had to re-walk the tree and each one that did missed a different
    // subset.
    #[test]
    fn every_declaration_position_reaches_the_hook() {
        struct Collect(Vec<String>);
        impl<'a> Visitor<'a> for Collect {
            fn visit_binding_def(&mut self, name: &'a str) {
                self.0.push(name.to_string());
            }
        }
        let mut c = Collect(Vec::new());
        for stmt in &body() {
            c.visit_statement(stmt);
        }
        assert_eq!(
            c.0,
            vec!["declared", "item", "key", "err", "Widget", "props"]
        );
    }

    #[test]
    fn the_mutable_hook_can_rename_a_declaration() {
        struct Upper;
        impl MutVisitor for Upper {
            fn visit_binding_def(&mut self, name: &mut String) {
                *name = name.to_uppercase();
            }
        }
        let mut stmts = body();
        for stmt in stmts.iter_mut() {
            Upper.visit_statement(stmt);
        }
        match &stmts[0] {
            Statement::Let { name, .. } => assert_eq!(name, "DECLARED"),
            other => panic!("unexpected {other:?}"),
        }
        match &stmts[3] {
            Statement::TryCatch { catch_param, .. } => {
                assert_eq!(catch_param.as_deref(), Some("ERR"))
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // A class body is deliberately out of reach: several passes that rewrite
    // expressions are wrong inside one, and walking it renamed `extends _default`
    // to `extends r10023` on the reference bundle.
    #[test]
    fn a_class_body_is_not_walked() {
        struct Count(usize);
        impl<'a> Visitor<'a> for Count {
            fn visit_expression(&mut self, e: &'a Expression) {
                self.0 += 1;
                self.walk_expression(e);
            }
        }
        let mut c = Count(0);
        c.visit_statement(&Statement::Class {
            name: "Widget".into(),
            super_class: Some(Expression::constant(Constant::Integer(9))),
            constructor: None,
            methods: vec![],
        });
        assert_eq!(c.0, 0, "the super class expression stays out of reach");
    }
}
