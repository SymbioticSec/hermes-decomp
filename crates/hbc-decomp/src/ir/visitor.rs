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

    fn walk_statement(&mut self, stmt: &'a Statement) {
        match stmt {
            Statement::Expr(e) => self.visit_expression(e),
            Statement::Let { value, .. } => self.visit_expression(value),
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
                catch_body,
                finally_body,
                ..
            } => {
                for s in try_body {
                    self.visit_statement(s);
                }
                for s in catch_body {
                    self.visit_statement(s);
                }
                for s in finally_body {
                    self.visit_statement(s);
                }
            }
            Statement::ForIn { object, body, .. } => {
                self.visit_expression(object);
                for s in body {
                    self.visit_statement(s);
                }
            }
            Statement::ForOf { iterable, body, .. } => {
                self.visit_expression(iterable);
                for s in body {
                    self.visit_statement(s);
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
                self.visit_expression(target);
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
            _ => {}
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

    fn walk_statement(&mut self, stmt: &mut Statement) {
        match stmt {
            Statement::Expr(e) => self.visit_expression(e),
            Statement::Let { value, .. } => self.visit_expression(value),
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
                catch_body,
                finally_body,
                ..
            } => {
                self.visit_statement_list(try_body);
                self.visit_statement_list(catch_body);
                self.visit_statement_list(finally_body);
            }
            Statement::ForIn { object, body, .. } => {
                self.visit_expression(object);
                self.visit_statement_list(body);
            }
            Statement::ForOf { iterable, body, .. } => {
                self.visit_expression(iterable);
                self.visit_statement_list(body);
            }
            Statement::Return(None)
            | Statement::Debugger
            | Statement::Comment(_)
            | Statement::Break(_)
            | Statement::Continue(_)
            | Statement::Goto(_)
            | Statement::Class { .. } => {}
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
                self.visit_expression(target);
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
            _ => {}
        }
    }
}
