use crate::file::ExceptionHandler;
use crate::ir::{BlockId, Expression, Statement, Terminator, CFG};
use crate::{BytecodeFile, BytecodeFormat, Instruction, Result};
use std::collections::BTreeMap;

use super::dispatch::dispatch_instruction;
use super::jump_analysis::find_block_starts_with_handlers;
use super::opcodes_flow::FlowResult;

#[derive(Debug, Clone, Default)]
pub struct IRBuilderOptions {
    pub resolve_strings: bool,
    pub include_offsets: bool,
    pub absolute_offsets: bool,
}

pub struct IRBuilder<'a> {
    file: &'a BytecodeFile,
    format: &'a BytecodeFormat,
    options: IRBuilderOptions,
}

impl<'a> IRBuilder<'a> {
    pub fn new(
        file: &'a BytecodeFile,
        format: &'a BytecodeFormat,
        options: IRBuilderOptions,
    ) -> Self {
        IRBuilder {
            file,
            format,
            options,
        }
    }

    pub fn build_function(&mut self, function_id: u32) -> Result<CFG> {
        let instructions = self
            .file
            .decode_function_instructions(self.format, function_id)?;
        let handlers = self.file.exception_handlers.get(&function_id)
            .map(|h| h.as_slice())
            .unwrap_or(&[]);
        // Compute function's bytecode offset in the global instructions array
        let func_bytecode_offset = self.file.function_headers
            .get(function_id as usize)
            .map(|h| h.offset().saturating_sub(self.file.instruction_offset))
            .unwrap_or(0);
        let mut cfg = self.build_from_instructions(&instructions, handlers, func_bytecode_offset)?;

        super::generator_cfg::transform_generator_cfg(&mut cfg);

        Ok(cfg)
    }

    fn build_from_instructions(&mut self, instructions: &[Instruction], exception_handlers: &[ExceptionHandler], func_bytecode_offset: u32) -> Result<CFG> {
        if instructions.is_empty() {
            let mut cfg = CFG::new();
            cfg.get_mut(cfg.entry)
                .expect("entry block must exist")
                .set_terminator(Terminator::Return(None));
            return Ok(cfg);
        }

        let block_starts = find_block_starts_with_handlers(instructions, self.format, self.file, exception_handlers, func_bytecode_offset);

        let mut offset_to_block: BTreeMap<u32, BlockId> = BTreeMap::new();
        let mut cfg = CFG::new();

        let first_offset = instructions[0].offset;
        offset_to_block.insert(first_offset, cfg.entry);

        for &offset in &block_starts {
            if offset != first_offset {
                let id = cfg.create_block();
                offset_to_block.insert(offset, id);
            }
        }

        for handler in exception_handlers {
            if let (Some(&try_start), Some(&catch_block)) = (
                offset_to_block.get(&handler.start),
                offset_to_block.get(&handler.target),
            ) {
                cfg.exception_handlers.push(crate::ir::CfgExceptionHandler {
                    try_block_start: try_start,
                    catch_block,
                });
            }
        }
        cfg.offset_to_block = offset_to_block.clone();

        // SAFETY: current_block always holds a valid BlockId from this CFG (entry or created above),
        // so cfg.get_mut(current_block).expect(...) below cannot fail.
        let mut current_block = cfg.entry;
        let mut current_stmts: Vec<Statement> = Vec::new();

        for inst in instructions {
            if let Some(&block_id) = offset_to_block.get(&inst.offset) {
                if block_id != current_block {
                    self.finalize_block(&mut cfg, current_block, current_stmts, block_id);
                    current_stmts = Vec::new();
                    current_block = block_id;
                }
            }

            if self.options.include_offsets {
                if self.options.absolute_offsets {
                    let abs_offset = self
                        .file
                        .instruction_offset
                        .wrapping_add(func_bytecode_offset)
                        .wrapping_add(inst.offset);
                    current_stmts.push(Statement::Comment(format!("@{abs_offset:08x}")));
                } else {
                    current_stmts.push(Statement::Comment(format!("@{:04x}", inst.offset)));
                }
            }

            let result =
                dispatch_instruction(inst, self.file, self.format, self.options.resolve_strings, func_bytecode_offset);

            match result {
                FlowResult::Statement(stmt) => {
                    current_stmts.push(stmt);
                }
                FlowResult::Jump { target } => {
                    let target_block =
                        self.get_or_create_block(&mut cfg, &mut offset_to_block, target);
                    self.set_block_stmts(&mut cfg, current_block, current_stmts);
                    cfg.get_mut(current_block)
                        .expect("current block must exist")
                        .set_terminator(Terminator::Jump(target_block));
                    current_stmts = Vec::new();
                    current_block = target_block;
                }
                FlowResult::Branch {
                    condition,
                    target,
                    fallthrough,
                } => {
                    let true_block =
                        self.get_or_create_block(&mut cfg, &mut offset_to_block, target);
                    let false_block =
                        self.get_or_create_block(&mut cfg, &mut offset_to_block, fallthrough);
                    self.set_block_stmts(&mut cfg, current_block, current_stmts);
                    cfg.get_mut(current_block)
                        .expect("current block must exist")
                        .set_terminator(Terminator::Branch {
                            condition,
                            true_target: true_block,
                            false_target: false_block,
                        });
                    current_stmts = Vec::new();
                    current_block = false_block;
                }
                FlowResult::Return(value) => {
                    self.set_block_stmts(&mut cfg, current_block, current_stmts);
                    cfg.get_mut(current_block)
                        .expect("current block must exist")
                        .set_terminator(Terminator::Return(value));
                    current_stmts = Vec::new();
                }
                FlowResult::Throw(value) => {
                    self.set_block_stmts(&mut cfg, current_block, current_stmts);
                    cfg.get_mut(current_block)
                        .expect("current block must exist")
                        .set_terminator(Terminator::Throw(value));
                    current_stmts = Vec::new();
                }
                FlowResult::Noop => {}
                FlowResult::Switch {
                    value,
                    default,
                    cases,
                } => {
                    let default_block =
                        self.get_or_create_block(&mut cfg, &mut offset_to_block, default);
                    let mut switch_cases = Vec::new();

                    for (case_expr, target_offset) in cases {
                        let target_block =
                            self.get_or_create_block(&mut cfg, &mut offset_to_block, target_offset);
                        switch_cases.push((case_expr, target_block));
                    }

                    self.set_block_stmts(&mut cfg, current_block, current_stmts);
                    cfg.get_mut(current_block)
                        .expect("current block must exist")
                        .set_terminator(Terminator::Switch {
                            value,
                            cases: switch_cases,
                            default: default_block,
                        });
                    current_stmts = Vec::new();
                }
            }
        }

        if !current_stmts.is_empty()
            || matches!(
                cfg.get(current_block).map(|b| &b.terminator),
                Some(Terminator::None)
            )
        {
            self.set_block_stmts(&mut cfg, current_block, current_stmts);
            if matches!(
                cfg.get(current_block).map(|b| &b.terminator),
                Some(Terminator::None)
            ) {
                cfg.get_mut(current_block)
                    .expect("current block must exist")
                    .set_terminator(Terminator::Return(Some(Expression::constant(
                        crate::ir::Constant::Undefined,
                    ))));
            }
        }

        Ok(cfg)
    }

    fn get_or_create_block(
        &self,
        cfg: &mut CFG,
        offset_to_block: &mut BTreeMap<u32, BlockId>,
        offset: u32,
    ) -> BlockId {
        if let Some(&id) = offset_to_block.get(&offset) {
            id
        } else {
            let id = cfg.create_block();
            offset_to_block.insert(offset, id);
            id
        }
    }

    fn set_block_stmts(&self, cfg: &mut CFG, block: BlockId, stmts: Vec<Statement>) {
        if let Some(b) = cfg.get_mut(block) {
            b.statements = stmts;
        }
    }

    fn finalize_block(&self, cfg: &mut CFG, block: BlockId, stmts: Vec<Statement>, next: BlockId) {
        if let Some(b) = cfg.get_mut(block) {
            if !matches!(b.terminator, Terminator::None) && stmts.is_empty() {
            } else {
                b.statements = stmts;
            }
            if matches!(b.terminator, Terminator::None) {
                b.set_terminator(Terminator::Jump(next));
            }
        }
    }
}
