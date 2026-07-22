// Reserved for future constant propagation improvement, not yet used in the pipeline.

use crate::ir::{AssignTarget, BlockId, Statement, CFG};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DefSite {
    pub block: BlockId,
    pub stmt_index: usize,
    pub register: u32,
}

// Reaching Definitions analysis determines which assignments (definitions) might reach a given point.
// A definition `d` reaches point `p` if there is a path from `d` to `p` where `d` is not "killed" (overwritten).
//
// Use case: Constant Propagation, Copy Propagation.
// If only one definition reaches a use, and that definition is a constant, we can inline it.
#[derive(Debug)]
pub struct ReachingDefs {
    pub reaching_in: BTreeMap<BlockId, HashSet<DefSite>>,
    pub reaching_out: BTreeMap<BlockId, HashSet<DefSite>>,
}

impl ReachingDefs {
    pub fn analyze(cfg: &CFG) -> Self {
        let mut reaching_in: BTreeMap<BlockId, HashSet<DefSite>> = BTreeMap::new();
        let mut reaching_out: BTreeMap<BlockId, HashSet<DefSite>> = BTreeMap::new();

        for id in cfg.block_ids() {
            reaching_in.insert(id, HashSet::new());
            reaching_out.insert(id, compute_gen(cfg, id));
        }

        // Fixed-point iteration using Reverse Postorder (RPO).
        // IN[B] = U(OUT[P]) for P in predecessors(B)
        // OUT[B] = GEN[B] U (IN[B] - KILL[B])
        // Forward analysis (unlike liveness which is backward).
        let rpo = cfg.reverse_postorder();
        let mut changed = true;

        while changed {
            changed = false;

            for &block_id in &rpo {
                let mut new_in: HashSet<DefSite> = HashSet::new();
                for pred in cfg.predecessors(block_id) {
                    if let Some(pred_out) = reaching_out.get(&pred) {
                        new_in.extend(pred_out);
                    }
                }

                // reaching_out = gen(block) ∪ (reaching_in - kill(block))
                let gen = compute_gen(cfg, block_id);
                let kill = compute_kill(cfg, block_id, &new_in);
                let mut new_out = new_in.clone();
                for k in &kill {
                    new_out.remove(k);
                }
                new_out.extend(&gen);

                if new_in != *reaching_in.get(&block_id).unwrap_or(&HashSet::new()) {
                    changed = true;
                    reaching_in.insert(block_id, new_in);
                }
                if new_out != *reaching_out.get(&block_id).unwrap_or(&HashSet::new()) {
                    changed = true;
                    reaching_out.insert(block_id, new_out);
                }
            }
        }

        ReachingDefs {
            reaching_in,
            reaching_out,
        }
    }

    pub fn defs_for(&self, block: BlockId, reg: u32) -> Vec<DefSite> {
        self.reaching_in
            .get(&block)
            .map(|s| s.iter().filter(|d| d.register == reg).copied().collect())
            .unwrap_or_default()
    }
}

fn compute_gen(cfg: &CFG, block_id: BlockId) -> HashSet<DefSite> {
    let mut gen = HashSet::new();
    if let Some(block) = cfg.get(block_id) {
        for (i, stmt) in block.statements.iter().enumerate() {
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = stmt
            {
                // Remove any earlier def of same register in this block
                gen.retain(|d: &DefSite| d.register != *r);
                gen.insert(DefSite {
                    block: block_id,
                    stmt_index: i,
                    register: *r,
                });
            }
        }
    }
    gen
}

fn compute_kill(cfg: &CFG, block_id: BlockId, reaching: &HashSet<DefSite>) -> HashSet<DefSite> {
    let mut kill = HashSet::new();
    if let Some(block) = cfg.get(block_id) {
        for stmt in &block.statements {
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = stmt
            {
                // Kill all reaching defs of this register from other blocks
                for def in reaching {
                    if def.register == *r && def.block != block_id {
                        kill.insert(*def);
                    }
                }
            }
        }
    }
    kill
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CFGBuilder, Constant, Expression, Value};

    #[test]
    fn test_reaching_defs() {
        let mut builder = CFGBuilder::new();
        builder.emit(Statement::assign_reg(
            0,
            Expression::constant(Constant::Integer(1)),
        ));
        builder.emit_return(Some(Expression::Value(Value::Register(0))));

        let cfg = builder.finish();
        let reaching = ReachingDefs::analyze(&cfg);

        // At entry, no definitions should reach
        assert!(reaching.defs_for(cfg.entry, 0).is_empty());
    }
}
