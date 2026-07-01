use crate::ir::{AssignTarget, BlockId, Constant, Expression, PropertyKey, Statement, Terminator, Value, CFG};
use std::collections::BTreeMap;

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
    // once as a Parameter, Global, or Constant). These are valid in every block,
    // so seed them so a value used across blocks (e.g. a switch discriminant read
    // in sibling branches) is substituted consistently — not just within the
    // defining block.
    let globals = global_invariant_copies(cfg);

    for _ in 0..max_iter {
        let mut changed = false;

        for block_id in cfg.block_ids().collect::<Vec<_>>() {
            if propagate_block(cfg, block_id, &globals) {
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

// Replace every register read whose reaching definitions are ALL `globalThis`
// with `globalThis` itself. `globalThis` (GetGlobalObject) is an invariant,
// freely re-materialized value, so this substitution is always
// semantics-preserving.
//
// Motivation: the HBC >=97 register allocator aggressively reuses the register
// that held `globalThis` for unrelated later values (e.g. nested-ternary string
// results). The merge-freeze in `transform_to_ssa` then keeps that register
// under a single name, collapsing the two independent live ranges — producing
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

// Registers defined exactly once with an invariant value (Parameter / Global /
// Constant). Such a register holds the same value everywhere, so it can be
// propagated across block boundaries.
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
    values
        .into_iter()
        .filter(|(r, v)| {
            def_count.get(r).copied().unwrap_or(0) == 1
                && matches!(
                    v,
                    Expression::Value(Value::Parameter(_))
                        | Expression::Value(Value::Global)
                        | Expression::Value(Value::Constant(_))
                )
        })
        .collect()
}

fn propagate_block(cfg: &mut CFG, block_id: BlockId, globals: &BTreeMap<u32, Expression>) -> bool {
    let block = match cfg.get_mut(block_id) {
        Some(b) => b,
        None => return false,
    };

    // Seed with globally-invariant copies (valid in every block), then track
    // local copies on top.
    let mut copies: BTreeMap<u32, Expression> = globals.clone();
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
    // condition — leaving `1 === arg0` but `2 === tmp` inconsistent.
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

fn substitute_terminator(term: &Terminator, copies: &BTreeMap<u32, Expression>) -> Terminator {
    match term {
        Terminator::Branch {
            condition,
            true_target,
            false_target,
        } => Terminator::Branch {
            condition: substitute_expr(condition, copies),
            true_target: *true_target,
            false_target: *false_target,
        },
        Terminator::Return(Some(e)) => Terminator::Return(Some(substitute_expr(e, copies))),
        Terminator::Throw(e) => Terminator::Throw(substitute_expr(e, copies)),
        Terminator::Switch {
            value,
            cases,
            default,
        } => Terminator::Switch {
            value: substitute_expr(value, copies),
            cases: cases
                .iter()
                .map(|(e, t)| (substitute_expr(e, copies), *t))
                .collect(),
            default: *default,
        },
        _ => term.clone(),
    }
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

fn substitute_stmt(stmt: &Statement, copies: &BTreeMap<u32, Expression>) -> Statement {
    match stmt {
        Statement::Expr(e) => Statement::Expr(substitute_expr(e, copies)),
        Statement::Let { name, value, kind } => Statement::Let {
            name: name.clone(),
            value: substitute_expr(value, copies),
            kind: *kind,
        },
        Statement::Assign { target, value } => Statement::Assign {
            target: substitute_target(target, copies),
            value: substitute_expr(value, copies),
        },
        Statement::Return(Some(e)) => Statement::Return(Some(substitute_expr(e, copies))),
        Statement::Throw(e) => Statement::Throw(substitute_expr(e, copies)),
        _ => stmt.clone(),
    }
}

fn substitute_target(target: &AssignTarget, copies: &BTreeMap<u32, Expression>) -> AssignTarget {
    match target {
        AssignTarget::Index { object, key } => AssignTarget::Index {
            object: substitute_expr(object, copies),
            key: substitute_expr(key, copies),
        },
        AssignTarget::Member { object, property } => AssignTarget::Member {
            object: substitute_expr(object, copies),
            property: property.clone(),
        },
        _ => target.clone(),
    }
}

fn substitute_expr(expr: &Expression, copies: &BTreeMap<u32, Expression>) -> Expression {
    match expr {
        Expression::Value(Value::Register(r)) => {
            copies.get(r).cloned().unwrap_or_else(|| expr.clone())
        }
        Expression::Binary { op, left, right } => Expression::binary(
            *op,
            substitute_expr(left, copies),
            substitute_expr(right, copies),
        ),
        Expression::Unary { op, operand } => {
            Expression::unary(*op, substitute_expr(operand, copies))
        }
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(substitute_expr(callee, copies)),
            arguments: arguments
                .iter()
                .map(|a| substitute_expr(a, copies))
                .collect(),
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(substitute_expr(callee, copies)),
            arguments: arguments
                .iter()
                .map(|a| substitute_expr(a, copies))
                .collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => {
            let new_obj = substitute_expr(object, copies);
            let new_prop = substitute_property_key(property, copies);
            Expression::Member {
                object: Box::new(new_obj),
                property: new_prop,
                optional: *optional,
            }
        }
        Expression::Array { elements } => Expression::Array {
            elements: elements
                .iter()
                .map(|e| e.as_ref().map(|ex| substitute_expr(ex, copies)))
                .collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .iter()
                .map(|p| crate::ir::ObjectProperty {
                    key: substitute_property_key(&p.key, copies),
                    value: substitute_expr(&p.value, copies),
                })
                .collect(),
        },
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => Expression::Conditional {
            condition: Box::new(substitute_expr(condition, copies)),
            then_expr: Box::new(substitute_expr(then_expr, copies)),
            else_expr: Box::new(substitute_expr(else_expr, copies)),
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(substitute_expr(target, copies)),
            value: Box::new(substitute_expr(value, copies)),
        },
        Expression::Spread(inner) => Expression::Spread(Box::new(substitute_expr(inner, copies))),
        _ => expr.clone(),
    }
}

fn substitute_property_key(key: &PropertyKey, copies: &BTreeMap<u32, Expression>) -> PropertyKey {
    match key {
        PropertyKey::Computed(expr) => {
            let subst = substitute_expr(expr, copies);
            // If the substituted expression is a constant integer, convert to Index
            match &subst {
                Expression::Value(Value::Constant(Constant::Integer(n))) => {
                    PropertyKey::Index(*n as i64)
                }
                Expression::Value(Value::Constant(Constant::Number(n))) if n.fract() == 0.0 => {
                    PropertyKey::Index(*n as i64)
                }
                _ => PropertyKey::Computed(Box::new(subst)),
            }
        }
        _ => key.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CFGBuilder, Constant};

    #[test]
    fn test_constant_propagation() {
        let mut builder = CFGBuilder::new();
        builder.emit(Statement::assign_reg(
            0,
            Expression::constant(Constant::Integer(42)),
        ));
        builder.emit(Statement::assign_reg(
            1,
            Expression::Value(Value::Register(0)),
        ));
        builder.emit_return(Some(Expression::Value(Value::Register(1))));

        let mut cfg = builder.finish();
        propagate(&mut cfg, &PropagationConfig::new());

        let block = cfg.entry_block();
        // After propagation, r1 should be assigned 42, not r0
        if let Statement::Assign { value, .. } = &block.statements[1] {
            assert_eq!(*value, Expression::constant(Constant::Integer(42)));
        }
    }
}
