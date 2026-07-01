use super::recovery::recover_structure;
use super::{RecoveryCtx, Structure};
use crate::analysis::loops::LoopInfo;
use crate::ir::{BlockId, Expression, Terminator};

pub(super) fn recover_loop(
    ctx: &mut RecoveryCtx<'_>,
    loop_info: &LoopInfo,
    loop_stack: &[&LoopInfo],
) -> Structure {
    ctx.visited.insert(loop_info.header);

    let header = match ctx.cfg.get(loop_info.header) {
        Some(b) => b,
        None => return Structure::Block(loop_info.header, vec![]),
    };

    let header_stmts = header.statements.clone();

    let update_block = if loop_info.back_edges.len() == 1 {
        Some(loop_info.back_edges[0].0)
    } else {
        None
    };

    let make_loop = |body: Structure, condition: Expression| -> Structure {
        let loop_struct = if let Some(update_id) = update_block {
            if let Some((new_body, update_struct)) = split_update_from_body(body.clone(), update_id)
            {
                Structure::For {
                    init: Box::new(Structure::Block(BlockId(0), vec![])),
                    condition,
                    update: Box::new(update_struct),
                    body: Box::new(new_body),
                }
            } else {
                Structure::While {
                    condition,
                    body: Box::new(body),
                }
            }
        } else {
            Structure::While {
                condition,
                body: Box::new(body),
            }
        };

        let label_name = format!("label{}", loop_stack.len());
        Structure::Label(label_name, Box::new(loop_struct))
    };

    let mut new_stack = loop_stack.to_vec();
    new_stack.push(loop_info);

    match &header.terminator {
        Terminator::Branch {
            condition,
            true_target,
            false_target,
        } => {
            let condition = condition.clone();
            let true_target = *true_target;
            let false_target = *false_target;
            let true_in_loop = loop_info.body.contains(&true_target);
            let false_in_loop = loop_info.body.contains(&false_target);

            // Single-block do-while: the header branches back to itself, so its
            // own statements ARE the loop body. Emit `do { header } while(cond)`
            // instead of hoisting the body out and leaving the loop empty (which
            // silently dropped counting loops).
            if true_target == loop_info.header || false_target == loop_info.header {
                let loops_on_true = true_target == loop_info.header;
                let exit_target = if loops_on_true {
                    false_target
                } else {
                    true_target
                };
                let cond = if loops_on_true {
                    condition
                } else {
                    Expression::unary(crate::ir::UnaryOp::Not, condition)
                };
                let mut parts = vec![Structure::DoWhile {
                    body: Box::new(Structure::Block(loop_info.header, header_stmts)),
                    condition: cond,
                }];
                if !ctx.visited.contains(&exit_target) {
                    parts.push(recover_structure(ctx, exit_target, loop_stack));
                }
                return Structure::Sequence(parts);
            }

            if true_in_loop && !false_in_loop {
                let body = recover_structure(ctx, true_target, &new_stack);
                let mut parts = vec![Structure::Block(loop_info.header, header_stmts)];
                parts.push(make_loop(body, condition));

                if let Some(exit) = loop_info.exit {
                    if !ctx.visited.contains(&exit) {
                        let after = recover_structure(ctx, exit, loop_stack);
                        parts.push(after);
                    }
                }
                Structure::Sequence(parts)
            } else if false_in_loop && !true_in_loop {
                let body = recover_structure(ctx, false_target, &new_stack);
                let mut parts = vec![Structure::Block(loop_info.header, header_stmts)];
                parts.push(make_loop(
                    body,
                    Expression::unary(crate::ir::UnaryOp::Not, condition),
                ));

                if let Some(exit) = loop_info.exit {
                    if !ctx.visited.contains(&exit) {
                        let after = recover_structure(ctx, exit, loop_stack);
                        parts.push(after);
                    }
                }
                Structure::Sequence(parts)
            } else {
                let then_ = recover_structure(ctx, true_target, &new_stack);
                let else_ = recover_structure(ctx, false_target, &new_stack);
                let mut parts = vec![Structure::Block(loop_info.header, header_stmts)];
                parts.push(Structure::If {
                    condition,
                    then_: Box::new(then_),
                    else_: Box::new(else_),
                });
                Structure::Sequence(parts)
            }
        }
        Terminator::Jump(target) => {
            let target = *target;
            if loop_info.body.contains(&target) {
                let body = recover_structure(ctx, target, &new_stack);
                let mut parts = vec![Structure::Block(loop_info.header, header_stmts)];
                parts.push(make_loop(
                    body,
                    Expression::constant(crate::ir::Constant::Bool(true)),
                ));
                Structure::Sequence(parts)
            } else {
                let next = recover_structure(ctx, target, loop_stack);
                Structure::Sequence(vec![Structure::Block(loop_info.header, header_stmts), next])
            }
        }
        _ => Structure::Block(loop_info.header, header_stmts),
    }
}

pub(super) fn split_update_from_body(body: Structure, update_id: BlockId) -> Option<(Structure, Structure)> {
    match body {
        Structure::Sequence(mut parts) => {
            if parts.is_empty() {
                return None;
            }
            let last = parts.pop().unwrap();

            if let Structure::Block(id, ref _stmts) = last {
                if id == update_id {
                    let new_body = if parts.len() == 1 {
                        parts.into_iter().next().unwrap()
                    } else {
                        Structure::Sequence(parts)
                    };
                    return Some((new_body, last));
                }
            }

            if let Some((new_last, update)) = split_update_from_body(last.clone(), update_id) {
                parts.push(new_last);
                let new_body = Structure::Sequence(parts);
                return Some((new_body, update));
            }

            parts.push(last);
            None
        }
        Structure::Block(id, _) => {
            if id == update_id {
                return Some((Structure::Block(BlockId(0), vec![]), body));
            }
            None
        }
        _ => None,
    }
}
