use super::analysis::*;
use crate::ir::{AssignTarget, Constant, Expression, Statement, Value};
use std::collections::{HashMap, HashSet};

// Transform generator statements into clean yield/await expressions.
//
// This is the final reconstruction step.
// 1. We have `YieldPoints` (where execution stops).
// 2. We have `ResumePoints` (where execution continues).
// 3. We have `yield_to_resume` mapping (which yield feeds into which resume).
//
// We traverse the statements. When we hit a `marker_index` corresponding to a yield point:
// - We inject a high-level `Expression::Yield` or `Expression::Await`.
// - If there is a corresponding resume point, we emit an assignment: `r0 = await ...`.
// - We delete the low-level machinery (markers, state switching boilerplate).
pub fn transform_generator_stmts(
    stmts: Vec<Statement>,
    yield_points: &[YieldPoint],
    resume_points: &[ResumePoint],
    is_async: bool,
) -> Vec<Statement> {
    // Build sets of indices to skip/transform
    let yield_markers: HashSet<usize> = yield_points.iter().map(|yp| yp.marker_index).collect();
    let yield_returns: HashSet<usize> = yield_points
        .iter()
        .filter_map(|yp| yp.return_index)
        .collect();
    let resume_indices: HashSet<usize> = resume_points.iter().map(|rp| rp.index).collect();

    // Build mapping from yield marker index to resume register
    // We pair yield points with the NEXT resume point (which receives the yielded result)
    let mut yield_to_resume: HashMap<usize, u32> = HashMap::new();

    // Sort yield points by marker index
    let mut sorted_yields: Vec<_> = yield_points.iter().collect();
    sorted_yields.sort_by_key(|yp| yp.marker_index);

    // Sort resume points by index
    let mut sorted_resumes: Vec<_> = resume_points.iter().collect();
    sorted_resumes.sort_by_key(|rp| rp.index);

    // Match each yield with the next resume point
    let mut resume_iter = sorted_resumes.iter().peekable();
    for yp in &sorted_yields {
        if let Some(ret_idx) = yp.return_index {
            // Find the first resume point after this yield's return
            while let Some(rp) = resume_iter.peek() {
                if rp.index > ret_idx {
                    yield_to_resume.insert(yp.marker_index, rp.result_register);
                    resume_iter.next(); // consume this resume point
                    break;
                }
                resume_iter.next();
            }
        }
    }

    let mut result = Vec::new();

    for (i, stmt) in stmts.into_iter().enumerate() {
        // Skip yield markers - we'll emit the yield expression instead
        if yield_markers.contains(&i) {
            // Find this yield point and emit the yield/await expression
            if let Some(yp) = yield_points.iter().find(|yp| yp.marker_index == i) {
                if let Some(ref yield_val) = yp.yield_value {
                    let yield_expr = if is_async {
                        Expression::Await(Box::new(yield_val.clone()))
                    } else {
                        Expression::Yield {
                            value: Box::new(yield_val.clone()),
                            delegate: false,
                        }
                    };

                    // Check if this yield has a result register
                    if let Some(result_reg) = yield_to_resume.get(&i) {
                        // Emit: result = yield value
                        result.push(Statement::Assign {
                            target: AssignTarget::Register(*result_reg),
                            value: yield_expr,
                        });
                    } else {
                        // Emit: yield value (no result)
                        result.push(Statement::Expr(yield_expr));
                    }
                }
            }
            continue;
        }

        // Skip yield returns - they're part of the yield
        if yield_returns.contains(&i) {
            continue;
        }

        // Skip resume calls - they're part of the yield result
        if resume_indices.contains(&i) {
            continue;
        }

        // Skip StartGenerator comments
        if let Statement::Comment(c) = &stmt {
            if c == "StartGenerator" {
                continue;
            }
        }

        // Transform nested statements
        let transformed = transform_nested_stmt(stmt, yield_points, resume_points, is_async);
        result.push(transformed);
    }

    result
}

// Transform a single statement, handling nested structures.
fn transform_nested_stmt(
    stmt: Statement,
    yield_points: &[YieldPoint],
    resume_points: &[ResumePoint],
    is_async: bool,
) -> Statement {
    match stmt {
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition: transform_expr(condition, is_async),
            then_body: transform_generator_stmts(then_body, yield_points, resume_points, is_async),
            else_body: transform_generator_stmts(else_body, yield_points, resume_points, is_async),
        },
        Statement::While { condition, body } => Statement::While {
            condition: transform_expr(condition, is_async),
            body: transform_generator_stmts(body, yield_points, resume_points, is_async),
        },
        Statement::For {
            init,
            condition,
            update,
            body,
        } => Statement::For {
            init: init.map(|s| {
                Box::new(transform_nested_stmt(
                    *s,
                    yield_points,
                    resume_points,
                    is_async,
                ))
            }),
            condition: condition.map(|c| transform_expr(c, is_async)),
            update: update.map(|s| {
                Box::new(transform_nested_stmt(
                    *s,
                    yield_points,
                    resume_points,
                    is_async,
                ))
            }),
            body: transform_generator_stmts(body, yield_points, resume_points, is_async),
        },
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => Statement::TryCatch {
            try_body: transform_generator_stmts(try_body, yield_points, resume_points, is_async),
            catch_param,
            catch_body: transform_generator_stmts(
                catch_body,
                yield_points,
                resume_points,
                is_async,
            ),
            finally_body: transform_generator_stmts(
                finally_body,
                yield_points,
                resume_points,
                is_async,
            ),
        },
        Statement::Block(inner) => Statement::Block(transform_generator_stmts(
            inner,
            yield_points,
            resume_points,
            is_async,
        )),
        Statement::Assign { target, value } => Statement::Assign {
            target,
            value: transform_expr(value, is_async),
        },
        Statement::Return(Some(expr)) => Statement::Return(Some(transform_expr(expr, is_async))),
        Statement::Throw(expr) => Statement::Throw(transform_expr(expr, is_async)),
        Statement::Expr(expr) => Statement::Expr(transform_expr(expr, is_async)),
        other => other,
    }
}

// Transform expressions, converting any remaining resume calls.
fn transform_expr(expr: Expression, is_async: bool) -> Expression {
    match expr {
        Expression::Call { callee, arguments }
            if is_resume_call(&Expression::Call {
                callee: callee.clone(),
                arguments: arguments.clone(),
            }) =>
        {
            // Orphan resume call - shouldn't happen but handle gracefully
            if is_async {
                Expression::Await(Box::new(Expression::Value(Value::Constant(
                    Constant::Undefined,
                ))))
            } else {
                Expression::Yield {
                    value: Box::new(Expression::Value(Value::Constant(Constant::Undefined))),
                    delegate: false,
                }
            }
        }
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(transform_expr(*callee, is_async)),
            arguments: arguments
                .into_iter()
                .map(|a| transform_expr(a, is_async))
                .collect(),
        },
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(transform_expr(*left, is_async)),
            right: Box::new(transform_expr(*right, is_async)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(transform_expr(*operand, is_async)),
        },
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => Expression::Conditional {
            condition: Box::new(transform_expr(*condition, is_async)),
            then_expr: Box::new(transform_expr(*then_expr, is_async)),
            else_expr: Box::new(transform_expr(*else_expr, is_async)),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(transform_expr(*object, is_async)),
            property,
            optional,
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(transform_expr(*callee, is_async)),
            arguments: arguments
                .into_iter()
                .map(|a| transform_expr(a, is_async))
                .collect(),
        },
        Expression::Array { elements } => Expression::Array {
            elements: elements
                .into_iter()
                .map(|e| e.map(|ex| transform_expr(ex, is_async)))
                .collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .into_iter()
                .map(|p| crate::ir::ObjectProperty {
                    key: p.key,
                    value: transform_expr(p.value, is_async),
                })
                .collect(),
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(transform_expr(*target, is_async)),
            value: Box::new(transform_expr(*value, is_async)),
        },
        Expression::Spread(inner) => Expression::Spread(Box::new(transform_expr(*inner, is_async))),
        other => other,
    }
}
