// Tracks which environment-nesting *level* each register currently holds.
//
// Hermes bytecode:
//   CreateEnvironment r0          → r0 = current function env (level 0)
//   GetEnvironment r0, N          → r0 = env N levels up (0 = current)
//   LoadFromEnvironment rD, rE, S → load slot S from the env in rE
//   StoreToEnvironment rE, S, rV  → store into slot S of the env in rE
//
// We lower Load/Store to `ClosureVar { level, slot }` so closure resolution can
// distinguish parent captures from local env slots that share the same index.

use std::collections::{BTreeMap, HashSet};

// Levels from this value up denote a nested environment, one that was loaded out
// of a slot rather than reached by walking up the parent chain. The level encodes
// which slot it came from, so two different nested environments never share one.
// `encode_level_slot` keeps 8 bits of level and real nesting is a handful deep,
// so the upper half of the range is free.
pub const NESTED_ENV_LEVEL_BASE: u32 = 128;
const MAX_LEVEL: u32 = 255;

#[derive(Debug, Clone, Default)]
pub struct EnvRegMap {
    /// register → environment nesting level (0 = current function)
    reg_level: BTreeMap<u32, u32>,
    /// register → the (level, slot) it was loaded from, for a register that later
    /// turns out to hold an environment
    reg_source_slot: BTreeMap<u32, (u32, u32)>,
    /// The function never creates the environment it runs in, so what
    /// `GetParentEnvironment N` hands back is the Nth ancestor of the ENCLOSING
    /// function, one hop further out than the IR level contract assumes.
    borrows_current_env: bool,
    /// registers holding an environment this function just created
    created_envs: HashSet<u32>,
    /// Whether the environment this function runs in has already been created.
    /// A second `CreateFunctionEnvironment` in the same function does not rebuild
    /// that environment, it builds a separate small one for a closure being made.
    own_env_created: bool,
    /// How many separate environments this function has built past its own, used
    /// to give each one a distinct level so their slots stay apart.
    extra_env_count: u32,
}

impl EnvRegMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare that this function does not create the environment it runs in.
    /// Set once, before any instruction is dispatched.
    pub fn set_borrows_current_env(&mut self, borrows: bool) {
        self.borrows_current_env = borrows;
    }

    /// The IR level a `GetParentEnvironment N` denotes. The IR contract is
    /// `level 0 = this function's environment, 1 = its parent, ...`. A function
    /// that creates no environment of its own runs in the enclosing one, so its
    /// `GetParentEnvironment 0` already IS the parent and every level shifts out
    /// by one. Without the shift a generator body read its grandparent's slots
    /// against its parent, and the SecureStore key `pilote_jwt` held by the
    /// factory two hops up resolved to the state label sitting at the same slot
    /// index one hop up.
    pub fn parent_env_level(&self, operand_level: u32) -> u32 {
        operand_level + u32::from(self.borrows_current_env)
    }

    /// Register `reg` now holds the environment at nesting `level`. An
    /// environment reached by walking the parent chain is not one loaded out of a
    /// slot, so any earlier provenance for this register is dropped.
    pub fn set_level(&mut self, reg: u32, level: u32) {
        self.reg_level.insert(reg, level);
        self.reg_source_slot.remove(&reg);
        self.created_envs.remove(&reg);
    }

    /// `reg` holds an environment this function just created.
    /// Claim `reg` as the environment this function runs in, or, when that has
    /// already been claimed, as a separate environment built for a closure.
    /// Returns the level to give it: 0 for the running environment, a distinct
    /// level past `NESTED_ENV_LEVEL_BASE` for each separate one.
    ///
    /// Hermes emits one `CreateFunctionEnvironment` for the function itself and
    /// one more per closure needing its own scope, all writing slot 0, slot 1 and
    /// so on. Reading them at a single level merges scopes that share nothing: a
    /// factory whose own slots start at 3 was given a slot 0 by a two slot child
    /// environment, so captures of slot 0 inherited that unrelated value's name.
    pub fn claim_function_env(&mut self, reg: u32) -> u32 {
        if !self.own_env_created {
            self.own_env_created = true;
            self.set_level(reg, 0);
            return 0;
        }
        self.extra_env_count += 1;
        let level = (NESTED_ENV_LEVEL_BASE + self.extra_env_count).min(MAX_LEVEL);
        self.set_level(reg, level);
        self.created_envs.insert(reg);
        level
    }

    pub fn mark_created_env(&mut self, reg: u32) {
        self.created_envs.insert(reg);
    }

    pub fn is_created_env(&self, reg: u32) -> bool {
        self.created_envs.contains(&reg)
    }

    /// Level for an env register, defaulting to 0 (current) when unknown.
    /// Unknown is common for Mov/phi-like paths; level 0 is the conservative
    /// historical behaviour.
    pub fn level_of(&self, reg: u32) -> u32 {
        self.reg_level.get(&reg).copied().unwrap_or(0)
    }

    /// Record that `reg` received the value of slot `slot` of the environment at
    /// `level`. If that value turns out to be an environment itself, this is what
    /// tells the two apart.
    pub fn set_source_slot(&mut self, reg: u32, level: u32, slot: u32) {
        self.reg_source_slot.insert(reg, (level, slot));
    }

    /// The level to address `reg` with when it is used as an environment.
    ///
    /// A register loaded out of a slot holds a nested environment: Hermes parks an
    /// inner scope there and indexes it directly. Falling back to level 0 made its
    /// slots share a name with the current environment's slots, so a login token
    /// written to the inner slot 1 was read back under the name of the outer slot
    /// 1 and the output claimed `setJwt(password)`.
    pub fn env_level_of(&self, reg: u32) -> u32 {
        if let Some(&(level, slot)) = self.reg_source_slot.get(&reg) {
            if level < NESTED_ENV_LEVEL_BASE {
                return (NESTED_ENV_LEVEL_BASE + slot).min(MAX_LEVEL);
            }
        }
        self.level_of(reg)
    }

    /// When `dst = src` (Mov), propagate env-level knowledge if `src` is known.
    pub fn copy_reg(&mut self, dst: u32, src: u32) {
        if let Some(&lvl) = self.reg_level.get(&src) {
            self.reg_level.insert(dst, lvl);
        } else {
            self.reg_level.remove(&dst);
        }
        match self.reg_source_slot.get(&src).copied() {
            Some(v) => {
                self.reg_source_slot.insert(dst, v);
            }
            None => {
                self.reg_source_slot.remove(&dst);
            }
        }
        if self.created_envs.contains(&src) {
            self.created_envs.insert(dst);
        } else {
            self.created_envs.remove(&dst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_levels() {
        let mut m = EnvRegMap::new();
        m.set_level(0, 0); // CreateEnvironment r0
        m.set_level(1, 2); // GetEnvironment r1, 2
        assert_eq!(m.level_of(0), 0);
        assert_eq!(m.level_of(1), 2);
        assert_eq!(m.level_of(99), 0); // unknown → current
    }

    // A function that creates the environment it runs in keeps the plain IR
    // contract: GetParentEnvironment N is the Nth ancestor.
    #[test]
    fn owning_function_keeps_parent_levels() {
        let mut m = EnvRegMap::new();
        m.set_borrows_current_env(false);
        assert_eq!(m.parent_env_level(0), 0); // own environment
        assert_eq!(m.parent_env_level(1), 1); // direct parent
    }

    // A function with no environment of its own runs in the enclosing one, so
    // every GetParentEnvironment level shifts out by one. This is the v98
    // generator body: GetParentEnvironment 1 is the grandparent, which is where
    // the factory keeps its constants (the SecureStore key).
    #[test]
    fn borrowing_function_shifts_parent_levels() {
        let mut m = EnvRegMap::new();
        m.set_borrows_current_env(true);
        assert_eq!(m.parent_env_level(0), 1); // already the parent
        assert_eq!(m.parent_env_level(1), 2); // grandparent, not the parent
    }

    // An environment created here and then stored into a slot of the enclosing
    // one takes that slot's identity, which is the level a load from the same
    // slot produces. Builder and capturing closure then agree on the name.
    #[test]
    fn captured_created_env_takes_the_capture_slot_identity() {
        let mut m = EnvRegMap::new();
        m.set_level(20, 0);
        m.mark_created_env(20);
        assert!(m.is_created_env(20));

        // StoreToEnvironment r1(level 0), slot 3, r20
        m.set_source_slot(20, 0, 3);
        let created = m.env_level_of(20);

        // LoadFromEnvironment rX, r1, 3 in the capturing closure
        let mut child = EnvRegMap::new();
        child.set_source_slot(7, 0, 3);
        assert_eq!(created, child.env_level_of(7));
        assert_eq!(created, NESTED_ENV_LEVEL_BASE + 3);
    }

    // Reaching an environment through the parent chain is not reaching one out of
    // a slot: the provenance must not survive.
    #[test]
    fn walking_the_parent_chain_clears_slot_provenance() {
        let mut m = EnvRegMap::new();
        m.set_source_slot(4, 0, 2);
        assert_eq!(m.env_level_of(4), NESTED_ENV_LEVEL_BASE + 2);
        m.set_level(4, 1);
        assert_eq!(m.env_level_of(4), 1);
    }

    #[test]
    fn copy_propagates_level() {
        let mut m = EnvRegMap::new();
        m.set_level(3, 1);
        m.copy_reg(5, 3);
        assert_eq!(m.level_of(5), 1);
        m.copy_reg(5, 7); // src unknown → clear
        assert_eq!(m.level_of(5), 0);
        assert!(!m.reg_level.contains_key(&5));

        // A Mov of a created environment carries that fact to the destination.
        m.mark_created_env(3);
        m.copy_reg(9, 3);
        assert!(m.is_created_env(9));
        m.copy_reg(9, 7); // src is not a created env → clear
        assert!(!m.is_created_env(9));
    }
}
