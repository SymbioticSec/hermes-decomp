// Generator CFG transform: converts SaveGenerator+Ret patterns into yield/await
// at the CFG level, before structure recovery distorts the state machine.
//
// In Hermes generator functions:
// - SaveGenerator writes __yield_point__:N as a comment
// - Ret yields the value (the generator suspends)
// - At offset N, ResumeGenerator reads the resumed value
//
// This pass converts:
//   Block B: [...stmts, Comment("__yield_point__:N")] + Return(val)
// Into:
//   Block B: [...stmts, Assign(result_reg, Yield(val))] + Jump(resume_block)
//
// This makes the yield visible to structure recovery as a regular statement,
// not a return that breaks the control flow.

use crate::ir::{AssignTarget, BlockId, Expression, PropertyKey, Statement, Terminator, CFG};
use std::collections::BTreeMap;

pub fn transform_generator_cfg(cfg: &mut CFG) {
    // Check if this function has any generator patterns
    let has_start_gen = cfg.blocks().any(|b| {
        b.statements.iter().any(|s| matches!(s, Statement::Comment(c) if c == "StartGenerator"))
    });
    if !has_start_gen {
        return;
    }

    // Build offset → BlockId map for resume targets
    let offset_to_block = cfg.offset_to_block.clone();

    // Collect yield point info: (block_id, yield_value, resume_offset)
    let mut yield_blocks: Vec<(BlockId, Option<Expression>, u32)> = Vec::new();

    for (block_id, block) in cfg.blocks_with_ids() {
        // Check if last statement is __yield_point__:N
        if let Some(Statement::Comment(c)) = block.statements.last() {
            if let Some(addr_str) = c.strip_prefix("__yield_point__:") {
                if let Ok(resume_offset) = addr_str.parse::<u32>() {
                    // Check if terminator is Return(Some(value))
                    if let Terminator::Return(Some(val)) = &block.terminator {
                        yield_blocks.push((block_id, Some(val.clone()), resume_offset));
                    } else if let Terminator::Return(None) = &block.terminator {
                        yield_blocks.push((block_id, None, resume_offset));
                    }
                }
            }
        }
    }


    if yield_blocks.is_empty() {
        return;
    }

    // For each resume block, find the ResumeGenerator statement and its result register
    let mut resume_registers: BTreeMap<u32, u32> = BTreeMap::new(); // resume_offset → result_register
    for (&offset, &block_id) in &offset_to_block {
        if let Some(block) = cfg.get(block_id) {
            if let Some(first_stmt) = block.statements.first() {
                if let Statement::Assign {
                    target: AssignTarget::Register(reg),
                    value,
                } = first_stmt
                {
                    if is_resume_call(value) {
                        resume_registers.insert(offset, *reg);
                    }
                }
            }
        }
    }

    // Transform each yield block
    for (block_id, yield_value, resume_offset) in yield_blocks {
        let resume_block = match offset_to_block.get(&resume_offset) {
            Some(&bid) => bid,
            None => continue,
        };

        let block = match cfg.get_mut(block_id) {
            Some(b) => b,
            None => continue,
        };

        // Remove the __yield_point__ comment
        block.statements.pop();

        // Create the yield expression
        let yield_expr = Expression::Yield {
            value: Box::new(yield_value.unwrap_or(Expression::constant(crate::ir::Constant::Undefined))),
            delegate: false,
        };

        // If there's a resume register, assign the yield result to it
        if let Some(&result_reg) = resume_registers.get(&resume_offset) {
            block.statements.push(Statement::Assign {
                target: AssignTarget::Register(result_reg),
                value: yield_expr,
            });
        } else {
            // No resume result — just emit as expression statement
            block.statements.push(Statement::Expr(yield_expr));
        }

        // Change terminator from Return to Jump to the resume block
        block.set_terminator(Terminator::Jump(resume_block));

        // Remove the ResumeGenerator statement from the resume block
        // and also remove the "is resumed?" branch that follows it
        if let Some(resume_block_data) = cfg.get_mut(resume_block) {
            // The resume block typically starts with:
            //   r_result = gen.resume()  ← already consumed by yield assignment
            //   if (gen) goto completed else goto continue
            // We need to keep just the "continue" path

            // Remove the resume call statement
            if !resume_block_data.statements.is_empty() {
                if let Some(Statement::Assign { value, .. }) = resume_block_data.statements.first() {
                    if is_resume_call(value) {
                        resume_block_data.statements.remove(0);
                    }
                }
            }

            // If the terminator is a Branch checking the "is completed" flag,
            // redirect to just the "not completed" (continue) path
            if let Terminator::Branch { true_target, false_target, .. } = resume_block_data.terminator.clone() {
                // true_target = "generator completed" (return path)
                // false_target = "continue execution" (normal path)
                resume_block_data.set_terminator(Terminator::Jump(false_target));

                // Mark the "completed" path block to return the resume value
                // (it already has a Return terminator from the original CFG)
                let _ = true_target; // completed path stays as-is
            }
        }
    }

    // Clean up the StartGenerator comment and initial resume check
    // The entry block typically has: StartGenerator, arg setup, ResumeGenerator, Branch
    // We want to keep the arg setup but remove the generator boilerplate
    let entry_id = cfg.entry;
    if let Some(entry) = cfg.get_mut(entry_id) {
        // Remove StartGenerator comment
        entry.statements.retain(|s| !matches!(s, Statement::Comment(c) if c == "StartGenerator"));

        // Remove the initial ResumeGenerator (it's the "is being resumed" check)
        let mut resume_idx = None;
        for (i, stmt) in entry.statements.iter().enumerate() {
            if let Statement::Assign { value, .. } = stmt {
                if is_resume_call(value) {
                    resume_idx = Some(i);
                    break;
                }
            }
        }
        if let Some(idx) = resume_idx {
            entry.statements.remove(idx);

            // If the terminator branches on the resume check, skip to the "not resumed" branch
            if let Terminator::Branch { false_target, .. } = entry.terminator.clone() {
                entry.set_terminator(Terminator::Jump(false_target));
            }
        }
    }

    // Also remove CompleteGenerator blocks (they just have Return terminators, already handled)
}

fn is_resume_call(expr: &Expression) -> bool {
    if let Expression::Call { callee, .. } = expr {
        if let Expression::Member {
            property: PropertyKey::Ident(name),
            ..
        } = callee.as_ref()
        {
            return name == "resume";
        }
    }
    false
}
