// Fold chain assignments: `r0 = x; y = r0` -> `y = x`

use crate::ir::{AssignTarget, Expression, Statement, Value, Visitor};
use std::collections::BTreeMap;

// Fold chain assignments. Only when r0 is used exactly once in the whole
// function — folding `r = expr; cv = r` into `cv = expr` removes r's definition,
// so any *other* use (e.g. a later `return r`) would otherwise be left dangling.
pub(super) fn fold_chain_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut use_counts = BTreeMap::new();
    let mut counter = RegUseCounter { counts: &mut use_counts };
    for stmt in &stmts {
        counter.visit_statement(stmt);
    }
    fold_with_counts(stmts, &use_counts)
}

fn fold_with_counts(stmts: Vec<Statement>, use_counts: &BTreeMap<u32, usize>) -> Vec<Statement> {
    let mut result = Vec::new();
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        match &stmt {
            Statement::Assign {
                target: AssignTarget::Register(r),
                value,
            } if !value.has_side_effects() => {
                if let Some(Statement::Assign {
                    target: next_target,
                    value: Expression::Value(Value::Register(r2)),
                }) = iter.peek()
                {
                    // Fold only if r is used exactly once (this immediate use); a
                    // later use such as `return r` must not be orphaned.
                    if r == r2
                        && !matches!(next_target, AssignTarget::Register(_))
                        && use_counts.get(r).copied().unwrap_or(0) == 1
                    {
                        let Some(next) = iter.next() else { continue };
                        if let Statement::Assign { target: t, .. } = next {
                            result.push(Statement::Assign {
                                target: t,
                                value: value.clone(),
                            });
                            continue;
                        }
                    }
                }
                result.push(stmt);
            }
            Statement::If { condition, then_body, else_body } => {
                result.push(Statement::If {
                    condition: condition.clone(),
                    then_body: fold_with_counts(then_body.clone(), use_counts),
                    else_body: fold_with_counts(else_body.clone(), use_counts),
                });
            }
            Statement::While { condition, body } => {
                result.push(Statement::While {
                    condition: condition.clone(),
                    body: fold_with_counts(body.clone(), use_counts),
                });
            }
            Statement::Block(inner) => {
                result.push(Statement::Block(fold_with_counts(inner.clone(), use_counts)));
            }
            _ => result.push(stmt),
        }
    }

    result
}

// Tally every value-position register reference (one per occurrence, recursive).
struct RegUseCounter<'a> {
    counts: &'a mut BTreeMap<u32, usize>,
}

impl<'a, 'b> Visitor<'b> for RegUseCounter<'a> {
    fn visit_expression(&mut self, expr: &'b Expression) {
        if let Expression::Value(Value::Register(r)) = expr {
            *self.counts.entry(*r).or_insert(0) += 1;
        }
        self.walk_expression(expr);
    }
}
