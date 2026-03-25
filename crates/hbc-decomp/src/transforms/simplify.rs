use crate::ir::{BinaryOp, Constant, Expression, MutVisitor, Statement, UnaryOp, Value};
use std::mem;

pub fn simplify_expr(mut expr: Expression) -> Expression {
    let mut simplifier = Simplifier;
    simplifier.visit_expression(&mut expr);
    expr
}

pub fn simplify_statements(stmts: &mut Vec<Statement>) {
    let mut simplifier = Simplifier;
    simplifier.visit_statement_list(stmts);
}

pub fn simplify_stmt(mut stmt: Statement) -> Statement {
    let mut simplifier = Simplifier;
    simplifier.visit_statement(&mut stmt);
    stmt
}

struct Simplifier;

impl MutVisitor for Simplifier {
    fn visit_expression(&mut self, expr: &mut Expression) {
        // Post-order: simplify children first
        self.walk_expression(expr);

        let new_expr = match expr {
            Expression::Binary { op, left, right } => {
                let left = mem::replace(&mut **left, Expression::constant(Constant::Undefined));
                let right = mem::replace(&mut **right, Expression::constant(Constant::Undefined));
                Some(simplify_binary(*op, left, right))
            }
            Expression::Unary { op, operand } => {
                let operand = mem::replace(&mut **operand, Expression::constant(Constant::Undefined));
                Some(simplify_unary(*op, operand))
            }
            Expression::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                match &**condition {
                    Expression::Value(Value::Constant(Constant::Bool(true))) => {
                        Some(mem::replace(&mut **then_expr, Expression::constant(Constant::Undefined)))
                    }
                    Expression::Value(Value::Constant(Constant::Bool(false))) => {
                        Some(mem::replace(&mut **else_expr, Expression::constant(Constant::Undefined)))
                    }
                    _ => None,
                }
            }
            _ => None,
        };

        if let Some(new_e) = new_expr {
            *expr = new_e;
        }
    }
}

fn simplify_binary(op: BinaryOp, left: Expression, right: Expression) -> Expression {
    // Constant folding for integers
    if let (
        Expression::Value(Value::Constant(Constant::Integer(l))),
        Expression::Value(Value::Constant(Constant::Integer(r))),
    ) = (&left, &right)
    {
        if let Some(result) = fold_int_binary(op, *l, *r) {
            return Expression::constant(Constant::Integer(result));
        }
    }

    // Identity simplifications
    match op {
        BinaryOp::Add if is_zero(&right) => return left,
        BinaryOp::Add if is_zero(&left) => return right,
        BinaryOp::Sub if is_zero(&right) => return left,
        BinaryOp::Mul if is_one(&right) => return left,
        BinaryOp::Mul if is_one(&left) => return right,
        BinaryOp::Mul if is_zero(&left) || is_zero(&right) => {
            return Expression::constant(Constant::Integer(0));
        }
        BinaryOp::Div if is_one(&right) => return left,
        _ => {}
    }

    Expression::binary(op, left, right)
}

fn simplify_unary(op: UnaryOp, operand: Expression) -> Expression {
    // Double negation
    if op == UnaryOp::Not {
        if let Expression::Unary {
            op: UnaryOp::Not,
            operand: inner,
        } = operand
        {
            return *inner;
        }
    }

    // Constant folding
    if let Expression::Value(Value::Constant(c)) = &operand {
        match (op, c) {
            (UnaryOp::Not, Constant::Bool(b)) => {
                return Expression::constant(Constant::Bool(!b));
            }
            (UnaryOp::Neg, Constant::Integer(i)) => {
                return Expression::constant(Constant::Integer(-i));
            }
            _ => {}
        }
    }

    Expression::unary(op, operand)
}

fn fold_int_binary(op: BinaryOp, l: i32, r: i32) -> Option<i32> {
    match op {
        BinaryOp::Add => l.checked_add(r),
        BinaryOp::Sub => l.checked_sub(r),
        BinaryOp::Mul => l.checked_mul(r),
        BinaryOp::Div if r != 0 => l.checked_div(r),
        BinaryOp::Mod if r != 0 => l.checked_rem(r),
        BinaryOp::BitAnd => Some(l & r),
        BinaryOp::BitOr => Some(l | r),
        BinaryOp::BitXor => Some(l ^ r),
        BinaryOp::Shl => Some(l << (r & 31)),
        BinaryOp::Shr => Some(l >> (r & 31)),
        _ => None,
    }
}

use super::patterns::utils::is_zero;

fn is_one(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Constant(Constant::Integer(1))) => true,
        Expression::Value(Value::Constant(Constant::Number(n))) => *n == 1.0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_folding() {
        let expr = Expression::binary(
            BinaryOp::Add,
            Expression::constant(Constant::Integer(2)),
            Expression::constant(Constant::Integer(3)),
        );
        let result = simplify_expr(expr);
        assert_eq!(result, Expression::constant(Constant::Integer(5)));
    }

    #[test]
    fn test_identity_elimination() {
        let expr = Expression::binary(
            BinaryOp::Add,
            Expression::register(0),
            Expression::constant(Constant::Integer(0)),
        );
        let result = simplify_expr(expr);
        assert_eq!(result, Expression::register(0));
    }
}
