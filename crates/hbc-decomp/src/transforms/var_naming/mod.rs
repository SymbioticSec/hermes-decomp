mod analysis;
mod closure_def_naming;
mod closure_definitions;
mod closure_inference;
mod closure_usage;
mod renaming;
mod state;
mod suggestions;

use crate::ir::Statement;
use state::VariableNamer;

pub use closure_definitions::{rename_closure_variables, rename_closure_variables_cross_function};
pub use closure_def_naming::rename_closures_from_definitions;
use analysis::analyze_stmt;

// Infer and apply better variable names.
//
// This is a two-pass transformation:
// 1. Analysis: Visit all statements to find "naming hints".
//    - Hints come from property accesses (`obj.length` -> `len`),
//    - Initializers (`fetch(...)` -> `response`),
//    - and object keys (`{ email: r0 }` -> `r0` is `email`).
// 2. Renaming: Apply the best found names to variables, replacing generic names like `r0`, `val`.
pub fn infer_variable_names(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut namer = VariableNamer::new();

    // First pass: analyze to infer names
    for stmt in &stmts {
        analyze_stmt(&mut namer, stmt);
    }

    // PHASE 2: Infer closure names from usage context
    // This analyzes how closures are used (e.g., .then() -> "promise") to suggest better names
    closure_usage::infer_closure_names_from_usage(&stmts, &mut namer);

    // Second pass: apply inferred names
    stmts.into_iter().map(|s| renaming::rename_stmt(&namer, s)).collect()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Expression, PropertyKey, Value};

    #[test]
    fn test_fetch_naming() {
        // r0 = fetch(url) → response = fetch(url)
        let stmts = vec![Statement::Assign {
            target: AssignTarget::Register(0),
            value: Expression::Call {
                callee: Box::new(Expression::Value(Value::Variable("fetch".to_string()))),
                arguments: vec![Expression::Value(Value::Variable("url".to_string()))],
            },
        }];

        let result = infer_variable_names(stmts);

        if let Statement::Assign { target, .. } = &result[0] {
            assert!(matches!(target, AssignTarget::Variable(n) if n == "response"));
        } else {
            panic!("Expected assign statement");
        }
    }

    #[test]
    fn test_property_naming() {
        // r0 = obj.length → length = obj.length (raw property name preferred)
        let stmts = vec![Statement::Assign {
            target: AssignTarget::Register(0),
            value: Expression::Member {
                object: Box::new(Expression::Value(Value::Variable("obj".to_string()))),
                property: PropertyKey::Ident("length".to_string()),
                optional: false,
            },
        }];

        let result = infer_variable_names(stmts);

        if let Statement::Assign { target, .. } = &result[0] {
            assert!(matches!(target, AssignTarget::Variable(n) if n == "length"));
        } else {
            panic!("Expected assign statement");
        }
    }

    #[test]
    fn test_new_instance_naming() {
        // r0 = new Date() → date = new Date()
        let stmts = vec![Statement::Assign {
            target: AssignTarget::Register(0),
            value: Expression::New {
                callee: Box::new(Expression::Value(Value::Variable("Date".to_string()))),
                arguments: vec![],
            },
        }];

        let result = infer_variable_names(stmts);

        if let Statement::Assign { target, .. } = &result[0] {
            assert!(matches!(target, AssignTarget::Variable(n) if n == "date"));
        } else {
            panic!("Expected assign statement");
        }
    }

    #[test]
    fn test_binary_op_naming() {
        // r0 = a + b → sum = a + b
        let stmts = vec![Statement::Assign {
            target: AssignTarget::Register(0),
            value: Expression::Binary {
                op: crate::ir::BinaryOp::Add,
                left: Box::new(Expression::Value(Value::Variable("a".to_string()))),
                right: Box::new(Expression::Value(Value::Variable("b".to_string()))),
            },
        }];

        let result = infer_variable_names(stmts);

        if let Statement::Assign { target, .. } = &result[0] {
            assert!(matches!(target, AssignTarget::Variable(n) if n == "sum"));
        } else {
            panic!("Expected assign statement");
        }
    }

    #[test]
    fn test_array_index_zero_naming() {
        // r0 = arr[0] → first = arr[0]
        let stmts = vec![Statement::Assign {
            target: AssignTarget::Register(0),
            value: Expression::Member {
                object: Box::new(Expression::Value(Value::Variable("items".to_string()))),
                property: PropertyKey::Index(0),
                optional: false,
            },
        }];

        let result = infer_variable_names(stmts);

        if let Statement::Assign { target, .. } = &result[0] {
            assert!(matches!(target, AssignTarget::Variable(n) if n == "first"));
        } else {
            panic!("Expected assign statement");
        }
    }

    #[test]
    fn test_unique_names() {
        // Two fetch calls should get unique names
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Register(0),
                value: Expression::Call {
                    callee: Box::new(Expression::Value(Value::Variable("fetch".to_string()))),
                    arguments: vec![],
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Call {
                    callee: Box::new(Expression::Value(Value::Variable("fetch".to_string()))),
                    arguments: vec![],
                },
            },
        ];

        let result = infer_variable_names(stmts);

        let names: Vec<_> = result
            .iter()
            .filter_map(|s| {
                if let Statement::Assign {
                    target: AssignTarget::Variable(n),
                    ..
                } = s
                {
                    Some(n.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
    }
}
