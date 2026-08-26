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

use std::collections::BTreeMap;

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
}

impl EnvRegMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `reg` now holds the environment at nesting `level`. An
    /// environment reached by walking the parent chain is not one loaded out of a
    /// slot, so any earlier provenance for this register is dropped.
    pub fn set_level(&mut self, reg: u32, level: u32) {
        self.reg_level.insert(reg, level);
        self.reg_source_slot.remove(&reg);
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

    #[test]
    fn copy_propagates_level() {
        let mut m = EnvRegMap::new();
        m.set_level(3, 1);
        m.copy_reg(5, 3);
        assert_eq!(m.level_of(5), 1);
        m.copy_reg(5, 7); // src unknown → clear
        assert_eq!(m.level_of(5), 0);
        assert!(!m.reg_level.contains_key(&5));
    }
}
