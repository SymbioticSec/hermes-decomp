use crate::ir::{AssignTarget, Statement};
use std::collections::BTreeMap;
use super::types::{ClosureInfo, ClosureSlotValue};
use super::value::value_from_expr;

impl ClosureInfo {
    pub fn analyze(stmts: &[Statement]) -> Self {
        let mut info = Self::new();
        let mut register_values: BTreeMap<u32, ClosureSlotValue> = BTreeMap::new();

        for stmt in stmts {
            info.analyze_stmt(stmt, &mut register_values);
        }

        info
    }

    fn analyze_stmt(&mut self, stmt: &Statement, reg_values: &mut BTreeMap<u32, ClosureSlotValue>) {
        match stmt {
            Statement::Assign { target, value } => {
                if let AssignTarget::Register(r) = target {
                    // Use reg_values so copies like `r5 = require` still track.
                    if let Some(val) = value_from_expr(value, Some(reg_values), true) {
                        reg_values.insert(*r, val);
                    }
                }

                if let AssignTarget::ClosureVar { slot, .. } = target {
                    if let Some(val) = value_from_expr(value, Some(reg_values), true) {
                        self.store_slot(*slot, val);
                    }
                }
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                for s in then_body {
                    self.analyze_stmt(s, reg_values);
                }
                for s in else_body {
                    self.analyze_stmt(s, reg_values);
                }
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::For { body, .. }
            | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. }
            | Statement::Block(body) => {
                for s in body {
                    self.analyze_stmt(s, reg_values);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                for s in try_body {
                    self.analyze_stmt(s, reg_values);
                }
                for s in catch_body {
                    self.analyze_stmt(s, reg_values);
                }
                for s in finally_body {
                    self.analyze_stmt(s, reg_values);
                }
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    for s in body {
                        self.analyze_stmt(s, reg_values);
                    }
                }
                if let Some(d) = default {
                    for s in d {
                        self.analyze_stmt(s, reg_values);
                    }
                }
            }
            _ => {}
        }
    }
}
