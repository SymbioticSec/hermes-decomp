// Global closure context for cross-function resolution.
// Tracks parent-child relationships and environment slot assignments across all functions.

mod analyze;
mod async_prop;
mod helpers;
mod merge;
mod walk;

use super::info::{ClosureInfo, ClosureSlotValue};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ClosureContext {
    pub parent_function: BTreeMap<u32, u32>,
    pub function_closures: BTreeMap<u32, ClosureInfo>,
    pub function_names: BTreeMap<u32, String>,
    // Set of function IDs that are async (created with CreateAsyncClosure)
    pub async_functions: HashSet<u32>,
    // Set of function IDs that are generators (created with CreateGeneratorClosure)
    pub generator_functions: HashSet<u32>,
    // A block environment a function built on top of its own (a class
    // binding's scope) is a scope of its own between that function and the
    // closures created inside the block. It is given a synthetic id so it
    // takes part in the parent chain like a function would. Keyed by
    // (creating function, builder level), value: the synthetic id.
    #[serde(default)]
    pub block_scopes: BTreeMap<(u32, u32), u32>,
    // synthetic id -> the builder level its slots are addressed with inside
    // the creating function.
    #[serde(default)]
    pub block_level: BTreeMap<u32, u32>,
    // function -> (baked capture name -> (level, slot) it came from). A
    // capture whose ancestor slot had no name when it was baked prints as
    // `closure_N`, which says nothing about the level; the late inherit pass
    // resolves it from this record once the owner has named the slot.
    #[serde(default)]
    pub baked_captures: BTreeMap<u32, BTreeMap<String, (u32, u32)>>,
    // child -> (creating function, builder level of the block environment it
    // was created with). Such a child's parent is that block scope.
    #[serde(default)]
    pub creation_scopes: BTreeMap<u32, (u32, u32)>,
}

// Synthetic ids count down from the top of the range; a bundle has far fewer
// than two billion functions, so they never meet a real id.
const FIRST_BLOCK_SCOPE_ID: u32 = u32::MAX - 1;

impl ClosureContext {
    pub fn new() -> Self {
        Self::default()
    }

    // The synthetic scope for the block environment `level` that `parent`
    // built, created on first use. The scope's parent is the function.
    pub fn block_scope(&mut self, parent: u32, level: u32) -> u32 {
        if let Some(&id) = self.block_scopes.get(&(parent, level)) {
            return id;
        }
        let id = FIRST_BLOCK_SCOPE_ID - self.block_scopes.len() as u32;
        self.block_scopes.insert((parent, level), id);
        self.block_level.insert(id, level);
        self.parent_function.insert(id, parent);
        id
    }

    // The scope that owns slot writes `func` makes at `level`: `func` itself
    // at level 0, the block environment it built at a builder level past
    // `NESTED_ENV_LEVEL_BASE`, otherwise the ancestor that many hops up.
    pub fn slot_owner(&self, func: u32, level: u32) -> Option<u32> {
        if level == 0 {
            return Some(func);
        }
        if level >= crate::ir::NESTED_ENV_LEVEL_BASE {
            return self.block_scopes.get(&(func, level)).copied();
        }
        self.ancestor_at(func, level)
    }

    pub fn is_block_scope(&self, id: u32) -> bool {
        self.block_level.contains_key(&id)
    }

    // The function whose body a scope id belongs to: the function itself, or
    // the function that built a block scope.
    pub fn scope_function(&self, id: u32) -> u32 {
        if self.is_block_scope(id) {
            self.parent_function.get(&id).copied().unwrap_or(id)
        } else {
            id
        }
    }

    // Names of the functions declared directly in the body of `scope` (block
    // scopes included): the identifiers a `class X` or `function X` binds at
    // that level, which a variable declared there cannot share.
    pub fn declared_function_names(&self, scope: u32) -> HashSet<String> {
        let owner = self.scope_function(scope);
        self.parent_function
            .iter()
            .filter(|(child, parent)| {
                !self.is_block_scope(**child) && self.scope_function(**parent) == owner
            })
            .filter_map(|(child, _)| self.function_names.get(child).cloned())
            .collect()
    }

    pub fn add_child(&mut self, parent: u32, child: u32) {
        // hermesc can create a closure of a function inside that very
        // function (a chainable API whose methods rebuild the chain). A
        // function is not its own parent; recording it as such made every
        // walk up the chain stop there and left the function without a module.
        if parent == child {
            return;
        }
        // Created with a block environment of `parent`: the block scope is the
        // parent, its slots are what the child reads at level 1.
        if let Some(&(creator, level)) = self.creation_scopes.get(&child) {
            if creator == parent {
                let scope = self.block_scope(parent, level);
                self.parent_function.insert(child, scope);
                return;
            }
        }
        self.parent_function.insert(child, parent);
    }

    pub fn set_creation_scope(&mut self, child: u32, creator: u32, level: u32) {
        self.creation_scopes.insert(child, (creator, level));
    }

    pub fn add_closure_info(&mut self, function_id: u32, info: ClosureInfo) {
        self.function_closures.insert(function_id, info);
    }

    pub fn add_function_name(&mut self, function_id: u32, name: String) {
        self.function_names.insert(function_id, name);
    }

    pub fn update_slot_variable(&mut self, function_id: u32, slot: u32, name: String) {
        if let Some(info) = self.function_closures.get_mut(&function_id) {
            if log::log_enabled!(target: "slotname", log::Level::Trace) {
                log::trace!(
                    target: "slotname",
                    "{function_id}:{slot} {:?} -> {name}",
                    info.slots.get(&slot)
                );
            }
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
}

#[cfg(test)]
mod block_scope_tests {
    use super::ClosureContext;
    use crate::analysis::closure::info::{encode_level_slot, ClosureSlotValue};
    use crate::ir::{
        AssignTarget, Binding, Expression, FunctionId, Statement, Value, NESTED_ENV_LEVEL_BASE,
    };

    #[test]
    fn a_closure_stored_in_a_block_scope_reads_that_scope_at_level_one() {
        let block = NESTED_ENV_LEVEL_BASE + 1;
        // Factory 1: slot 0 of its own env holds `global`; a block env holds the class.
        let factory = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::ClosureVar { level: 0, slot: 0 }),
                value: Expression::Value(Value::Binding(Binding::Variable("global".into()))),
            },
            Statement::Assign {
                target: AssignTarget::Binding(Binding::ClosureVar {
                    level: block,
                    slot: 0,
                }),
                value: Expression::Function {
                    id: FunctionId(7),
                    name: Some("MessageQueue".into()),
                    is_arrow: false,
                    is_async: false,
                    is_generator: false,
                },
            },
        ];
        let mut ctx = ClosureContext::new();
        ctx.analyze_function(1, &factory);
        ctx.analyze_function(7, &[]);

        // The constructor's parent is the block, whose parent is the factory.
        let scope = ctx.parent_function[&7];
        assert!(ctx.is_block_scope(scope));
        assert_eq!(ctx.parent_function[&scope], 1);

        // Inside the constructor, level 1 slot 0 is the class, level 2 slot 0 is `global`.
        let info = ctx.get_closure_info_for(7);
        assert!(matches!(
            info.slots.get(&encode_level_slot(1, 0)),
            Some(ClosureSlotValue::Function { id: 7, .. })
        ));
        assert!(matches!(
            info.slots.get(&encode_level_slot(2, 0)),
            Some(ClosureSlotValue::Variable(v)) if v == "global"
        ));

        // Inside the factory, the block store resolves under the builder level.
        let own = ctx.get_closure_info_for(1);
        assert_eq!(
            own.get_slot_name(encode_level_slot(block, 0)),
            "MessageQueue"
        );
        assert_eq!(own.get_slot_name(0), "global");
    }
}

#[cfg(test)]
mod constant_name_tests {
    use super::ClosureContext;
    use crate::analysis::closure::info::{ClosureInfo, ClosureSlotValue};

    #[test]
    fn a_string_slot_does_not_take_the_name_of_a_function() {
        let mut ctx = ClosureContext::new();
        ctx.add_function_name(7, "GuildSelector".into());
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Constant("\"GuildSelector\"".into()));
        info.store_slot(1, ClosureSlotValue::Constant("\"ADMINISTRATOR\"".into()));
        ctx.add_closure_info(1, info);
        ctx.enrich_function_slot_names();
        let info = ctx.get_closure_info_for(1);
        assert_eq!(info.get_slot_name(0), "GuildSelector_str");
        assert_eq!(info.get_slot_name(1), "ADMINISTRATOR");
    }
}
