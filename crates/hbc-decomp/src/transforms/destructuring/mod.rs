mod arrays;
mod iterator;
mod transformer;
mod utils;
mod v98;

use crate::ir::Statement;

pub use v98::reconstruct_v98_array_destructuring;
use arrays::transform_rest_destructuring;
pub use iterator::detect_iterator_destructuring;
pub(crate) use transformer::transform_destructuring;

pub fn detect_destructuring(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut stmts = stmts;
    transform_destructuring(&mut stmts);
    transform_rest_destructuring(&mut stmts);
    stmts.into_iter().map(transform_nested).collect()
}

fn transform_nested(stmt: Statement) -> Statement {
    match stmt {
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition,
            then_body: detect_destructuring(then_body),
            else_body: detect_destructuring(else_body),
        },
        Statement::While { condition, body } => Statement::While {
            condition,
            body: detect_destructuring(body),
        },
        Statement::For {
            init,
            condition,
            update,
            body,
        } => Statement::For {
            init: init.map(|s| Box::new(transform_nested(*s))),
            condition,
            update: update.map(|s| Box::new(transform_nested(*s))),
            body: detect_destructuring(body),
        },
        Statement::ForOf {
            variable,
            iterable,
            body,
        } => Statement::ForOf {
            variable,
            iterable,
            body: detect_destructuring(body),
        },
        Statement::ForIn {
            variable,
            object,
            body,
        } => Statement::ForIn {
            variable,
            object,
            body: detect_destructuring(body),
        },
        Statement::Block(inner) => Statement::Block(detect_destructuring(inner)),
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => Statement::TryCatch {
            try_body: detect_destructuring(try_body),
            catch_param,
            catch_body: detect_destructuring(catch_body),
            finally_body: detect_destructuring(finally_body),
        },
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => Statement::Switch {
            discriminant,
            cases: cases
                .into_iter()
                .map(|(expr, body)| (expr, detect_destructuring(body)))
                .collect(),
            default: default.map(detect_destructuring),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Constant, Expression, PropertyKey, Value};

    #[test]
    fn test_object_destructuring_detection() {
        // r1 = obj.x; r2 = obj.y;
        let obj = Expression::Value(Value::Register(0));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("x".to_string()),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(2),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("y".to_string()),
                    optional: false,
                },
            },
        ];

        let result = detect_destructuring(stmts);

        assert_eq!(result.len(), 1);
        if let Statement::Assign {
            target: AssignTarget::DestructuringObject(props),
            ..
        } = &result[0]
        {
            assert_eq!(props.len(), 2);
            assert_eq!(props[0].0, "x");
            assert_eq!(props[1].0, "y");
        } else {
            panic!("Expected destructuring object, got: {:?}", result[0]);
        }
    }

    #[test]
    fn test_array_destructuring_detection() {
        // r1 = arr[0]; r2 = arr[1]; r3 = arr[2];
        let arr = Expression::Value(Value::Register(0));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Member {
                    object: Box::new(arr.clone()),
                    property: PropertyKey::Index(0),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(2),
                value: Expression::Member {
                    object: Box::new(arr.clone()),
                    property: PropertyKey::Index(1),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(3),
                value: Expression::Member {
                    object: Box::new(arr.clone()),
                    property: PropertyKey::Index(2),
                    optional: false,
                },
            },
        ];

        let result = detect_destructuring(stmts);

        assert_eq!(result.len(), 1);
        if let Statement::Assign {
            target: AssignTarget::DestructuringArray(elements),
            ..
        } = &result[0]
        {
            assert_eq!(elements.len(), 3);
            assert!(elements.iter().all(|e| e.is_some()));
        } else {
            panic!("Expected destructuring array, got: {:?}", result[0]);
        }
    }

    #[test]
    fn test_computed_index_destructuring() {
        // r1 = arr[0]; r2 = arr[1]; using Computed property keys
        let arr = Expression::Value(Value::Register(0));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Member {
                    object: Box::new(arr.clone()),
                    property: PropertyKey::Computed(Box::new(Expression::constant(
                        Constant::Integer(0),
                    ))),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(2),
                value: Expression::Member {
                    object: Box::new(arr.clone()),
                    property: PropertyKey::Computed(Box::new(Expression::constant(
                        Constant::Integer(1),
                    ))),
                    optional: false,
                },
            },
        ];

        let result = detect_destructuring(stmts);

        assert_eq!(result.len(), 1);
        if let Statement::Assign {
            target: AssignTarget::DestructuringArray(elements),
            ..
        } = &result[0]
        {
            assert_eq!(elements.len(), 2);
        } else {
            panic!("Expected destructuring array, got: {:?}", result[0]);
        }
    }

    #[test]
    fn test_non_consecutive_indices_not_destructured() {
        // r1 = arr[0]; r2 = arr[2]; (skip index 1)
        let arr = Expression::Value(Value::Register(0));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Member {
                    object: Box::new(arr.clone()),
                    property: PropertyKey::Index(0),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(2),
                value: Expression::Member {
                    object: Box::new(arr.clone()),
                    property: PropertyKey::Index(2),
                    optional: false,
                },
            },
        ];

        let result = detect_destructuring(stmts);

        // Should not be destructured since indices are not consecutive
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_single_access_not_destructured() {
        // r1 = obj.x; (single property - should not destructure)
        let obj = Expression::Value(Value::Register(0));
        let stmts = vec![Statement::Assign {
            target: AssignTarget::Register(1),
            value: Expression::Member {
                object: Box::new(obj),
                property: PropertyKey::Ident("x".to_string()),
                optional: false,
            },
        }];

        let result = detect_destructuring(stmts);

        assert_eq!(result.len(), 1);
        // Should remain as single property access
        if let Statement::Assign {
            target: AssignTarget::DestructuringObject(_),
            ..
        } = &result[0]
        {
            panic!("Should not destructure single property access");
        }
    }

    #[test]
    fn test_different_objects_not_destructured() {
        // r1 = obj1.x; r2 = obj2.y; (different objects)
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Member {
                    object: Box::new(Expression::Value(Value::Register(0))),
                    property: PropertyKey::Ident("x".to_string()),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(2),
                value: Expression::Member {
                    object: Box::new(Expression::Value(Value::Register(10))),
                    property: PropertyKey::Ident("y".to_string()),
                    optional: false,
                },
            },
        ];

        let result = detect_destructuring(stmts);

        // Should remain as two separate assignments
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_nested_destructuring() {
        // Inside an if block
        let obj = Expression::Value(Value::Register(0));
        let stmts = vec![Statement::If {
            condition: Expression::constant(Constant::Bool(true)),
            then_body: vec![
                Statement::Assign {
                    target: AssignTarget::Register(1),
                    value: Expression::Member {
                        object: Box::new(obj.clone()),
                        property: PropertyKey::Ident("a".to_string()),
                        optional: false,
                    },
                },
                Statement::Assign {
                    target: AssignTarget::Register(2),
                    value: Expression::Member {
                        object: Box::new(obj.clone()),
                        property: PropertyKey::Ident("b".to_string()),
                        optional: false,
                    },
                },
            ],
            else_body: vec![],
        }];

        let result = detect_destructuring(stmts);

        assert_eq!(result.len(), 1);
        if let Statement::If { then_body, .. } = &result[0] {
            assert_eq!(then_body.len(), 1);
            if let Statement::Assign {
                target: AssignTarget::DestructuringObject(props),
                ..
            } = &then_body[0]
            {
                assert_eq!(props.len(), 2);
            } else {
                panic!("Expected destructuring object in then body");
            }
        } else {
            panic!("Expected if statement");
        }
    }

    #[test]
    fn test_optional_member_not_destructured() {
        // r1 = obj?.x; r2 = obj?.y; (optional chaining should not destructure)
        let obj = Expression::Value(Value::Register(0));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Register(1),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("x".to_string()),
                    optional: true, // Optional!
                },
            },
            Statement::Assign {
                target: AssignTarget::Register(2),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("y".to_string()),
                    optional: true,
                },
            },
        ];

        let result = detect_destructuring(stmts);

        // Should not destructure because of optional chaining
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_three_properties_destructuring() {
        let obj = Expression::Value(Value::Variable("data".to_string()));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Variable("x".to_string()),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("x".to_string()),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Variable("y".to_string()),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("y".to_string()),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Variable("z".to_string()),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("z".to_string()),
                    optional: false,
                },
            },
        ];

        let result = detect_destructuring(stmts);

        assert_eq!(result.len(), 1);
        if let Statement::Assign {
            target: AssignTarget::DestructuringObject(props),
            value,
        } = &result[0]
        {
            assert_eq!(props.len(), 3);
            assert_eq!(props[0].0, "x");
            assert_eq!(props[1].0, "y");
            assert_eq!(props[2].0, "z");
            // Check value is the original object
            if let Expression::Value(Value::Variable(name)) = value {
                assert_eq!(name, "data");
            } else {
                panic!("Expected variable 'data'");
            }
        } else {
            panic!("Expected destructuring object with 3 props");
        }
    }

    #[test]
    fn test_default_value_destructuring() {
        use crate::ir::{BinaryOp, Constant, Value};
        let obj = Expression::Value(Value::Variable("param".to_string()));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Variable("x".to_string()),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("x".to_string()),
                    optional: false,
                },
            },
            Statement::If {
                condition: Expression::Binary {
                    op: BinaryOp::StrictEq,
                    left: Box::new(Expression::Value(Value::Variable("x".to_string()))),
                    right: Box::new(Expression::constant(Constant::Undefined)),
                },
                then_body: vec![Statement::Assign {
                    target: AssignTarget::Variable("x".to_string()),
                    value: Expression::constant(Constant::Integer(42)),
                }],
                else_body: vec![],
            },
            Statement::Assign {
                target: AssignTarget::Variable("y".to_string()),
                value: Expression::Member {
                    object: Box::new(obj.clone()),
                    property: PropertyKey::Ident("y".to_string()),
                    optional: false,
                },
            },
        ];

        let mut stmts_mut = stmts.clone();
        crate::transforms::destructuring::transformer::transform_destructuring(&mut stmts_mut);

        assert_eq!(stmts_mut.len(), 1);
        if let Statement::Assign {
            target: AssignTarget::DestructuringObject(props),
            ..
        } = &stmts_mut[0]
        {
            assert_eq!(props.len(), 2);
            assert_eq!(props[0].0, "x");
            // Check default value for `x` was extracted
            if let Some(Expression::Value(Value::Constant(Constant::Integer(val)))) = &props[0].2 {
                assert_eq!(*val, 42);
            } else {
                panic!("Expected default value 42 for x");
            }
            assert_eq!(props[1].0, "y");
            assert!(props[1].2.is_none());
        } else {
            panic!("Expected destructuring object");
        }
    }
}
