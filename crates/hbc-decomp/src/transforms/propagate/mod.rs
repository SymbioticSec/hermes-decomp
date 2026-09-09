use crate::ir::{AssignTarget, BlockId, Constant, Expression, PropertyKey, Statement, Value, CFG};
use std::collections::BTreeMap;

mod reaching_passes;
mod substitute;

pub use reaching_passes::{propagate_copies, resolve_global_reads};
use substitute::{substitute_stmt, substitute_terminator};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Default)]
pub struct PropagationConfig {
    pub max_iterations: usize,
}

impl PropagationConfig {
    pub fn new() -> Self {
        Self { max_iterations: 10 }
    }
}

pub fn propagate(cfg: &mut CFG, config: &PropagationConfig) {
    let max_iter = if config.max_iterations == 0 {
        10
    } else {
        config.max_iterations
    };

    // Global copies of loop-/branch-invariant values (a register defined exactly
    // once as a Parameter, Global, or Constant, possibly through a copy chain).
    // These hold the same value in every block, so substitute them everywhere in a
    // single pass. The previous code re-seeded a clone of this map into every block
    // on every iteration, which was O(globals x blocks x iterations) and blew up on
    // a large, constant-heavy global function.
    let globals = global_invariant_copies(cfg);
    if !globals.is_empty() {
        for block in cfg.blocks_mut() {
            let statements = std::mem::take(&mut block.statements);
            block.statements = statements
                .into_iter()
                .map(|s| substitute_stmt(&s, &globals))
                .collect();
            block.terminator = substitute_terminator(&block.terminator, &globals);
        }
    }

    for _ in 0..max_iter {
        let mut changed = false;

        for block_id in cfg.block_ids().collect::<Vec<_>>() {
            if propagate_block(cfg, block_id) {
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

// Registers defined exactly once with an invariant value (Parameter / Global /
// Constant). Such a register holds the same value everywhere, so it can be
// propagated across block boundaries. A single-def register that copies another
// single-def register resolves through the chain (`r1 = r0; r0 = Parameter` makes
// r1 the Parameter too), which SSA makes sound because every version is defined
// once. Without this, a param staged through a copy across a clobbered register
// (`Mov r1, r3; r3 = ...; use(r1)` in a later block) is left dangling as `tmp`.
fn global_invariant_copies(cfg: &CFG) -> BTreeMap<u32, Expression> {
    let mut def_count: BTreeMap<u32, usize> = BTreeMap::new();
    let mut values: BTreeMap<u32, Expression> = BTreeMap::new();
    for block in cfg.blocks() {
        for stmt in &block.statements {
            if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
                *def_count.entry(*r).or_insert(0) += 1;
                values.insert(*r, value.clone());
            }
        }
    }
    let mut result = BTreeMap::new();
    for &r in values.keys() {
        if let Some(inv) = resolve_invariant_register(r, &values, &def_count, 0) {
            result.insert(r, inv);
        }
    }
    result
}

// Resolve a single-def register to its invariant value, following register copy
// chains. Returns None if any link is redefined more than once or the chain does
// not end in a Parameter / Global / Constant.
fn resolve_invariant_register(
    r: u32,
    values: &BTreeMap<u32, Expression>,
    def_count: &BTreeMap<u32, usize>,
    depth: u8,
) -> Option<Expression> {
    if depth > 16 || def_count.get(&r).copied().unwrap_or(0) != 1 {
        return None;
    }
    match values.get(&r)? {
        v @ Expression::Value(
            Value::Parameter(_) | Value::Global | Value::Constant(_),
        ) => Some(v.clone()),
        Expression::Value(Value::Register(b)) => {
            resolve_invariant_register(*b, values, def_count, depth + 1)
        }
        _ => None,
    }
}

fn propagate_block(cfg: &mut CFG, block_id: BlockId) -> bool {
    let block = match cfg.get_mut(block_id) {
        Some(b) => b,
        None => return false,
    };

    // Block-local copies only; the globally-invariant ones were already
    // substituted whole-function in `propagate`.
    let mut copies: BTreeMap<u32, Expression> = BTreeMap::new();
    let mut changed = false;

    // Take ownership instead of cloning
    let statements = std::mem::take(&mut block.statements);
    let mut new_statements = Vec::with_capacity(statements.len());

    for stmt in statements {
        // Substitute uses
        let substituted = substitute_stmt(&stmt, &copies);
        if substituted != stmt {
            changed = true;
        }

        // Track definitions
        if let Statement::Assign {
            target: AssignTarget::Register(r),
            value,
        } = &substituted
        {
            // Redefining `r` invalidates any earlier copy whose value reads `r`.
            // Otherwise `x = r; r = new; use(x)` would resolve `x` to the NEW
            // value of `r` (e.g. `tmp = sum; sum = undefined; print(tmp)` would
            // become `print(undefined)`), since copies store a register reference
            // rather than a snapshot of the value.
            copies.retain(|_, v| !crate::ir::expr_uses_register(v, *r));
            if is_propagatable(value) {
                copies.insert(*r, value.clone());
            } else {
                copies.remove(r);
            }
        }

        new_statements.push(substituted);
    }

    // Substitute copies into the terminator too. Branch/Switch conditions and
    // Return/Throw values live in the terminator, not in `statements`, so
    // without this a register copied from a parameter (e.g. a switch
    // discriminant compared in several arms) survives un-propagated in the
    // condition, leaving `1 === arg0` but `2 === tmp` inconsistent.
    let new_terminator = {
        let block = match cfg.get_mut(block_id) {
            Some(b) => b,
            None => return changed,
        };
        block.statements = new_statements;
        substitute_terminator(&block.terminator, &copies)
    };
    if let Some(block) = cfg.get_mut(block_id) {
        if new_terminator != block.terminator {
            changed = true;
        }
        block.terminator = new_terminator;
    }

    changed
}

fn is_propagatable(expr: &Expression) -> bool {
    match expr {
        // `__exception` is the synthetic binding produced by the `Catch` opcode.
        // Propagating it detaches the exception value from the register that
        // becomes the catch parameter, so the catch body ends up referring to a
        // free `__exception` (renamed inconsistently from the `catch (e)` param).
        // Keep it pinned to its register.
        Expression::Value(Value::Variable(name)) if name == "__exception" => false,
        Expression::Value(_) => true,
        // Allow propagation of simple member access on known safe objects
        // e.g., `Object = globalThis.Object` → inline `globalThis.Object`
        Expression::Member {
            object,
            property: PropertyKey::Ident(_),
            optional: false,
        } => matches!(
            object.as_ref(),
            Expression::Value(Value::Global)
                | Expression::Value(Value::Constant(Constant::String(_)))
        ),
        _ => false,
    }
}
