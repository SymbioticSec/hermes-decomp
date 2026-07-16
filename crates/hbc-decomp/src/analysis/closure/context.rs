use super::info::{encode_level_slot, ClosureInfo, ClosureSlotValue};
use crate::ir::{AssignTarget, Expression, Statement};
use std::collections::{BTreeMap, HashSet};

/// Intermediate SSA temps that must not rename a captured env slot.
fn is_ephemeral_name(name: &str) -> bool {
    if name == "tmp"
        || name
            .strip_prefix("tmp")
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    if name
        .strip_prefix('r')
        .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    matches!(
        name,
        "sum" | "diff" | "product" | "quotient" | "text" | "result" | "value" | "ret"
            | "tmpResult" | "callResult"
    ) || name.ends_with("Result")
        || name.ends_with("Return")
}

fn slot_value_is_stable(val: &ClosureSlotValue) -> bool {
    match val {
        ClosureSlotValue::Constant(_) | ClosureSlotValue::Function { .. } => true,
        ClosureSlotValue::Variable(v) => !is_ephemeral_name(v),
        ClosureSlotValue::RegExp | ClosureSlotValue::Unknown => false,
    }
}

// Maximum iterations for propagating async flags through generator chains.
// Convergence typically occurs in 2-3 iterations; 20 guarantees termination.
const MAX_ASYNC_PROPAGATION_ITERATIONS: usize = 20;

// Global closure context for cross-function resolution.
// Tracks parent-child relationships and environment slot assignments across all functions.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ClosureContext {
    pub parent_function: BTreeMap<u32, u32>,
    pub function_closures: BTreeMap<u32, ClosureInfo>,
    pub function_names: BTreeMap<u32, String>,
    // Set of function IDs that are async (created with CreateAsyncClosure)
    pub async_functions: HashSet<u32>,
    // Set of function IDs that are generators (created with CreateGeneratorClosure)
    pub generator_functions: HashSet<u32>,
}

impl ClosureContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_child(&mut self, parent: u32, child: u32) {
        self.parent_function.insert(child, parent);
    }

    pub fn add_closure_info(&mut self, function_id: u32, info: ClosureInfo) {
        self.function_closures.insert(function_id, info);
    }

    pub fn add_function_name(&mut self, function_id: u32, name: String) {
        self.function_names.insert(function_id, name);
    }

    pub fn update_slot_variable(&mut self, function_id: u32, slot: u32, name: String) {
        if let Some(info) = self.function_closures.get_mut(&function_id) {
            info.slots.insert(slot, ClosureSlotValue::Variable(name));
        }
    }

    pub fn mark_async(&mut self, function_id: u32) {
        self.async_functions.insert(function_id);
    }

    pub fn mark_generator(&mut self, function_id: u32) {
        self.generator_functions.insert(function_id);
    }

    pub fn is_async(&self, function_id: u32) -> bool {
        self.async_functions.contains(&function_id)
    }

    pub fn is_generator(&self, function_id: u32) -> bool {
        self.generator_functions.contains(&function_id)
    }

    // Propagate async flag from outer wrapper to inner generator.
    // In Hermes (via Babel), async functions compile as:
    //   1. An outer wrapper created via CreateGeneratorClosure (marked as generator)
    //   2. An inner generator (CreateGenerator) containing the actual body with yields
    //
    // Heuristic: iteratively mark generators as async if their parent is NOT a generator
    // OR if their parent is already marked as async. This handles the two-level chain:
    //   Metro factory → CreateGeneratorClosure(719) → CreateGenerator(720)
    //   719 gets async (parent is non-generator), then 720 gets async (parent 719 is async).
    //
    // Async is detected explicitly elsewhere: modern bytecode marks it via the
    // `CreateAsyncClosure` opcode, and the legacy Babel `_asyncToGenerator(
    // function*(){})` pattern is recognised by `detect_async_generator_wrappers`.
    // Here we only PROPAGATE that flag from an async wrapper to the inner
    // generator body it drives. We must NOT guess "async" from the parent merely
    // not being a generator, a real `function*` also has a non-generator parent,
    // and that guess rendered real generators as `async`/`await`.
    pub fn propagate_async_to_generators(&mut self) {
        // Iterate until no more changes (handles multi-level chains)
        for _ in 0..MAX_ASYNC_PROPAGATION_ITERATIONS {
            let async_generators: Vec<u32> = self
                .generator_functions
                .iter()
                .filter(|&&func_id| {
                    if self.async_functions.contains(&func_id) {
                        return false; // already marked
                    }
                    // Inner body of an async wrapper: parent is async.
                    matches!(self.parent_function.get(&func_id), Some(&parent) if self.async_functions.contains(&parent))
                })
                .copied()
                .collect();

            if async_generators.is_empty() {
                break;
            }
            for func_id in async_generators {
                self.async_functions.insert(func_id);
            }
        }
    }

    // Looks up the parent chain to find closure slot assignments.
    // Supports multi-level closure resolution for deep nesting.
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

    pub fn analyze_function(&mut self, function_id: u32, stmts: &[Statement]) {
        let mut info = ClosureInfo::new();
        let mut register_values: BTreeMap<u32, ClosureSlotValue> = BTreeMap::new();

        for stmt in stmts {
            self.analyze_stmt_context(function_id, stmt, &mut info, &mut register_values);
        }

        self.function_closures.insert(function_id, info);
    }

    fn analyze_stmt_context(
        &mut self,
        parent_fn: u32,
        stmt: &Statement,
        info: &mut ClosureInfo,
        reg_values: &mut BTreeMap<u32, ClosureSlotValue>,
    ) {
        match stmt {
            Statement::Assign { target, value } => {
                if let Expression::Function {
                    id,
                    name,
                    is_async,
                    is_generator,
                    ..
                } = value
                {
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

                    if let AssignTarget::Register(r) = target {
                        reg_values.insert(
                            *r,
                            ClosureSlotValue::Function {
                                id: id.0,
                                name: name.clone(),
                            },
                        );
                    }
                }

                if let AssignTarget::Register(r) = target {
                    if let Some(val) = super::info::value_from_expr(value, None, true) {
                        reg_values.insert(*r, val);
                    }
                }

                if let AssignTarget::ClosureVar { slot, level } = target {
                    if *level == 0 {
                        if let Some(val) = super::info::value_from_expr(value, Some(reg_values), true) {
                            info.store_slot(*slot, val);
                        }
                    }
                }
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                for s in then_body {
                    self.analyze_stmt_context(parent_fn, s, info, reg_values);
                }
                for s in else_body {
                    self.analyze_stmt_context(parent_fn, s, info, reg_values);
                }
            }
            Statement::While { body, .. } | Statement::For { body, .. } => {
                for s in body {
                    self.analyze_stmt_context(parent_fn, s, info, reg_values);
                }
            }
            Statement::Block(inner) => {
                for s in inner {
                    self.analyze_stmt_context(parent_fn, s, info, reg_values);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                for s in try_body {
                    self.analyze_stmt_context(parent_fn, s, info, reg_values);
                }
                for s in catch_body {
                    self.analyze_stmt_context(parent_fn, s, info, reg_values);
                }
                for s in finally_body {
                    self.analyze_stmt_context(parent_fn, s, info, reg_values);
                }
            }
            _ => {}
        }
    }

}
