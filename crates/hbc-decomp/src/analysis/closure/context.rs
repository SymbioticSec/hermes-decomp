use super::info::{encode_level_slot, ClosureInfo, ClosureSlotValue};
use crate::ir::{AssignTarget, Expression, Statement};
use std::collections::{BTreeMap, HashSet};

// Maximum iterations for propagating async flags through generator chains.
// Convergence typically occurs in 2-3 iterations; 20 guarantees termination.
const MAX_ASYNC_PROPAGATION_ITERATIONS: usize = 20;

// Global closure context for cross-function resolution.
// Tracks parent-child relationships and environment slot assignments across all functions.
#[derive(Debug, Clone, Default)]
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
                    if let Some(&parent) = self.parent_function.get(&func_id) {
                        // If parent is already async, this is an async body
                        if self.async_functions.contains(&parent) {
                            return true;
                        }
                        // If parent is NOT a generator, this generator is likely
                        // an async function body (Babel async-to-generator pattern)
                        if !self.generator_functions.contains(&parent) {
                            return true;
                        }
                    }
                    false
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

        // 0. Include local slots (level 0)
        if let Some(local_info) = self.function_closures.get(&function_id) {
            for (slot, value) in &local_info.slots {
                combined.slots.insert(*slot, value.clone());
            }
        }

        // For each ancestor level, copy their closure slots
        // Level 0 = direct parent
        for (level, &ancestor) in ancestors.iter().enumerate() {
            if let Some(ancestor_info) = self.function_closures.get(&ancestor) {
                for (&slot, value) in &ancestor_info.slots {
                    // Store with the level info so we can resolve ClosureVar { level, slot }
                    let ir_level = level as u32;
                    let key = encode_level_slot(ir_level, slot);
                    combined.slots.entry(key).or_insert_with(|| value.clone());
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
                            info.slots.insert(*slot, val);
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
