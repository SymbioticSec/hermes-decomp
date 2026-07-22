use super::recovery::recover_structure_inner;
use super::{RecoveryCtx, Structure};
use crate::analysis::loops::LoopInfo;
use crate::ir::{BlockId, Statement, Value};

pub(super) fn recover_catch_body(
    ctx: &mut RecoveryCtx<'_>,
    block_id: BlockId,
    loop_stack: &[&LoopInfo],
) -> (Option<String>, Structure) {
    let catch_param = ctx.cfg.get(block_id)
        .and_then(|b| extract_catch_param(&b.statements));

    let body = recover_structure_inner(ctx, block_id, loop_stack);

    // Strip the `__exception` assignment from the catch body
    let body = strip_exception_assign(body);

    (catch_param, body)
}

// Extract catch parameter from the first statement of a catch block.
// The Catch opcode generates: `reg = __exception` (later named as a Variable).
fn extract_catch_param(stmts: &[Statement]) -> Option<String> {
    for stmt in stmts {
        match stmt {
            Statement::Assign { target, value } => {
                if is_exception_value(value) {
                    return Some(target.to_string());
                }
            }
            Statement::Let { name, value, .. } => {
                if is_exception_value(value) {
                    return Some(name.clone());
                }
            }
            _ => {}
        }
    }
    None
}

fn is_exception_value(value: &crate::ir::Expression) -> bool {
    matches!(
        value,
        crate::ir::Expression::Value(Value::Variable(name)) if name == "__exception"
    )
}

fn strip_exception_assign(structure: Structure) -> Structure {
    match structure {
        Structure::Block(id, stmts) => {
            let filtered: Vec<_> = stmts.into_iter().filter(|s| {
                if let Statement::Assign { value, .. } = s {
                    if let crate::ir::Expression::Value(Value::Variable(name)) = value {
                        return name != "__exception";
                    }
                }
                true
            }).collect();
            Structure::Block(id, filtered)
        }
        Structure::Sequence(mut parts) => {
            if !parts.is_empty() {
                let first = parts.remove(0);
                let stripped = strip_exception_assign(first);
                parts.insert(0, stripped);
            }
            Structure::Sequence(parts)
        }
        other => other,
    }
}
