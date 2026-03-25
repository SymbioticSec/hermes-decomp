use super::Structure;
use crate::ir::{Statement, CFG};

impl Structure {
    // Convert structure to flat statements.
    //
    // This flattens the recursive `Structure` tree into a linear list of `Statement`s.
    // It handles specific syntax generation like:
    // - Converting generic `For` structures into C-style `for(init; cond; update)` if possible.
    // - Falling back to `while` loops if init/update are too complex.
    // - Generating labeled blocks for named breaks/continues.
    pub fn to_statements(&self, _cfg: &CFG) -> Vec<Statement> {
        match self {
            Structure::Block(_, stmts) => stmts.clone(),
            Structure::Sequence(parts) => {
                parts.iter().flat_map(|p| p.to_statements(_cfg)).collect()
            }
            Structure::If {
                condition,
                then_,
                else_,
            } => {
                let then_body = then_.to_statements(_cfg);
                let else_body = else_.to_statements(_cfg);
                vec![Statement::If {
                    condition: condition.clone(),
                    then_body,
                    else_body,
                }]
            }
            Structure::While { condition, body } => {
                let body = body.to_statements(_cfg);
                vec![Statement::While {
                    condition: condition.clone(),
                    body,
                }]
            }
            Structure::DoWhile { body, condition } => {
                let body_stmts = body.to_statements(_cfg);
                vec![Statement::DoWhile {
                    body: body_stmts,
                    condition: condition.clone(),
                }]
            }
            Structure::For {
                init,
                condition,
                update,
                body,
            } => {
                let init_stmts = init.to_statements(_cfg);
                let update_stmts = update.to_statements(_cfg);
                let body_stmts = body.to_statements(_cfg);

                // Check if we can emit a clean for loop
                // Init: 0 or 1 statement (Let or Assign)
                // Update: 1 statement (Assign or Expr)

                let can_be_for_update = update_stmts.len() == 1
                    && matches!(
                        update_stmts[0],
                        Statement::Assign { .. } | Statement::Expr(_)
                    );
                let can_be_for_init = init_stmts.is_empty()
                    || (init_stmts.len() == 1
                        && matches!(
                            init_stmts[0],
                            Statement::Let { .. } | Statement::Assign { .. }
                        ));

                if can_be_for_update && can_be_for_init {
                    // Emit canonical for loop: for (i=0; i<10; i++)
                    let init_stmt = if !init_stmts.is_empty() {
                        Some(Box::new(init_stmts[0].clone()))
                    } else {
                        None
                    };

                    let update_stmt = Some(Box::new(update_stmts[0].clone()));

                    vec![Statement::For {
                        init: init_stmt,
                        condition: Some(condition.clone()),
                        update: update_stmt,
                        body: body_stmts,
                    }]
                } else {
                    // Fallback to while loop:
                    // {
                    //    init;
                    //    while (cond) { body; update; }
                    // }
                    let mut stmts = init_stmts;
                    let mut body_stmts = body_stmts;
                    body_stmts.extend(update_stmts);
                    stmts.push(Statement::While {
                        condition: condition.clone(),
                        body: body_stmts,
                    });
                    stmts
                }
            }
            Structure::Switch {
                discriminant,
                cases,
                default,
            } => {
                let cases_mapped = cases
                    .iter()
                    .map(|(val, struct_)| (val.clone(), struct_.to_statements(_cfg)))
                    .collect();
                let default_stmts = default.to_statements(_cfg);
                let default_opt = if default_stmts.is_empty() {
                    None
                } else {
                    Some(default_stmts)
                };

                vec![Statement::Switch {
                    discriminant: discriminant.clone(),
                    cases: cases_mapped,
                    default: default_opt,
                }]
            }
            Structure::TryCatch { try_body, catch_param, catch_body } => {
                let try_stmts = try_body.to_statements(_cfg);
                let catch_stmts = catch_body.to_statements(_cfg);
                vec![Statement::TryCatch {
                    try_body: try_stmts,
                    catch_param: catch_param.clone(),
                    catch_body: catch_stmts,
                    finally_body: vec![],
                }]
            }
            Structure::Return(e) => vec![Statement::Return(e.clone())],
            Structure::Break(label) => {
                vec![Statement::Break(label.clone())]
            }
            Structure::Continue(label) => {
                vec![Statement::Continue(label.clone())]
            }
            Structure::Label(label, body) => {
                let mut stmts = vec![Statement::Comment(format!("{label}:"))];
                stmts.extend(body.to_statements(_cfg));
                stmts
            }
        }
    }

    // Check if structure is empty.
    pub fn is_empty(&self) -> bool {
        match self {
            Structure::Block(_, stmts) => stmts.is_empty(),
            Structure::Sequence(parts) => parts.iter().all(|p| p.is_empty()),
            Structure::Switch { cases, default, .. } => {
                cases.iter().all(|(_, s)| s.is_empty()) && default.is_empty()
            }
            Structure::TryCatch { try_body, catch_body, .. } => {
                try_body.is_empty() && catch_body.is_empty()
            }
            _ => false,
        }
    }
}
