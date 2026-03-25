use crate::ir::{AssignTarget, Expression, PropertyKey, Statement};

// Information about a yield point in the generator.
#[derive(Debug, Clone)]
pub struct YieldPoint {
    // Index in statement list where __yield_point__ marker is
    pub marker_index: usize,
    // The resume address (state number)
    pub resume_state: u32,
    // The value being yielded (from the following return)
    pub yield_value: Option<Expression>,
    // Index of the return statement
    pub return_index: Option<usize>,
}

// Information about a resume point.
#[derive(Debug, Clone)]
pub struct ResumePoint {
    // Index in statement list
    pub index: usize,
    // Register that receives the resumed value
    pub result_register: u32,
}

// Collect all yield points from statements.
//
// Hermes compiles generators/async functions into a state machine.
// When disassembling, we might see comments like `// __yield_point__: 12`.
// This analysis phase scans the IR for these markers to reconstruct where `yield` or `await` should be.
// - `YieldPoint`: A location where the function suspends (await/yield).
// - `ResumePoint`: A location where the function resumes execution (often a callback or state jump).
pub fn collect_yield_points(stmts: &[Statement]) -> Vec<YieldPoint> {
    let mut points = Vec::new();
    let mut i = 0;

    while i < stmts.len() {
        if let Statement::Comment(c) = &stmts[i] {
            if let Some(addr_str) = c.strip_prefix("__yield_point__:") {
                if let Ok(addr) = addr_str.parse::<u32>() {
                    let mut yp = YieldPoint {
                        marker_index: i,
                        resume_state: addr,
                        yield_value: None,
                        return_index: None,
                    };

                    // Look for the following return
                    if i + 1 < stmts.len() {
                        if let Statement::Return(Some(val)) = &stmts[i + 1] {
                            yp.yield_value = Some(val.clone());
                            yp.return_index = Some(i + 1);
                        }
                    }

                    points.push(yp);
                }
            }
        }

        // Recurse into nested structures
        match &stmts[i] {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                points.extend(collect_yield_points(then_body));
                points.extend(collect_yield_points(else_body));
            }
            Statement::While { body, .. } | Statement::For { body, .. } => {
                points.extend(collect_yield_points(body));
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                points.extend(collect_yield_points(try_body));
                points.extend(collect_yield_points(catch_body));
                points.extend(collect_yield_points(finally_body));
            }
            Statement::Block(inner) => {
                points.extend(collect_yield_points(inner));
            }
            _ => {}
        }

        i += 1;
    }

    points
}

// Collect all resume points (ResumeGenerator calls).
//
// A "Resume Point" corresponds to the re-entry logic of the generator.
// In the bytecode, this often looks like a call to `generator.resume()` or internal helper.
// We map these back to the return value of the `yield` expression (e.g., `let result = yield x;`).
pub fn collect_resume_points(stmts: &[Statement]) -> Vec<ResumePoint> {
    let mut points = Vec::new();

    for (i, stmt) in stmts.iter().enumerate() {
        match stmt {
            Statement::Assign {
                target: AssignTarget::Register(reg),
                value,
            } => {
                if is_resume_call(value) {
                    points.push(ResumePoint {
                        index: i,
                        result_register: *reg,
                    });
                }
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                points.extend(collect_resume_points(then_body));
                points.extend(collect_resume_points(else_body));
            }
            Statement::While { body, .. } | Statement::For { body, .. } => {
                points.extend(collect_resume_points(body));
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                points.extend(collect_resume_points(try_body));
                points.extend(collect_resume_points(catch_body));
                points.extend(collect_resume_points(finally_body));
            }
            Statement::Block(inner) => {
                points.extend(collect_resume_points(inner));
            }
            _ => {}
        }
    }

    points
}

// Check if an expression is a ResumeGenerator call (gen.resume()).
pub fn is_resume_call(expr: &Expression) -> bool {
    if let Expression::Call { callee, .. } = expr {
        if let Expression::Member {
            property: PropertyKey::Ident(name),
            ..
        } = callee.as_ref()
        {
            return name == "resume";
        }
    }
    false
}

// Detect if statements contain generator patterns (yield points).
pub fn has_generator_patterns(stmts: &[Statement]) -> bool {
    for stmt in stmts {
        match stmt {
            Statement::Comment(c) if c.starts_with("__yield_point__:") => return true,
            Statement::Comment(c) if c == "StartGenerator" => return true,
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                if has_generator_patterns(then_body) || has_generator_patterns(else_body) {
                    return true;
                }
            }
            Statement::While { body, .. } | Statement::For { body, .. } => {
                if has_generator_patterns(body) {
                    return true;
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                if has_generator_patterns(try_body)
                    || has_generator_patterns(catch_body)
                    || has_generator_patterns(finally_body)
                {
                    return true;
                }
            }
            Statement::Block(inner) => {
                if has_generator_patterns(inner) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}
