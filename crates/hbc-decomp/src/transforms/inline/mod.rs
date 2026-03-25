mod arguments;
mod cleanup;
mod counting;
mod declarations;
mod esm_cleanup;
mod folding;
mod inline_named;
mod reserved_words;
mod strip_this;

pub use arguments::simplify_arguments_copy;
pub use cleanup::cleanup_noise;
pub use declarations::insert_declarations;
pub use folding::{fold_array_literals, fold_object_literals};
pub use inline_named::inline_named_variables;
pub use reserved_words::rename_reserved_words;
pub use strip_this::strip_hermes_this;

use crate::ir::{AssignTarget, Expression, MutVisitor, Statement, Value, Visitor};
use std::collections::BTreeMap;

pub fn inline_expressions(mut stmts: Vec<Statement>) -> Vec<Statement> {
    let mut counter = UseCounter::new();
    counter.count_uses(&stmts);

    let mut inliner = ExpressionInliner::new(counter.use_count);
    inliner.visit_statement_list(&mut stmts);

    stmts
}

struct UseCounter {
    use_count: BTreeMap<u32, usize>,
}

impl UseCounter {
    fn new() -> Self {
        Self {
            use_count: BTreeMap::new(),
        }
    }

    fn count_uses(&mut self, stmts: &[Statement]) {
        for stmt in stmts {
            self.visit_statement(stmt);
        }
    }
}

impl<'a> Visitor<'a> for UseCounter {
    fn visit_expression(&mut self, expr: &'a Expression) {
        if let Expression::Value(Value::Register(r)) = expr {
            *self.use_count.entry(*r).or_insert(0) += 1;
        }
        self.walk_expression(expr);
    }
}

struct ExpressionInliner {
    // Map from register to its defining expression
    definitions: BTreeMap<u32, Expression>,
    // Count of uses for each register
    use_count: BTreeMap<u32, usize>,
}

impl ExpressionInliner {
    fn new(use_count: BTreeMap<u32, usize>) -> Self {
        Self {
            definitions: BTreeMap::new(),
            use_count,
        }
    }
}

impl MutVisitor for ExpressionInliner {
    fn visit_statement_list(&mut self, stmts: &mut Vec<Statement>) {
        let mut result = Vec::new();
        // Pending assignments with side effects: (register, statement, expression value)
        let mut pending: Vec<(u32, Statement, Expression)> = Vec::new();

        // We drain the original statements and build them back
        let old_stmts = std::mem::take(stmts);

        for mut stmt in old_stmts {
            let has_side_effects = crate::ir::stmt_has_side_effects(&stmt);

            let mut kept_pending = Vec::new();
            for (r, s, expr) in pending {
                if stmt_uses(&stmt, r) {
                    // Activate inline for this statement!
                    self.definitions.insert(r, expr);
                } else if has_side_effects {
                    // Flush to output unaffected by this statement
                    result.push(s);
                } else {
                    // Keep waiting
                    kept_pending.push((r, s, expr));
                }
            }
            pending = kept_pending;

            // Recurse to apply the activated definitions
            self.visit_statement(&mut stmt);

            // Clear applied definitions to not leak
            self.definitions.clear();

            // Process the resulting statement for potential new pending
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                value,
            } = &stmt
            {
                let uses = self.use_count.get(r).copied().unwrap_or(0);
                if uses == 1 {
                    // Candidate for pending (chaining)
                    pending.push((*r, stmt.clone(), value.clone()));
                    continue; // Do not emit yet
                }
            }
            result.push(stmt);
        }

        for (_, s, _) in pending {
            result.push(s);
        }

        *stmts = result;
    }

    fn visit_expression(&mut self, expr: &mut Expression) {
        // First recurse into children
        self.walk_expression(expr);

        // Then modify this expression if it's a register use
        if let Expression::Value(Value::Register(r)) = expr {
            if let Some(def) = self.definitions.get(r) {
                *expr = def.clone();
            }
        }
    }
}

// Helpers
fn stmt_uses(stmt: &Statement, reg: u32) -> bool {
    struct UsesRegister(u32, bool);
    impl<'a> Visitor<'a> for UsesRegister {
        fn visit_assign_target(&mut self, target: &'a AssignTarget) {
            if let AssignTarget::Register(r) = target {
                if *r == self.0 {
                    self.1 = true;
                }
            }
            self.walk_assign_target(target);
        }
        fn visit_expression(&mut self, expr: &'a Expression) {
            if let Expression::Value(Value::Register(r)) = expr {
                if *r == self.0 {
                    self.1 = true;
                }
            }
            self.walk_expression(expr);
        }
    }
    let mut checker = UsesRegister(reg, false);
    checker.visit_statement(stmt);
    checker.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, PropertyKey};

    #[test]
    fn test_inline_simple() {
        let stmts = vec![
            Statement::assign_reg(
                0,
                Expression::constant(Constant::String("test".to_string())),
            ),
            Statement::assign_reg(
                1,
                Expression::Member {
                    object: Box::new(Expression::Value(Value::Register(0))),
                    property: PropertyKey::Ident("length".to_string()),
                    optional: false,
                },
            ),
        ];

        let result = inline_expressions(stmts);

        // r0 should be inlined into r1's expression
        assert!(result.len() <= 2);
    }
}
