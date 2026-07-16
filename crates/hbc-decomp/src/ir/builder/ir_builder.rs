use crate::file::ExceptionHandler;
use crate::ir::{BlockId, Expression, Statement, Terminator, CFG};
use crate::{BytecodeFile, BytecodeFormat, Instruction, Result};
use std::collections::BTreeMap;

use super::dispatch::dispatch_instruction;
use super::env_state::EnvRegMap;
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
        // Frame size drives the implicit call/construct argument register layout.
        let frame_size = self.file.function_headers
            .get(function_id as usize)
            .map(|h| h.frame_size())
            .unwrap_or(0);
        let mut cfg = self.build_from_instructions(&instructions, handlers, func_bytecode_offset, frame_size)?;

        super::generator_cfg::transform_generator_cfg(&mut cfg);

        Ok(cfg)
    }

    fn build_from_instructions(&mut self, instructions: &[Instruction], exception_handlers: &[ExceptionHandler], func_bytecode_offset: u32, frame_size: u32) -> Result<CFG> {
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
            // Skip synthetic empty-`finally` handlers. The Hermes compiler lowers
            // `try { ... } finally {}` (and the implicit cleanup edge of a
            // `try/catch/finally`) to a handler whose target is a pure rethrow
            // (`Catch rX; Throw rX`). Keeping it both clobbers the real catch
            // (handlers share a try-start) and wraps the body in a spurious extra
            // try/catch. An empty finally is a semantic no-op, so drop it.
            if is_rethrow_only_handler(instructions, self.format, handler.target) {
                continue;
            }
            // Skip the synthetic iterator-cleanup handler. The spec lowers array
            // destructuring / for-of to a `try { ...iterate... } catch { iter
            // .return(); throw }` so the iterator is closed on abrupt completion;
            // its catch block calls IteratorClose. This compiler-generated cleanup
            // is decompilation noise that fragments the iterator protocol across
            // try-bodies and defeats for-of / destructuring reconstruction. Drop
            // it (the close on the normal path remains); the `.return()` on error
            // is a no-op for non-throwing runs.
            if is_iterator_cleanup_handler(instructions, self.format, handler.target) {
                continue;
            }
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
        // Flow-insensitive last-write map: env register → nesting level.
        let mut env_map = EnvRegMap::new();

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

            let result = dispatch_instruction(
                inst,
                self.file,
                self.format,
                self.options.resolve_strings,
                func_bytecode_offset,
                frame_size,
                &mut env_map,
            );

            match result {
                // A handler that lowers one opcode to several statements returns
                // them wrapped in a Block; flatten it into the instruction stream
                // (basic blocks are still flat at this stage).
                FlowResult::Statement(Statement::Block(inner)) => {
                    current_stmts.extend(inner);
                }
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

// True if the exception handler whose catch target is `target_offset` is a pure
// rethrow: the target block consists of exactly `Catch rX` followed by
// `Throw rX`. The Hermes compiler emits such handlers for an empty `finally`
// (and for the cleanup edge of a `try/catch/finally`), where they are semantic
// no-ops that should not surface as their own try/catch in the output.
fn is_rethrow_only_handler(
    insts: &[Instruction],
    format: &BytecodeFormat,
    target_offset: u32,
) -> bool {
    let opcode_name = |inst: &Instruction| -> Option<&str> {
        format.definitions.get(inst.opcode as usize).map(|d| d.name.as_str())
    };

    let idx = match insts.iter().position(|i| i.offset == target_offset) {
        Some(i) => i,
        None => return false,
    };

    let catch = &insts[idx];
    if opcode_name(catch) != Some("Catch") {
        return false;
    }
    let catch_reg = match catch.operands.first().and_then(|o| o.value.as_u32()) {
        Some(r) => r,
        None => return false,
    };

    match insts.get(idx + 1) {
        Some(next) if opcode_name(next) == Some("Throw") => {
            next.operands.first().and_then(|o| o.value.as_u32()) == Some(catch_reg)
        }
        _ => false,
    }
}

// True if the handler whose catch target is `target_offset` is the synthetic
// iterator-cleanup handler: a `Catch` block that calls `IteratorClose` (closing
// the iterator on abrupt completion) before the next handler block. Emitted by
// the compiler for array destructuring and for-of; it is decompilation noise.
fn is_iterator_cleanup_handler(
    insts: &[Instruction],
    format: &BytecodeFormat,
    target_offset: u32,
) -> bool {
    let opcode_name = |inst: &Instruction| -> Option<&str> {
        format.definitions.get(inst.opcode as usize).map(|d| d.name.as_str())
    };

    let idx = match insts.iter().position(|i| i.offset == target_offset) {
        Some(i) => i,
        None => return false,
    };
    if opcode_name(&insts[idx]) != Some("Catch") {
        return false;
    }
    // Scan the catch block for an IteratorClose, following a single unconditional
    // `Jmp` to the convergent cleanup block (the per-element catches all `Jmp` to
    // a shared `... IteratorClose; Throw`). Stop at the next handler's `Catch`.
    let jmp_target = |inst: &Instruction| -> Option<usize> {
        let rel = inst.operands.iter().find_map(|o| {
            matches!(o.ty, crate::opcode::OperandType::Addr8 | crate::opcode::OperandType::Addr32)
                .then(|| o.value.as_i32())
                .flatten()
        })?;
        let tgt = (inst.offset as i64 + rel as i64) as u32;
        insts.iter().position(|x| x.offset == tgt)
    };
    let mut i = idx + 1;
    let mut followed = false;
    let mut steps = 0;
    while i < insts.len() && steps < 64 {
        steps += 1;
        match opcode_name(&insts[i]) {
            Some("IteratorClose") => return true,
            Some("Catch") => return false,
            Some("Jmp") | Some("JmpLong") if !followed => {
                if let Some(p) = jmp_target(&insts[i]) {
                    followed = true;
                    i = p;
                    continue;
                }
                return false;
            }
            _ => {}
        }
        i += 1;
    }
    false
}
