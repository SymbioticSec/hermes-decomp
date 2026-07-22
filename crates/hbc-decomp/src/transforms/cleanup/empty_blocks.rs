// Drop empty blocks and no-op empty statement lists left by other passes.

use crate::ir::{map_nested_bodies_mut, Statement};

pub fn remove_empty_blocks(mut stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.retain(|s| !is_empty_stmt(s));
    for stmt in &mut stmts {
        map_nested_bodies_mut(stmt, remove_empty_blocks);
    }
    stmts
}

fn is_empty_stmt(stmt: &Statement) -> bool {
    match stmt {
        Statement::Block(inner) => inner.is_empty(),
        Statement::Expr(crate::ir::Expression::Value(crate::ir::Value::Constant(
            crate::ir::Constant::Undefined,
        ))) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_empty_blocks() {
        let stmts = vec![
            Statement::Block(vec![]),
            Statement::Comment("keep".into()),
            Statement::Block(vec![Statement::Comment("inner".into())]),
        ];
        let out = remove_empty_blocks(stmts);
        assert_eq!(out.len(), 2);
    }
}
