// Advanced cleanup transformations.
//
// This module provides more aggressive cleanup passes to improve code quality:
// 1. **Redundant Assignment Elimination**: `x = y; x = z;` -> `x = z;` (if `x` unused in `z`).
// 2. **Inline Single-Use Temporaries**: `t = expr; use(t);` -> `use(expr);` (avoids clutter).
// 3. **Dead Assignment Elimination**: `x = expr;` where `x` is never read -> remove (if no side effects).
//
// Refactored to use Visitor pattern.

use crate::ir::{is_simple_value, AssignTarget, Expression, MutVisitor, Statement, Value, Visitor};
use std::collections::{BTreeMap, HashSet};

// Apply advanced cleanup transformations.
pub fn cleanup_advanced(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut stmts = stmts;

    // Pass 1: Remove redundant consecutive assignments to same register
    remove_redundant_assignments(&mut stmts);

    // Pass 2: Inline single-use temporaries
    inline_single_use(&mut stmts);

    // Pass 3: Remove dead assignments (assigned but never read)
    remove_dead_assignments(&mut stmts);

    stmts
}

// Remove redundant assignments where same target is assigned multiple times
// without the value being used in between.
fn remove_redundant_assignments(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i < stmts.len() {
        // Look for pattern: r = x; r = y; (where r is not used in y)
        if i + 1 < stmts.len() {
            if let (
                Statement::Assign {
                    target: t1,
                    value: _,
                },
                Statement::Assign {
                    target: t2,
                    value: v2,
                },
            ) = (&stmts[i], &stmts[i + 1])
            {
                if t1 == t2 && !expr_uses_target(v2, t1) {
                    // The first assignment is dead, remove it
                    stmts.remove(i);
                    continue; // Don't increment, check new position
                }
            }
        }
        i += 1;
    }
}

// Inline temporaries that are only used once.
fn inline_single_use(stmts: &mut Vec<Statement>) {
    // Count uses of each register
    let mut use_count: BTreeMap<u32, usize> = BTreeMap::new();
    let mut def_value: BTreeMap<u32, Expression> = BTreeMap::new();
    let mut def_index: BTreeMap<u32, usize> = BTreeMap::new();

    // First pass: collect definitions and count uses
    {
        let mut counter = UseCounter {
            counts: &mut use_count,
        };
        for (idx, stmt) in stmts.iter().enumerate() {
            match stmt {
                Statement::Assign {
                    target: AssignTarget::Register(r),
                    value,
                } => {
                    def_value.insert(*r, value.clone());
                    def_index.insert(*r, idx);
                    counter.visit_expression(value);
                }
                _ => counter.visit_statement(stmt),
            }
        }
    }

    // Find registers used exactly once and defined with simple expressions
    let mut to_inline: HashSet<u32> = HashSet::new();
    for (reg, count) in &use_count {
        if *count == 1 {
            if let Some(value) = def_value.get(reg) {
                // Only inline simple values (not complex expressions that might have side effects)
                if is_simple_value(value) {
                    to_inline.insert(*reg);
                }
            }
        }
    }

    // Second pass: inline and mark definitions for removal
    let mut to_remove: HashSet<usize> = HashSet::new();
    for (reg, idx) in &def_index {
        if to_inline.contains(reg) {
            to_remove.insert(*idx);
        }
    }

    // Apply inlining
    {
        let mut inliner = Inliner {
            to_inline: &to_inline,
            values: &def_value,
        };
        for stmt in stmts.iter_mut() {
            inliner.visit_statement(stmt);
        }
    }

    // Remove inlined definitions (in reverse order to preserve indices)
    let mut indices: Vec<usize> = to_remove.into_iter().collect();
    indices.sort_by(|a, b| b.cmp(a));
    for idx in indices {
        if idx < stmts.len() {
            stmts.remove(idx);
        }
    }
}

// Remove assignments where the value is never used.
fn remove_dead_assignments(stmts: &mut Vec<Statement>) {
    // Collect all used registers
    let mut used: HashSet<u32> = HashSet::new();
    {
        let mut collector = UseCollector { used: &mut used };
        for stmt in stmts.iter() {
            collector.visit_statement(stmt);
        }
    }

    // Remove assignments to unused registers (but keep side-effectful expressions)
    stmts.retain(|stmt| {
        if let Statement::Assign {
            target: AssignTarget::Register(r),
            value,
        } = stmt
        {
            if !used.contains(r) && !value.has_side_effects() {
                return false;
            }
        }
        true
    });
}

// -- Visitors --

struct UseCounter<'a> {
    counts: &'a mut BTreeMap<u32, usize>,
}

impl<'a> Visitor<'a> for UseCounter<'a> {
    fn visit_expression(&mut self, expr: &'a Expression) {
        if let Expression::Value(Value::Register(r)) = expr {
            *self.counts.entry(*r).or_insert(0) += 1;
        }
        self.walk_expression(expr);
    }
}

struct UseCollector<'a> {
    used: &'a mut HashSet<u32>,
}

impl<'a> Visitor<'a> for UseCollector<'a> {
    fn visit_expression(&mut self, expr: &'a Expression) {
        if let Expression::Value(Value::Register(r)) = expr {
            self.used.insert(*r);
        }
        self.walk_expression(expr);
    }
}

struct Inliner<'a> {
    to_inline: &'a HashSet<u32>,
    values: &'a BTreeMap<u32, Expression>,
}

impl<'a> MutVisitor for Inliner<'a> {
    fn visit_expression(&mut self, expr: &mut Expression) {
        if let Expression::Value(Value::Register(r)) = expr {
            if self.to_inline.contains(r) {
                if let Some(val) = self.values.get(r) {
                    *expr = val.clone();
                    return;
                }
            }
        }
        self.walk_expression(expr);
    }
}

// -- Helpers --

fn expr_uses_target(expr: &Expression, target: &AssignTarget) -> bool {
    let mut checker = TargetUseChecker {
        target,
        found: false,
    };
    checker.visit_expression(expr);
    checker.found
}

struct TargetUseChecker<'a> {
    target: &'a AssignTarget,
    found: bool,
}

impl<'a> Visitor<'a> for TargetUseChecker<'a> {
    fn visit_expression(&mut self, expr: &'a Expression) {
        if self.found {
            return;
        }

        match (expr, self.target) {
            (Expression::Value(Value::Register(r1)), AssignTarget::Register(r2)) if r1 == r2 => {
                self.found = true
            }
            (Expression::Value(Value::Variable(v1)), AssignTarget::Variable(v2)) if v1 == v2 => {
                self.found = true
            }
            _ => self.walk_expression(expr),
        }
    }
}
