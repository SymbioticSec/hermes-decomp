// Per-function env-slot analysis and nested Function parent edges.
use std::collections::BTreeMap;
use crate::ir::{Expression, Statement};
use super::super::info::{ClosureInfo, ClosureSlotValue};
use super::helpers::binding_name_is_slot_worthy;
use super::ClosureContext;

impl ClosureContext {
    pub fn analyze_function(&mut self, function_id: u32, stmts: &[Statement]) {
        let mut deferred = Vec::new();
        self.analyze_function_collecting(function_id, stmts, &mut deferred);
        for (from_fn, level, slot, val) in deferred {
            if let Some(target) = self.ancestor_at(from_fn, level) {
                self.function_closures
                    .entry(target)
                    .or_default()
                    .store_slot(slot, val);
            }
        }
    }

    pub(super) fn analyze_function_collecting(
        &mut self,
        function_id: u32,
        stmts: &[Statement],
        deferred: &mut Vec<(u32, u32, u32, ClosureSlotValue)>,
    ) {
        let mut info = ClosureInfo::new();
        let mut register_values: BTreeMap<u32, ClosureSlotValue> = BTreeMap::new();
        // Named locals after register naming: `let require = arg1` then `env[1] = require`.
        let mut named_values: BTreeMap<String, ClosureSlotValue> = BTreeMap::new();

        for stmt in stmts {
            self.analyze_stmt_context(
                function_id,
                stmt,
                &mut info,
                &mut register_values,
                &mut named_values,
                deferred,
            );
        }

        self.function_closures.insert(function_id, info);
    }

    /// Resolve a value for env-slot storage: follow register and named-local aliases
    /// so `env[s] = x` with `x = require` records `require`, not just a temp.
    ///
    /// If the RHS is already a *meaningful* local name (`require`, `HTTP`, …), keep
    /// it — even when that local was initialized from `arg1` (the name is the signal
    /// we want in the slot map; Metro roles handle remaining `argN` later).
    pub(super) fn resolve_store_value(
        value: &Expression,
        reg_values: &BTreeMap<u32, ClosureSlotValue>,
        named_values: &BTreeMap<String, ClosureSlotValue>,
    ) -> Option<ClosureSlotValue> {
        let val = super::super::info::value_from_expr(value, Some(reg_values), true)?;
        match &val {
            ClosureSlotValue::Variable(n) if binding_name_is_slot_worthy(n) => Some(val),
            ClosureSlotValue::Variable(n) => {
                // Ephemeral/generic alias: one hop to the bound origin when present.
                if let Some(origin) = named_values.get(n) {
                    return Some(origin.clone());
                }
                Some(val)
            }
            _ => Some(val),
        }
    }

}
