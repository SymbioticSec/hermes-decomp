use super::utils::{exprs_equal, is_null, is_undefined};
use crate::ir::{BinaryOp, Constant, Expression, Statement, Value};

// Detect string concatenation patterns and convert to template literals.
// Pattern: "prefix" + x + "suffix" → `prefix${x}suffix`
//
// Hermes often optimizes template literals into simple string concatenations.
// We try to reverse this by looking for chains of `BinaryOp::Add`.
// If we find a chain mixing string constants and expressions, we assume it was a template literal.
pub fn detect_string_concat(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::Assign { target, value } => Statement::Assign {
                target,
                value: transform_string_concat(value),
            },
            Statement::If {
                condition,
                then_body,
                else_body,
            } => Statement::If {
                condition: transform_string_concat(condition),
                then_body: detect_string_concat(then_body),
                else_body: detect_string_concat(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition: transform_string_concat(condition),
                body: detect_string_concat(body),
            },
            Statement::Block(inner) => Statement::Block(detect_string_concat(inner)),
            other => other,
        })
        .collect()
}

fn transform_string_concat(expr: Expression) -> Expression {
    // Only check top-level Add operations for template literal conversion
    if let Expression::Binary {
        op: BinaryOp::Add, ..
    } = &expr
    {
        if let Some((quasis, exprs)) = try_extract_template_literal(&expr) {
            // Only convert if there's at least one string literal AND at least one expression
            let has_string = quasis.iter().any(|s| !s.is_empty());
            if has_string && !exprs.is_empty() {
                return Expression::TemplateLiteral {
                    quasis,
                    expressions: exprs,
                };
            }
        }
    }

    // Recursively transform sub-expressions
    match expr {
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(transform_string_concat(*left)),
            right: Box::new(transform_string_concat(*right)),
        },
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(transform_string_concat(*callee)),
            arguments: arguments.into_iter().map(transform_string_concat).collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(transform_string_concat(*object)),
            property,
            optional,
        },
        other => other,
    }
}

// Try to extract template literal parts from a string concatenation.
// Returns None if there are no string literals in the concatenation.
fn try_extract_template_literal(expr: &Expression) -> Option<(Vec<String>, Vec<Expression>)> {
    let mut quasis = Vec::new();
    let mut expressions = Vec::new();
    let mut current_string = String::new();
    let mut has_string = false;

    fn collect_parts(
        expr: &Expression,
        quasis: &mut Vec<String>,
        expressions: &mut Vec<Expression>,
        current: &mut String,
        has_string: &mut bool,
    ) -> bool {
        match expr {
            Expression::Binary {
                op: BinaryOp::Add,
                left,
                right,
            } => {
                if !collect_parts(left, quasis, expressions, current, has_string) {
                    return false;
                }
                collect_parts(right, quasis, expressions, current, has_string)
            }
            Expression::Value(Value::Constant(Constant::String(s))) => {
                current.push_str(s);
                *has_string = true;
                true
            }
            _ => {
                // Non-string expression - push current string and the expression
                quasis.push(std::mem::take(current));
                expressions.push(expr.clone());
                true
            }
        }
    }

    if collect_parts(
        expr,
        &mut quasis,
        &mut expressions,
        &mut current_string,
        &mut has_string,
    ) {
        // Push the final string part
        quasis.push(current_string);

        // Only return if we found at least one string literal
        if has_string {
            return Some((quasis, expressions));
        }
    }

    None
}

// Detect nullish coalescing: `x == null ? default : x` → `x ?? default`
//
// This pattern is common in transpiled code (Babel/TypeScript).
// We look for a ternary operator where the condition checks if a value is null/undefined,
// and the branches correspond to the value itself or a default.
// Note: In strict nullish coalescing `??`, only null/undefined trigger the default.
pub fn detect_nullish_coalescing(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::Assign { target, value } => Statement::Assign {
                target,
                value: transform_nullish(value),
            },
            Statement::If {
                condition,
                then_body,
                else_body,
            } => Statement::If {
                condition: transform_nullish(condition),
                then_body: detect_nullish_coalescing(then_body),
                else_body: detect_nullish_coalescing(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition: transform_nullish(condition),
                body: detect_nullish_coalescing(body),
            },
            Statement::Block(inner) => Statement::Block(detect_nullish_coalescing(inner)),
            other => other,
        })
        .collect()
}

fn transform_nullish(expr: Expression) -> Expression {
    match expr {
        // x == null ? default : x  →  x ?? default
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            if let Expression::Binary {
                op: BinaryOp::Eq,
                left,
                right,
            } = condition.as_ref()
            {
                if is_null(right) && exprs_equal(left, &else_expr) {
                    return Expression::Binary {
                        op: BinaryOp::NullishCoalesce,
                        left: Box::new(transform_nullish(*else_expr)),
                        right: Box::new(transform_nullish(*then_expr)),
                    };
                }
                if is_null(left) && exprs_equal(right, &else_expr) {
                    return Expression::Binary {
                        op: BinaryOp::NullishCoalesce,
                        left: Box::new(transform_nullish(*else_expr)),
                        right: Box::new(transform_nullish(*then_expr)),
                    };
                }
            }
            Expression::Conditional {
                condition: Box::new(transform_nullish(*condition)),
                then_expr: Box::new(transform_nullish(*then_expr)),
                else_expr: Box::new(transform_nullish(*else_expr)),
            }
        }
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(transform_nullish(*left)),
            right: Box::new(transform_nullish(*right)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(transform_nullish(*operand)),
        },
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(transform_nullish(*callee)),
            arguments: arguments.into_iter().map(transform_nullish).collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(transform_nullish(*object)),
            property,
            optional,
        },
        other => other,
    }
}

// Detect optional chaining patterns: `x == null ? undefined : x.y` → `x?.y`
//
// Optional chaining (`?.`) guards property access against null/undefined.
// Transpilers generate a check before access. We recover the syntax by detecting:
// - A null check on the object.
// - Returning `undefined` if null.
// - Accessing the property if not null.
pub fn detect_optional_chaining(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::Assign { target, value } => Statement::Assign {
                target,
                value: transform_optional_chain(value),
            },
            Statement::If {
                condition,
                then_body,
                else_body,
            } => Statement::If {
                condition: transform_optional_chain(condition),
                then_body: detect_optional_chaining(then_body),
                else_body: detect_optional_chaining(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition: transform_optional_chain(condition),
                body: detect_optional_chaining(body),
            },
            Statement::Block(inner) => Statement::Block(detect_optional_chaining(inner)),
            other => other,
        })
        .collect()
}

fn transform_optional_chain(expr: Expression) -> Expression {
    match expr {
        // x == null ? undefined : x.y  →  x?.y
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            if is_undefined(&then_expr) {
                if let Expression::Binary {
                    op: BinaryOp::Eq,
                    left,
                    right,
                } = condition.as_ref()
                {
                    if is_null(right) || is_null(left) {
                        let check_var = if is_null(right) { left } else { right };
                        // Check if else_expr is accessing a property of check_var
                        if let Expression::Member {
                            object, property, ..
                        } = else_expr.as_ref()
                        {
                            if exprs_equal(check_var, object) {
                                return Expression::Member {
                                    object: Box::new(transform_optional_chain(*object.clone())),
                                    property: property.clone(),
                                    optional: true,
                                };
                            }
                        }
                    }
                }
            }
            Expression::Conditional {
                condition: Box::new(transform_optional_chain(*condition)),
                then_expr: Box::new(transform_optional_chain(*then_expr)),
                else_expr: Box::new(transform_optional_chain(*else_expr)),
            }
        }
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(transform_optional_chain(*left)),
            right: Box::new(transform_optional_chain(*right)),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(transform_optional_chain(*object)),
            property,
            optional,
        },
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(transform_optional_chain(*callee)),
            arguments: arguments
                .into_iter()
                .map(transform_optional_chain)
                .collect(),
        },
        other => other,
    }
}

// Detect logical AND/OR patterns.
//
// In Bytecode/IR, logical operators (`&&`, `||`) are often represented as conditional jumps or ternary expressions.
// - `x || y` is `x ? x : y`
// - `x && y` is `x ? y : x`
// We detect these structural patterns to restore the original operators.
pub fn detect_logical_patterns(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::Assign { target, value } => Statement::Assign {
                target,
                value: transform_logical(value),
            },
            Statement::If {
                condition,
                then_body,
                else_body,
            } => Statement::If {
                condition: transform_logical(condition),
                then_body: detect_logical_patterns(then_body),
                else_body: detect_logical_patterns(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition: transform_logical(condition),
                body: detect_logical_patterns(body),
            },
            other => other,
        })
        .collect()
}

fn transform_logical(expr: Expression) -> Expression {
    match expr {
        // x ? x : y  →  x || y
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            if exprs_equal(&condition, &then_expr) {
                return Expression::Binary {
                    op: BinaryOp::LogicalOr,
                    left: Box::new(transform_logical(*condition)),
                    right: Box::new(transform_logical(*else_expr)),
                };
            }
            // x ? y : x  →  x && y
            if exprs_equal(&condition, &else_expr) {
                return Expression::Binary {
                    op: BinaryOp::LogicalAnd,
                    left: Box::new(transform_logical(*condition)),
                    right: Box::new(transform_logical(*then_expr)),
                };
            }
            Expression::Conditional {
                condition: Box::new(transform_logical(*condition)),
                then_expr: Box::new(transform_logical(*then_expr)),
                else_expr: Box::new(transform_logical(*else_expr)),
            }
        }
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(transform_logical(*left)),
            right: Box::new(transform_logical(*right)),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nullish_coalescing() {
        // x == null ? "default" : x  →  x ?? "default"
        let expr = Expression::Conditional {
            condition: Box::new(Expression::binary(
                BinaryOp::Eq,
                Expression::Value(Value::Register(0)),
                Expression::constant(Constant::Null),
            )),
            then_expr: Box::new(Expression::constant(Constant::String(
                "default".to_string(),
            ))),
            else_expr: Box::new(Expression::Value(Value::Register(0))),
        };

        let result = transform_nullish(expr);

        if let Expression::Binary {
            op: BinaryOp::NullishCoalesce,
            ..
        } = result
        {
            // Success
        } else {
            panic!("Expected nullish coalescing");
        }
    }
}
