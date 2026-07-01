use crate::ir::{Expression, Statement};
use std::collections::BTreeMap;

pub mod analysis;
pub mod cleanup;
pub mod state_machine;
pub mod state_machine_v98;
pub mod transform;

pub use analysis::*;
pub use cleanup::cleanup_generator_comments;
pub use state_machine::simplify_state_machine;
pub use state_machine_v98::reconstruct_generator_v98;

use transform::*;

pub fn detect_generator_patterns(stmts: Vec<Statement>, is_async: bool) -> Vec<Statement> {
    if !has_generator_patterns(&stmts) {
        return stmts;
    }

    let yield_points = collect_yield_points(&stmts);
    let resume_points = collect_resume_points(&stmts);

    if yield_points.is_empty() {
        return stmts;
    }

    let mut yield_info: BTreeMap<u32, (Option<Expression>, Option<u32>)> = BTreeMap::new();

    for yp in &yield_points {
        yield_info.insert(yp.resume_state, (yp.yield_value.clone(), None));
    }

    // This logic from original generator.rs seems to perform an update on yield_info
    // which is not used later?
    // Wait, the original code collected `yield_info` but updated `reg` inside it.
    // However, `transform_generator_stmts` does NOT use `yield_info`.
    // It uses `yield_to_resume` which it calculates again itself.
    // The snippet:
    // for rp in &resume_points { ... yield_info.get_mut ... }
    // seems dead code in the original implementation or duplicate logic.
    // `transform_generator_stmts` re-implements the resume matching logic.
    // So I can skip the `yield_info` logic here as it doesn't affect `transform_generator_stmts`.

    transform_generator_stmts(stmts, &yield_points, &resume_points, is_async)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Constant, PropertyKey, Value};
    use crate::transforms::generator::analysis::has_generator_patterns;

    #[test]
    fn test_detect_yield_point() {
        let stmts = vec![
            Statement::Comment("StartGenerator".to_string()),
            Statement::Assign {
                target: AssignTarget::Register(0),
                value: Expression::Value(Value::Constant(Constant::Integer(1))),
            },
            Statement::Comment("__yield_point__:100".to_string()),
            Statement::Return(Some(Expression::Value(Value::Register(0)))),
        ];

        assert!(has_generator_patterns(&stmts));
    }

    #[test]
    fn test_transform_yield_with_result() {
        // Simulate: result = yield 42
        // Pattern:
        //   __yield_point__:100
        //   return 42
        //   result = gen.resume()
        let stmts = vec![
            Statement::Comment("__yield_point__:100".to_string()),
            Statement::Return(Some(Expression::Value(Value::Constant(Constant::Integer(
                42,
            ))))),
            Statement::Assign {
                target: AssignTarget::Register(5),
                value: Expression::Call {
                    callee: Box::new(Expression::Member {
                        object: Box::new(Expression::Value(Value::Register(0))),
                        property: PropertyKey::Ident("resume".to_string()),
                        optional: false,
                    }),
                    arguments: vec![],
                },
            },
        ];

        let result = detect_generator_patterns(stmts, false);

        // Should have: r5 = yield 42
        let has_yield_assign = result.iter().any(|s| {
            if let Statement::Assign {
                target: AssignTarget::Register(5),
                value,
            } = s
            {
                matches!(value, Expression::Yield { .. })
            } else {
                false
            }
        });
        assert!(has_yield_assign, "Expected r5 = yield 42, got: {result:?}");
    }

    #[test]
    fn test_transform_await_with_result() {
        let stmts = vec![
            Statement::Comment("__yield_point__:100".to_string()),
            Statement::Return(Some(Expression::Call {
                callee: Box::new(Expression::Value(Value::Variable("fetch".to_string()))),
                arguments: vec![Expression::Value(Value::Constant(Constant::String(
                    "url".to_string(),
                )))],
            })),
            Statement::Assign {
                target: AssignTarget::Register(3),
                value: Expression::Call {
                    callee: Box::new(Expression::Member {
                        object: Box::new(Expression::Value(Value::Register(0))),
                        property: PropertyKey::Ident("resume".to_string()),
                        optional: false,
                    }),
                    arguments: vec![],
                },
            },
        ];

        let result = detect_generator_patterns(stmts, true);

        // Should have: r3 = await fetch("url")
        let has_await_assign = result.iter().any(|s| {
            if let Statement::Assign {
                target: AssignTarget::Register(3),
                value,
            } = s
            {
                matches!(value, Expression::Await(_))
            } else {
                false
            }
        });
        assert!(
            has_await_assign,
            "Expected r3 = await fetch(...), got: {result:?}"
        );
    }

    #[test]
    fn test_yield_expression_display() {
        let yield_expr = Expression::Yield {
            value: Box::new(Expression::Value(Value::Constant(Constant::Integer(42)))),
            delegate: false,
        };
        assert_eq!(format!("{yield_expr}"), "yield 42");

        let yield_delegate = Expression::Yield {
            value: Box::new(Expression::Value(Value::Variable("iter".to_string()))),
            delegate: true,
        };
        assert_eq!(format!("{yield_delegate}"), "yield* iter");
    }

    #[test]
    fn test_await_expression_display() {
        // Hermes convention: first arg is `this` (undefined for global calls)
        let await_expr = Expression::Await(Box::new(Expression::Call {
            callee: Box::new(Expression::Value(Value::Variable("fetch".to_string()))),
            arguments: vec![
                Expression::Value(Value::Constant(Constant::Undefined)), // this
                Expression::Value(Value::Constant(Constant::String("url".to_string()))),
            ],
        }));
        assert_eq!(format!("{await_expr}"), "await fetch(\"url\")");
    }

    #[test]
    fn test_cleanup_generator_comments() {
        let stmts = vec![
            Statement::Comment("StartGenerator".to_string()),
            Statement::Assign {
                target: AssignTarget::Register(0),
                value: Expression::Value(Value::Constant(Constant::Integer(1))),
            },
            Statement::Comment("__yield_point__:100".to_string()),
            Statement::Return(Some(Expression::Value(Value::Register(0)))),
            Statement::Comment("Some other comment".to_string()),
        ];

        let result = cleanup_generator_comments(stmts);

        assert!(!result
            .iter()
            .any(|s| matches!(s, Statement::Comment(c) if c == "StartGenerator")));
        assert!(!result
            .iter()
            .any(|s| matches!(s, Statement::Comment(c) if c.starts_with("__yield_point__"))));
        assert!(result
            .iter()
            .any(|s| matches!(s, Statement::Comment(c) if c == "Some other comment")));
    }

    #[test]
    fn test_multiple_yields_with_results() {
        // Simulate:
        //   a = yield 1
        //   b = yield 2
        let stmts = vec![
            Statement::Comment("StartGenerator".to_string()),
            Statement::Comment("__yield_point__:100".to_string()),
            Statement::Return(Some(Expression::Value(Value::Constant(Constant::Integer(
                1,
            ))))),
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Call {
                    callee: Box::new(Expression::Member {
                        object: Box::new(Expression::Value(Value::Register(0))),
                        property: PropertyKey::Ident("resume".to_string()),
                        optional: false,
                    }),
                    arguments: vec![],
                },
            },
            Statement::Comment("__yield_point__:200".to_string()),
            Statement::Return(Some(Expression::Value(Value::Constant(Constant::Integer(
                2,
            ))))),
            Statement::Assign {
                target: AssignTarget::Register(2),
                value: Expression::Call {
                    callee: Box::new(Expression::Member {
                        object: Box::new(Expression::Value(Value::Register(0))),
                        property: PropertyKey::Ident("resume".to_string()),
                        optional: false,
                    }),
                    arguments: vec![],
                },
            },
        ];

        let result = detect_generator_patterns(stmts, false);

        // Count yield assignments
        let yield_count = result
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    Statement::Assign {
                        value: Expression::Yield { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            yield_count, 2,
            "Expected 2 yield assignments, got: {result:?}"
        );
    }
}
