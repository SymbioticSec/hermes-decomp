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

#[derive(Debug, Clone, Default)]
pub struct EnvRegMap {
    /// register → environment nesting level (0 = current function)
    reg_level: BTreeMap<u32, u32>,
}

impl EnvRegMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `reg` now holds the environment at nesting `level`.
    pub fn set_level(&mut self, reg: u32, level: u32) {
        self.reg_level.insert(reg, level);
    }

    /// Level for an env register, defaulting to 0 (current) when unknown.
    /// Unknown is common for Mov/phi-like paths; level 0 is the conservative
    /// historical behaviour.
    pub fn level_of(&self, reg: u32) -> u32 {
        self.reg_level.get(&reg).copied().unwrap_or(0)
    }

    /// When `dst = src` (Mov), propagate env-level knowledge if `src` is known.
    pub fn copy_reg(&mut self, dst: u32, src: u32) {
        if let Some(&lvl) = self.reg_level.get(&src) {
            self.reg_level.insert(dst, lvl);
        } else {
            self.reg_level.remove(&dst);
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
