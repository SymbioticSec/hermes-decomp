use super::substitute::{substitute_stmt, substitute_terminator};
use crate::ir::{AssignTarget, BlockId, Expression, Statement, Value, CFG};
use std::collections::BTreeMap;

// Replace every register read whose reaching definitions are ALL `globalThis`
// with `globalThis` itself. `globalThis` (GetGlobalObject) is an invariant,
// freely re-materialized value, so this substitution is always
// semantics-preserving.
//
// Motivation: the HBC >=97 register allocator aggressively reuses the register
// that held `globalThis` for unrelated later values (e.g. nested-ternary string
// results). The merge-freeze in `transform_to_ssa` then keeps that register
// under a single name, collapsing the two independent live ranges, producing
// corrupt output like `g = globalThis; ...; g = "pos"; g.print(...)`. Resolving
// the `globalThis` reads up front frees the register so only the later values
// occupy it, and the freeze stays correct. Must run BEFORE `transform_to_ssa`.
pub fn resolve_global_reads(cfg: &mut CFG) {
    use crate::analysis::reaching::{DefSite, ReachingDefs};
    use std::collections::HashSet;

    // Definition sites that assign `globalThis`.
    let mut global_defs: HashSet<DefSite> = HashSet::new();
    for block in cfg.blocks() {
        for (i, stmt) in block.statements.iter().enumerate() {
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                value: Expression::Value(Value::Global),
            } = stmt
            {
                global_defs.insert(DefSite {
                    block: block.id,
                    stmt_index: i,
                    register: *r,
                });
            }
        }
    }
    if global_defs.is_empty() {
        return;
    }

    let rd = ReachingDefs::analyze(cfg);

    // Copies map for the registers whose current reaching defs are all `globalThis`.
    let global_copies = |reaching: &BTreeMap<u32, Vec<DefSite>>| -> BTreeMap<u32, Expression> {
        reaching
            .iter()
            .filter(|(_, defs)| !defs.is_empty() && defs.iter().all(|d| global_defs.contains(d)))
            .map(|(r, _)| (*r, Expression::Value(Value::Global)))
            .collect()
    };

    for block_id in cfg.block_ids().collect::<Vec<_>>() {
        // Reaching defs grouped per register at block entry.
        let mut reaching: BTreeMap<u32, Vec<DefSite>> = BTreeMap::new();
        if let Some(in_set) = rd.reaching_in.get(&block_id) {
            for d in in_set {
                reaching.entry(d.register).or_default().push(*d);
            }
        }

        let block = match cfg.get_mut(block_id) {
            Some(b) => b,
            None => continue,
        };
        let stmts = std::mem::take(&mut block.statements);
        let mut new_stmts = Vec::with_capacity(stmts.len());

        for (i, stmt) in stmts.into_iter().enumerate() {
            let copies = global_copies(&reaching);
            let new_stmt = if copies.is_empty() {
                stmt
            } else {
                substitute_stmt(&stmt, &copies)
            };
            // A def at this position becomes the sole reaching def for `r`.
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = &new_stmt
            {
                reaching.insert(
                    *r,
                    vec![DefSite {
                        block: block_id,
                        stmt_index: i,
                        register: *r,
                    }],
                );
            }
            new_stmts.push(new_stmt);
        }

        // Terminator reads see the post-last-statement reaching set.
        let copies = global_copies(&reaching);
        if let Some(block) = cfg.get_mut(block_id) {
            block.statements = new_stmts;
            if !copies.is_empty() {
                block.terminator = substitute_terminator(&block.terminator, &copies);
            }
        }
    }
}

// Cross-block copy propagation for register-to-register copies (`a = b`).
//
// The intra-block `propagate` pass cannot see a copy whose source and use live
// in different basic blocks. The classic example is a loop latch: Hermes emits
//
//     L_header:
//       ...
//       Mov  r0, r5          ; r0 = i   (a saved copy of the counter)
//       ...                  ; (branches -> separate blocks)
//     L_latch:
//       Inc  r5, r0          ; i = r0 + 1
//       JLess L_header, r5, len
//
// After SSA the copy becomes `tmp = i` in the header block and the increment
// `i = tmp + 1` in the latch block. Because they are in different blocks the
// intra-block pass never propagates `tmp := i`, and once the now-dead copy is
// removed the increment reads an undefined `tmp`. This pass propagates such
// copies globally, so the increment renders as `i = i + 1`.
//
// Correctness: a use of `a` may be replaced by `b` only when
//   1. `a` is defined exactly once (this copy), every use is reached solely by
//      it; and
//   2. `b` has not been redefined on any path from the copy to the use, checked
//      by requiring `b`'s reaching-definition set at the use to equal its set at
//      the copy. If `b` were reassigned in between, the two sets differ and the
//      copy is left in place (a still-valid, if redundant, statement).
//
// Must run AFTER `transform_to_ssa` (which gives the copy target its own single
// definition) and is idempotent, so it is safe within the propagation loop.
pub fn propagate_copies(cfg: &mut CFG) {
    use crate::analysis::reaching::{DefSite, ReachingDefs};
    use std::collections::{HashMap, HashSet};

    // Count register definitions to identify single-def copy targets.
    let mut def_count: HashMap<u32, usize> = HashMap::new();
    for block in cfg.blocks() {
        for stmt in &block.statements {
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = stmt
            {
                *def_count.entry(*r).or_insert(0) += 1;
            }
        }
    }

    // Register-to-register copies `a = b` where `a` is defined exactly once.
    // `a` -> source register `b`, plus the copy's definition site.
    let mut copies: HashMap<u32, u32> = HashMap::new();
    let mut copy_sites: HashMap<u32, DefSite> = HashMap::new();
    for block in cfg.blocks() {
        for (i, stmt) in block.statements.iter().enumerate() {
            if let Statement::Assign {
                target: AssignTarget::Register(a),
                value: Expression::Value(Value::Register(b)),
            } = stmt
            {
                if a != b && def_count.get(a).copied().unwrap_or(0) == 1 {
                    copies.insert(*a, *b);
                    copy_sites.insert(
                        *a,
                        DefSite {
                            block: block.id,
                            stmt_index: i,
                            register: *a,
                        },
                    );
                }
            }
        }
    }
    if copies.is_empty() {
        return;
    }

    let rd = ReachingDefs::analyze(cfg);

    // Reaching-definition set of every register on entry to `block`.
    let per_reg_reaching = |block: BlockId| -> HashMap<u32, HashSet<DefSite>> {
        let mut cur: HashMap<u32, HashSet<DefSite>> = HashMap::new();
        if let Some(in_set) = rd.reaching_in.get(&block) {
            for d in in_set {
                cur.entry(d.register).or_default().insert(*d);
            }
        }
        cur
    };

    // Pass 1, record each copy's source-register reaching set at the copy point.
    let mut signatures: HashMap<u32, HashSet<DefSite>> = HashMap::new();
    for block in cfg.blocks() {
        let mut cur = per_reg_reaching(block.id);
        for (i, stmt) in block.statements.iter().enumerate() {
            for (a, b) in &copies {
                if copy_sites
                    .get(a)
                    .is_some_and(|s| s.block == block.id && s.stmt_index == i)
                {
                    signatures.insert(*a, cur.get(b).cloned().unwrap_or_default());
                }
            }
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = stmt
            {
                let mut set = HashSet::new();
                set.insert(DefSite {
                    block: block.id,
                    stmt_index: i,
                    register: *r,
                });
                cur.insert(*r, set);
            }
        }
    }

    // The active substitution map (`a` -> `b`) at the current program point:
    // a copy applies only where its source's reaching set still matches the
    // signature recorded at the copy.
    let active_map = |cur: &HashMap<u32, HashSet<DefSite>>| -> BTreeMap<u32, Expression> {
        copies
            .iter()
            .filter_map(|(a, b)| {
                let sig = signatures.get(a)?;
                let now = cur.get(b).cloned().unwrap_or_default();
                if &now == sig {
                    Some((*a, Expression::Value(Value::Register(*b))))
                } else {
                    None
                }
            })
            .collect()
    };

    // Pass 2, substitute uses.
    for block_id in cfg.block_ids().collect::<Vec<_>>() {
        let mut cur = per_reg_reaching(block_id);
        let stmts = match cfg.get_mut(block_id) {
            Some(b) => std::mem::take(&mut b.statements),
            None => continue,
        };
        let mut new_stmts = Vec::with_capacity(stmts.len());
        for (i, stmt) in stmts.into_iter().enumerate() {
            let active = active_map(&cur);
            let new_stmt = if active.is_empty() {
                stmt
            } else {
                substitute_stmt(&stmt, &active)
            };
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = &new_stmt
            {
                let mut set = HashSet::new();
                set.insert(DefSite {
                    block: block_id,
                    stmt_index: i,
                    register: *r,
                });
                cur.insert(*r, set);
            }
            new_stmts.push(new_stmt);
        }
        let active = active_map(&cur);
        if let Some(block) = cfg.get_mut(block_id) {
            block.statements = new_stmts;
            if !active.is_empty() {
                block.terminator = substitute_terminator(&block.terminator, &active);
            }
        }
    }
}
