pub mod display;
mod ops;

pub use ops::*;

use super::types::{Constant, FunctionId, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropertyKey {
    Ident(String),
    Computed(Box<Expression>),
    String(String),
    Index(i64),
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expression {
    Value(Value),
    Binary {
        op: BinaryOp,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expression>,
    },
    Conditional {
        condition: Box<Expression>,
        then_expr: Box<Expression>,
        else_expr: Box<Expression>,
    },
    Member {
        object: Box<Expression>,
        property: PropertyKey,
        optional: bool,
    },
    Call {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
    },
    New {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
    },
    Array {
        elements: Vec<Option<Expression>>,
    },
    Object {
        properties: Vec<ObjectProperty>,
    },
    Function {
        id: FunctionId,
        name: Option<String>,
        is_arrow: bool,
        is_async: bool,
        is_generator: bool,
    },
    Assignment {
        target: Box<Expression>,
        value: Box<Expression>,
    },
    Spread(Box<Expression>),
    TemplateLiteral {
        quasis: Vec<String>,
        expressions: Vec<Expression>,
    },
    RegExp {
        pattern: String,
        flags: String,
    },
    Yield {
        value: Box<Expression>,
        delegate: bool,
    },
    Await(Box<Expression>),
    JSXElement {
        tag: String,
        attributes: Vec<(String, Expression)>,
        children: Vec<Expression>,
    },
    Unknown {
        opcode: String,
        operands: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectProperty {
    pub key: PropertyKey,
    pub value: Expression,
}

impl Expression {
    pub fn constant(c: Constant) -> Self {
        Expression::Value(Value::Constant(c))
    }

    pub fn register(r: u32) -> Self {
        Expression::Value(Value::Register(r))
    }

    pub fn binary(op: BinaryOp, left: Expression, right: Expression) -> Self {
        Expression::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn unary(op: UnaryOp, operand: Expression) -> Self {
        Expression::Unary {
            op,
            operand: Box::new(operand),
        }
    }

    pub fn member(object: Expression, prop: &str) -> Self {
        Expression::Member {
            object: Box::new(object),
            property: PropertyKey::Ident(prop.to_string()),
            optional: false,
        }
    }

    pub fn index(object: Expression, key: Expression) -> Self {
        Expression::Member {
            object: Box::new(object),
            property: PropertyKey::Computed(Box::new(key)),
            optional: false,
        }
    }

    pub fn call(callee: Expression, arguments: Vec<Expression>) -> Self {
        Expression::Call {
            callee: Box::new(callee),
            arguments,
        }
    }

    pub fn is_simple(&self) -> bool {
        matches!(self, Expression::Value(_))
    }

    pub fn has_side_effects(&self) -> bool {
        match self {
            Expression::Value(_) => false,
            Expression::Binary { left, right, .. } => {
                left.has_side_effects() || right.has_side_effects()
            }
            Expression::Unary { operand, .. } => operand.has_side_effects(),
            Expression::Member {
                object, property, ..
            } => {
                object.has_side_effects()
                    || match property {
                        PropertyKey::Computed(k) => k.has_side_effects(),
                        _ => false,
                    }
            }
            Expression::Array { elements } => {
                elements.iter().flatten().any(|e| e.has_side_effects())
            }
            Expression::Object { properties } => properties.iter().any(|p| {
                p.value.has_side_effects()
                    || match &p.key {
                        PropertyKey::Computed(k) => k.has_side_effects(),
                        _ => false,
                    }
            }),
            Expression::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                condition.has_side_effects()
                    || then_expr.has_side_effects()
                    || else_expr.has_side_effects()
            }
            Expression::TemplateLiteral { expressions, .. } => {
                expressions.iter().any(|e| e.has_side_effects())
            }
            Expression::Call { .. }
            | Expression::New { .. }
            | Expression::Assignment { .. }
            | Expression::Yield { .. }
            | Expression::Await(_)
            | Expression::Unknown { .. } => true,
            _ => false,
        }
    }
}
