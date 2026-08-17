// Merge ancestor slot maps, reanalyze whole-program, IPA/Metro name hooks.
use std::collections::{BTreeMap, HashSet};
use crate::ir::Statement;
use super::super::info::{encode_level_slot, ClosureInfo, ClosureSlotValue};
use super::helpers::{is_ephemeral_name, slot_value_is_stable};
use super::ClosureContext;

impl ClosureContext {
    pub fn get_closure_info_for(&self, function_id: u32) -> ClosureInfo {
        let mut combined = ClosureInfo::new();

        // Build a list of all ancestors (parent, grandparent, etc.)
        // Use visited set to break cycles in parent_function map.
        let mut ancestors = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(function_id);
        let mut current = function_id;
        while let Some(&parent) = self.parent_function.get(&current) {
            if !visited.insert(parent) {
                break;
            }
            ancestors.push(parent);
            current = parent;
        }

        // IR contract (see ir/builder/env_state.rs):
        //   ClosureVar.level 0 = this function's environment
        //   ClosureVar.level 1 = direct parent, 2 = grandparent, …
        // Ancestor depth d maps to IR level d+1. Keys never collide with local
        // level-0 slots that share the same slot *index*.
        for (depth, &ancestor) in ancestors.iter().enumerate() {
            if let Some(ancestor_info) = self.function_closures.get(&ancestor) {
                let ir_level = (depth as u32) + 1;
                for (&slot, value) in &ancestor_info.slots {
                    let key = encode_level_slot(ir_level, slot);
                    // Closer ancestors win if a deeper one already filled the key
                    // (should not happen, each level is unique).
                    combined.slots.entry(key).or_insert_with(|| value.clone());
                }
            }
        }

        // Local env (IR level 0): raw slot keys == encode_level_slot(0, slot).
        // Hermes GetEnvironment(0) in a nested function is often the *captured*
        // parent environment (no local CreateEnvironment). Local analysis may
        // then record only the temp `sum = c0+1; store sum`, renaming the slot
        // to `sum`. Prefer a stable ancestor name for the same raw slot index.
        if let Some(local_info) = self.function_closures.get(&function_id) {
            for (slot, value) in &local_info.slots {
                let key = *slot; // level 0
                let use_local = match value {
                    ClosureSlotValue::Variable(v) if is_ephemeral_name(v) => {
                        // Keep ancestor stable binding if present at any encoded level.
                        !ancestors.iter().any(|anc| {
                            self.function_closures.get(anc).is_some_and(|ai| {
                                ai.slots
                                    .get(slot)
                                    .is_some_and(slot_value_is_stable)
                            })
                        })
                    }
                    _ => true,
                };
                if use_local {
                    combined.slots.insert(key, value.clone());
                }
            }
        }

        // Also: if level-0 key is missing but ancestors have a stable slot, expose
        // it at level 0 so Hermes-level-0 loads of the captured env resolve.
        for (depth, &ancestor) in ancestors.iter().enumerate() {
            if let Some(ancestor_info) = self.function_closures.get(&ancestor) {
                for (&slot, value) in &ancestor_info.slots {
                    if !slot_value_is_stable(value) {
                        continue;
                    }
                    // Hermes: nested fn's env level 0 is often the same object as
                    // the parent's CreateEnvironment (depth 0 ancestor).
                    if depth == 0 {
                        combined
                            .slots
                            .entry(slot)
                            .or_insert_with(|| value.clone());
                    }
                }
            }
        }

        combined
    }

    pub fn resolve_closure_var(
        &self,
        function_id: u32,
        level: u32,
        slot: u32,
    ) -> Option<ClosureSlotValue> {
        // Walk up the parent chain to the appropriate level.
        // Break on cycles to avoid infinite loops.
        let mut current = function_id;
        let mut visited = std::collections::HashSet::new();
        visited.insert(current);
        for _ in 0..=level {
            let parent = *self.parent_function.get(&current)?;
            if !visited.insert(parent) {
                return None;
            }
            current = parent;
        }

        self.function_closures
            .get(&current)?
            .slots
            .get(&slot)
            .cloned()
    }

    // For each function, if its closure slots store generic `argN` names,
    // replace them with the IPA-inferred names from the same function.
    pub fn update_with_ipa_names(&mut self, param_names: &BTreeMap<u32, Vec<Option<String>>>) {
        for (&func_id, info) in self.function_closures.iter_mut() {
            if let Some(names) = param_names.get(&func_id) {
                info.update_with_param_names(names);
            }
        }
    }

    /// Apply Metro factory param role names (`arg1`→`require`, …) only to
    /// functions that are actual Metro factories (`is_factory`).
    ///
    /// Must not be applied to arbitrary functions: their `argN` are normal
    /// parameters, not Metro roles (see Babel helpers mislabeled as `require`).
    pub fn apply_metro_factory_param_roles(&mut self, is_factory: impl Fn(u32) -> bool) {
        for (&func_id, info) in self.function_closures.iter_mut() {
            if is_factory(func_id) {
                info.apply_metro_param_roles();
            }
        }
    }

    pub fn get_function_name(&self, function_id: u32) -> Option<&str> {
        self.function_names.get(&function_id).map(|s| s.as_str())
    }

    /// Walk `level` hops up the parent chain from `from_fn`.
    /// level 1 → direct parent, level 2 → grandparent, …
    pub fn ancestor_at(&self, from_fn: u32, level: u32) -> Option<u32> {
        if level == 0 {
            return Some(from_fn);
        }
        let mut current = from_fn;
        let mut visited = HashSet::new();
        visited.insert(current);
        for _ in 0..level {
            let parent = *self.parent_function.get(&current)?;
            if !visited.insert(parent) {
                return None;
            }
            current = parent;
        }
        Some(current)
    }

    /// Re-scan all function IR to refresh slot maps and parent edges.
    ///
    /// Call after semantic naming / IPA so stores capture meaningful variable
    /// names, not only pre-naming `argN`/`tmp*`. Rebuilds `function_closures`
    /// while merging parent edges. Applies deferred level≥1 stores onto the
    /// correct ancestor (nested body writing into a parent env).
    pub fn reanalyze_all(&mut self, all_ir: &BTreeMap<u32, Vec<Statement>>) {
        self.function_closures.clear();
        let mut deferred: Vec<(u32, u32, u32, ClosureSlotValue)> = Vec::new();
        let mut keys: Vec<u32> = all_ir.keys().copied().collect();
        keys.sort();
        for fid in keys {
            let Some(stmts) = all_ir.get(&fid) else {
                continue;
            };
            self.analyze_function_collecting(fid, stmts, &mut deferred);
        }
        // Second pass: ancestor links and function names are complete.
        for (from_fn, level, slot, val) in deferred {
            if let Some(target) = self.ancestor_at(from_fn, level) {
                self.function_closures
                    .entry(target)
                    .or_default()
                    .store_slot(slot, val);
            }
        }
        self.enrich_function_slot_names();
    }

    /// Fill `Function { id, name: None }` slots with resolved `function_names`.
    pub fn enrich_function_slot_names(&mut self) {
        let names = self.function_names.clone();
        for info in self.function_closures.values_mut() {
            for value in info.slots.values_mut() {
                if let ClosureSlotValue::Function { id, name } = value {
                    if name.is_none() {
                        if let Some(n) = names.get(id) {
                            *name = Some(n.clone());
                        }
                    }
                }
            }
        }
    }
}
