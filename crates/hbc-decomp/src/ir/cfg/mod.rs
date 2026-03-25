use std::collections::HashSet;
use super::{BlockId, Statement, Terminator};
use std::collections::BTreeMap;

pub mod dot;
pub use dot::generate_dot;

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}

impl BasicBlock {
    pub fn new(id: BlockId) -> Self {
        BasicBlock {
            id,
            statements: Vec::new(),
            terminator: Terminator::None,
        }
    }

    pub fn push(&mut self, stmt: Statement) {
        self.statements.push(stmt);
    }

    pub fn set_terminator(&mut self, term: Terminator) {
        self.terminator = term;
    }

    pub fn successors(&self) -> Vec<BlockId> {
        self.terminator.successors()
    }
}

#[derive(Debug, Clone)]
pub struct CfgExceptionHandler {
    pub try_block_start: BlockId,
    pub catch_block: BlockId,
}

#[derive(Debug)]
pub struct CFG {
    pub entry: BlockId,
    blocks: BTreeMap<BlockId, BasicBlock>,
    next_id: u32,
    pub exception_handlers: Vec<CfgExceptionHandler>,
    pub offset_to_block: BTreeMap<u32, BlockId>,
}

impl CFG {
    pub fn new() -> Self {
        let entry = BlockId(0);
        let mut blocks = BTreeMap::new();
        blocks.insert(entry, BasicBlock::new(entry));

        CFG {
            entry,
            blocks,
            next_id: 1,
            exception_handlers: Vec::new(),
            offset_to_block: BTreeMap::new(),
        }
    }

    pub fn create_block(&mut self) -> BlockId {
        let id = BlockId(self.next_id);
        self.next_id += 1;
        self.blocks.insert(id, BasicBlock::new(id));
        id
    }

    pub fn get(&self, id: BlockId) -> Option<&BasicBlock> {
        self.blocks.get(&id)
    }

    pub fn get_mut(&mut self, id: BlockId) -> Option<&mut BasicBlock> {
        self.blocks.get_mut(&id)
    }

    pub fn entry_block(&self) -> &BasicBlock {
        self.blocks
            .get(&self.entry)
            .expect("entry block must exist")
    }

    pub fn entry_block_mut(&mut self) -> &mut BasicBlock {
        self.blocks
            .get_mut(&self.entry)
            .expect("entry block must exist")
    }

    pub fn blocks(&self) -> impl Iterator<Item = &BasicBlock> {
        self.blocks.values()
    }

    pub fn blocks_mut(&mut self) -> impl Iterator<Item = &mut BasicBlock> {
        self.blocks.values_mut()
    }

    pub fn blocks_with_ids(&self) -> impl Iterator<Item = (BlockId, &BasicBlock)> {
        self.blocks.iter().map(|(&id, block)| (id, block))
    }

    pub fn block_ids(&self) -> impl Iterator<Item = BlockId> + '_ {
        self.blocks.keys().copied()
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn predecessors(&self, target: BlockId) -> Vec<BlockId> {
        self.blocks
            .values()
            .filter(|b| b.successors().contains(&target))
            .map(|b| b.id)
            .collect()
    }

    pub fn postorder(&self) -> Vec<BlockId> {
        let mut visited = HashSet::new();
        let mut result = Vec::new();
        self.postorder_visit(self.entry, &mut visited, &mut result);
        result
    }

    fn postorder_visit(
        &self,
        block: BlockId,
        visited: &mut HashSet<BlockId>,
        result: &mut Vec<BlockId>,
    ) {
        if !visited.insert(block) {
            return;
        }
        if let Some(b) = self.get(block) {
            for succ in b.successors() {
                self.postorder_visit(succ, visited, result);
            }
        }
        result.push(block);
    }

    pub fn reverse_postorder(&self) -> Vec<BlockId> {
        let mut order = self.postorder();
        order.reverse();
        order
    }
}

impl Default for CFG {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cfg_creation() {
        let cfg = CFG::new();
        assert_eq!(cfg.block_count(), 1);
        assert_eq!(cfg.entry, BlockId(0));
    }

    #[test]
    fn test_create_block() {
        let mut cfg = CFG::new();
        let b1 = cfg.create_block();
        let b2 = cfg.create_block();
        assert_eq!(b1, BlockId(1));
        assert_eq!(b2, BlockId(2));
        assert_eq!(cfg.block_count(), 3);
    }
}
