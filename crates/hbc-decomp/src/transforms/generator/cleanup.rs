use crate::ir::Statement;

// Cleanup generator-specific comments after transformation.
pub fn cleanup_generator_comments(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .filter(|stmt| {
            !matches!(stmt, Statement::Comment(c) if
                c == "StartGenerator" ||
                c.starts_with("__yield_point__:"))
        })
        .map(|stmt| match stmt {
            Statement::If {
                condition,
                then_body,
                else_body,
            } => Statement::If {
                condition,
                then_body: cleanup_generator_comments(then_body),
                else_body: cleanup_generator_comments(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition,
                body: cleanup_generator_comments(body),
            },
            Statement::For {
                init,
                condition,
                update,
                body,
            } => Statement::For {
                init,
                condition,
                update,
                body: cleanup_generator_comments(body),
            },
            Statement::TryCatch {
                try_body,
                catch_param,
                catch_body,
                finally_body,
            } => Statement::TryCatch {
                try_body: cleanup_generator_comments(try_body),
                catch_param,
                catch_body: cleanup_generator_comments(catch_body),
                finally_body: cleanup_generator_comments(finally_body),
            },
            Statement::Block(inner) => Statement::Block(cleanup_generator_comments(inner)),
            other => other,
        })
        .collect()
}
