mod dispatch;
pub(crate) mod generator_cfg;
mod ir_builder;
mod jump_analysis;
mod opcodes_arith;
mod opcodes_call;
mod opcodes_environment;
mod opcodes_flow;
mod opcodes_generator;
mod opcodes_load;
mod opcodes_obj;
mod opcodes_prop;
mod opcodes_switch;

pub use ir_builder::{IRBuilder, IRBuilderOptions};
pub use opcodes_obj::EXTENDS_MARKER;

use super::{BlockId, Expression, Statement, Terminator, CFG};
use std::collections::BTreeMap;

pub struct CFGBuilder {
    cfg: CFG,
    current_block: BlockId,
    offset_to_block: BTreeMap<u32, BlockId>,
}

impl CFGBuilder {
    pub fn new() -> Self {
        let cfg = CFG::new();
        let current = cfg.entry;
        CFGBuilder {
            cfg,
            current_block: current,
            offset_to_block: BTreeMap::new(),
        }
    }

    pub fn current_block(&self) -> BlockId {
        self.current_block
    }

    pub fn set_current_block(&mut self, id: BlockId) {
        self.current_block = id;
    }

    pub fn create_block(&mut self) -> BlockId {
        self.cfg.create_block()
    }

    pub fn get_or_create_block(&mut self, offset: u32) -> BlockId {
        if let Some(&id) = self.offset_to_block.get(&offset) {
            return id;
        }
        let id = self.cfg.create_block();
        self.offset_to_block.insert(offset, id);
        id
    }

    pub fn emit(&mut self, stmt: Statement) {
        if let Some(block) = self.cfg.get_mut(self.current_block) {
            block.push(stmt);
        }
    }

    pub fn terminate(&mut self, term: Terminator) {
        if let Some(block) = self.cfg.get_mut(self.current_block) {
            block.set_terminator(term);
        }
    }

    pub fn emit_jump(&mut self, target: BlockId) {
        self.terminate(Terminator::jump(target));
    }

    pub fn emit_branch(&mut self, cond: Expression, true_: BlockId, false_: BlockId) {
        self.terminate(Terminator::branch(cond, true_, false_));
    }

    pub fn emit_return(&mut self, value: Option<Expression>) {
        self.terminate(Terminator::Return(value));
    }

    pub fn has_terminator(&self) -> bool {
        self.cfg
            .get(self.current_block)
            .map(|b| !matches!(b.terminator, Terminator::None))
            .unwrap_or(false)
    }

    pub fn finish(self) -> CFG {
        self.cfg
    }
}

impl Default for CFGBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, Value};

    #[test]
    fn test_builder_basic() {
        let mut builder = CFGBuilder::new();

        let stmt = Statement::let_stmt("x", Expression::constant(Constant::Integer(1)));
        builder.emit(stmt);
        builder.emit_return(Some(Expression::Value(Value::Register(0))));

        let cfg = builder.finish();
        assert_eq!(cfg.block_count(), 1);
    }

    #[test]
    fn test_builder_branch() {
        let mut builder = CFGBuilder::new();

        let then_block = builder.create_block();
        let else_block = builder.create_block();

        let cond = Expression::Value(Value::Register(0));
        builder.emit_branch(cond, then_block, else_block);

        builder.set_current_block(then_block);
        builder.emit_return(Some(Expression::constant(Constant::Integer(1))));

        builder.set_current_block(else_block);
        builder.emit_return(Some(Expression::constant(Constant::Integer(2))));

        let cfg = builder.finish();
        assert_eq!(cfg.block_count(), 3);
    }
}
