use super::exceptions::recover_catch_body;
use super::loops::recover_loop;
use super::{RecoveryCtx, Structure};
use crate::analysis::loops::{detect_loops, LoopInfo};
use crate::ir::{BlockId, Expression, Statement, Terminator, CFG};
use std::collections::{BTreeMap, HashSet};

// Recovers high-level structures from CFG.
//
// This is the "Structurization" phase.
// The CFG (Control Flow Graph) is a graph of basic blocks with jumps.
// We need to convert this back into nested structures like `if`, `while`, `for`.
//
// Strategy:
// 1. Detect loops (headers, bodies, exits) using graph analysis.
// 2. Traverse the graph starting from entry.
// 3. Recursively match patterns:
//    - If a node dominates 2 children that merge back, it's an `if`.
//    - If a node is a loop header, recurse into the loop body.
//    - Handle `break` and `continue` by checking loop stacks.
pub fn analyze(cfg: &CFG) -> (Structure, Vec<LoopInfo>) {
    let loops = detect_loops(cfg);
    let mut visited = HashSet::new();
    let loop_stack = Vec::new();

    // Build a map: try_block_start → exception handler info.
    //
    // A single source-level `try` can compile to several handler ranges that all
    // target the *same* catch block — the compiler splits the protected region
    // around an inner branch (e.g. the `throw` arm gets its own range). Keying
    // naively by start would then emit one nested try/catch per range. Collapse
    // them: register a single try per catch block, at its earliest start, so the
    // body recovery folds the remaining ranges in as ordinary control flow.
    let mut earliest_for_catch: BTreeMap<BlockId, &crate::ir::CfgExceptionHandler> =
        BTreeMap::new();
    for handler in &cfg.exception_handlers {
        earliest_for_catch
            .entry(handler.catch_block)
            .and_modify(|h| {
                if handler.try_block_start < h.try_block_start {
                    *h = handler;
                }
            })
            .or_insert(handler);
    }
    let mut try_starts: BTreeMap<BlockId, _> = BTreeMap::new();
    for handler in earliest_for_catch.values() {
        try_starts.insert(handler.try_block_start, *handler);
    }

    let mut ctx = RecoveryCtx {
        cfg,
        loops: &loops,
        visited: &mut visited,
        try_starts: &try_starts,
    };

    let root = recover_structure(&mut ctx, cfg.entry, &loop_stack);
    (root, loops)
}

pub(super) fn recover_structure(
    ctx: &mut RecoveryCtx<'_>,
    block_id: BlockId,
    loop_stack: &[&LoopInfo],
) -> Structure {
    if ctx.visited.contains(&block_id) {
        // If we're jumping to a loop header we're in, it's a continue
        for (i, loop_info) in loop_stack.iter().enumerate().rev() {
            if block_id == loop_info.header {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Continue(label);
            }
            if loop_info.exit == Some(block_id) {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Break(label);
            }
        }
        return Structure::Block(block_id, vec![]);
    }

    if let Some(handler) = ctx.try_starts.get(&block_id) {
        let catch_block_id = handler.catch_block;

        // Mark catch block as visited to prevent it from being included in try body traversal
        let catch_was_visited = ctx.visited.contains(&catch_block_id);
        ctx.visited.insert(catch_block_id);

        // Recover the try body (catch block excluded via visited set)
        let try_body = recover_structure_inner(ctx, block_id, loop_stack);

        // Now recover the catch body — temporarily un-visit the catch block
        if !catch_was_visited {
            ctx.visited.remove(&catch_block_id);
        }
        let (catch_param, catch_body) = recover_catch_body(ctx, catch_block_id, loop_stack);

        return Structure::TryCatch {
            try_body: Box::new(try_body),
            catch_param,
            catch_body: Box::new(catch_body),
        };
    }

    recover_structure_inner(ctx, block_id, loop_stack)
}

pub(super) fn recover_structure_inner(
    ctx: &mut RecoveryCtx<'_>,
    block_id: BlockId,
    loop_stack: &[&LoopInfo],
) -> Structure {
    if ctx.visited.contains(&block_id) {
        for (i, loop_info) in loop_stack.iter().enumerate().rev() {
            if block_id == loop_info.header {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Continue(label);
            }
            if loop_info.exit == Some(block_id) {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Break(label);
            }
        }
        return Structure::Block(block_id, vec![]);
    }

    // Check if this is a loop header
    if let Some(loop_info) = ctx.loops.iter().find(|l| l.header == block_id) {
        if !ctx.visited.contains(&block_id) {
            let loop_info = loop_info.clone();
            return recover_loop(ctx, &loop_info, loop_stack);
        }
    }

    ctx.visited.insert(block_id);

    let block = match ctx.cfg.get(block_id) {
        Some(b) => b,
        None => {
            log::warn!(
                "Block {} not found in CFG during structure recovery",
                block_id.0
            );
            return Structure::Block(block_id, vec![]);
        }
    };

    let stmts = block.statements.clone();

    match &block.terminator {
        Terminator::Return(e) => {
            let mut all = stmts;
            all.push(Statement::Return(e.clone()));
            Structure::Block(block_id, all)
        }
        Terminator::Throw(e) => {
            let mut all = stmts;
            all.push(Statement::Throw(e.clone()));
            Structure::Block(block_id, all)
        }
        Terminator::Jump(target) => {
            let target = *target;
            // Check for loop continue/break
            for (i, loop_info) in loop_stack.iter().enumerate().rev() {
                if target == loop_info.header && ctx.visited.contains(&target) {
                    let label = if i < loop_stack.len() - 1 {
                        Some(format!("label{i}"))
                    } else {
                        None
                    };
                    let mut stmts = stmts;
                    stmts.push(Statement::Comment(if let Some(l) = label {
                        format!("continue {l}")
                    } else {
                        "continue".to_string()
                    }));
                    return Structure::Block(block_id, stmts);
                }
                if loop_info.exit == Some(target) {
                    let label = if i < loop_stack.len() - 1 {
                        Some(format!("label{i}"))
                    } else {
                        None
                    };
                    let mut stmts = stmts;
                    stmts.push(Statement::Comment(if let Some(l) = label {
                        format!("break {l}")
                    } else {
                        "break".to_string()
                    }));
                    return Structure::Block(block_id, stmts);
                }
            }

            let block_struct = Structure::Block(block_id, stmts);
            let next = recover_structure(ctx, target, loop_stack);
            Structure::Sequence(vec![block_struct, next])
        }
        Terminator::Branch {
            condition,
            true_target,
            false_target,
        } => {
            let condition = condition.clone();
            let true_target = *true_target;
            let false_target = *false_target;

            // Check for loop patterns
            if let Some(loop_info) = loop_stack.last() {
                let true_exits = loop_info.exit == Some(true_target);
                let false_exits = loop_info.exit == Some(false_target);

                if true_exits && !false_exits {
                    let mut parts = vec![Structure::Block(block_id, stmts.clone())];
                    let else_ = recover_structure(ctx, false_target, loop_stack);
                    parts.push(Structure::If {
                        condition: condition.clone(),
                        then_: Box::new(Structure::Break(None)),
                        else_: Box::new(else_),
                    });
                    return Structure::Sequence(parts);
                }

                if false_exits && !true_exits {
                    let mut parts = vec![Structure::Block(block_id, stmts.clone())];
                    let then_ = recover_structure(ctx, true_target, loop_stack);
                    parts.push(Structure::If {
                        condition: Expression::unary(crate::ir::UnaryOp::Not, condition.clone()),
                        then_: Box::new(Structure::Break(None)),
                        else_: Box::new(then_),
                    });
                    return Structure::Sequence(parts);
                }
            }

            // Diamond: if the two branches reconverge at a merge block, recover
            // each branch only UP TO that merge, then emit the merge AFTER the
            // if. Otherwise the merge (the common tail, e.g. a shared `return`)
            // is wrongly absorbed into the `then` branch and the `else` becomes
            // empty.
            if let Some(merge) = find_merge_point(ctx.cfg, block_id, true_target, false_target) {
                if merge != true_target || merge != false_target {
                    let merge_was_visited = ctx.visited.contains(&merge);
                    ctx.visited.insert(merge);
                    let then_ = recover_structure(ctx, true_target, loop_stack);
                    let else_ = recover_structure(ctx, false_target, loop_stack);
                    if !merge_was_visited {
                        ctx.visited.remove(&merge);
                    }
                    let mut parts = vec![
                        Structure::Block(block_id, stmts),
                        Structure::If {
                            condition,
                            then_: Box::new(then_),
                            else_: Box::new(else_),
                        },
                    ];
                    parts.push(recover_structure(ctx, merge, loop_stack));
                    return Structure::Sequence(parts);
                }
            }

            let then_ = recover_structure(ctx, true_target, loop_stack);
            let else_ = recover_structure(ctx, false_target, loop_stack);
            let mut parts = vec![Structure::Block(block_id, stmts)];
            parts.push(Structure::If {
                condition,
                then_: Box::new(then_),
                else_: Box::new(else_),
            });
            Structure::Sequence(parts)
        }
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            let value = value.clone();
            let cases = cases.clone();
            let default = *default;
            let mut parts = vec![Structure::Block(block_id, stmts)];

            let mut recovered: BTreeMap<BlockId, Structure> = BTreeMap::new();
            for (_case_val, target) in &cases {
                if !recovered.contains_key(target) {
                    let body = recover_structure(ctx, *target, loop_stack);
                    recovered.insert(*target, body);
                }
            }

            let mut switch_cases = Vec::new();
            for (case_val, target) in &cases {
                switch_cases.push((case_val.clone(), recovered[target].clone()));
            }

            let default_body = if let Some(cached) = recovered.get(&default) {
                cached.clone()
            } else {
                recover_structure(ctx, default, loop_stack)
            };

            parts.push(Structure::Switch {
                discriminant: value,
                cases: switch_cases,
                default: Box::new(default_body),
            });

            Structure::Sequence(parts)
        }
        Terminator::None => {
            log::debug!(
                "Block {} has Terminator::None, treating as simple block",
                block_id.0
            );
            Structure::Block(block_id, stmts)
        }
    }
}

// Find the merge (join) point of a branch: the nearest block reachable from
// BOTH the true and false targets. This is the block where the two arms of an
// `if`/`else` diamond reconverge (its immediate post-dominator for simple
// diamonds). Returns None when the branches never reconverge (e.g. both return).
//
// `header` is the branch block itself. Both reachability walks stop *before*
// re-entering `header`: a genuine forward join is reached without re-executing
// the branch. Without this guard, a loop back-edge that flows back through the
// branch would make the branch's own arms mutually reachable, and the walk would
// pick a spurious "merge" one arm earlier than the real post-dominator (which
// then gets wrongly absorbed into an `if` branch, producing infinite loops).
pub(super) fn find_merge_point(cfg: &CFG, header: BlockId, a: BlockId, b: BlockId) -> Option<BlockId> {
    use std::collections::VecDeque;
    // All blocks reachable from `a` without passing back through `header`.
    let mut reach_a: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![a];
    while let Some(n) = stack.pop() {
        if n == header || !reach_a.insert(n) {
            continue;
        }
        if let Some(blk) = cfg.get(n) {
            for s in blk.successors() {
                stack.push(s);
            }
        }
    }
    // Breadth-first from `b` for the NEAREST block also reachable from `a`,
    // likewise never crossing back through `header`.
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut queue: VecDeque<BlockId> = VecDeque::new();
    queue.push_back(b);
    while let Some(n) = queue.pop_front() {
        if n == header || !seen.insert(n) {
            continue;
        }
        if reach_a.contains(&n) {
            return Some(n);
        }
        if let Some(blk) = cfg.get(n) {
            for s in blk.successors() {
                queue.push_back(s);
            }
        }
    }
    None
}
