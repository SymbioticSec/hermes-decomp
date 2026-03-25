mod display;

use super::{BlockId, Expression};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VarKind {
    Const,
    #[default]
    Let,
    Var,
}

impl std::fmt::Display for VarKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VarKind::Const => write!(f, "const"),
            VarKind::Let => write!(f, "let"),
            VarKind::Var => write!(f, "var"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Statement {
    Expr(Expression),

    Let {
        name: String,
        value: Expression,
        kind: VarKind,
    },

    Assign {
        target: AssignTarget,
        value: Expression,
    },

    Delete {
        target: Expression,
        result: Option<u32>,
    },

    Return(Option<Expression>),

    Throw(Expression),

    Debugger,

    Comment(String),

    Break(Option<String>),

    Continue(Option<String>),

    Goto(BlockId),

    CondGoto {
        condition: Expression,
        target: BlockId,
        fallthrough: BlockId,
    },

    If {
        condition: Expression,
        then_body: Vec<Statement>,
        else_body: Vec<Statement>,
    },

    While {
        condition: Expression,
        body: Vec<Statement>,
    },

    DoWhile {
        body: Vec<Statement>,
        condition: Expression,
    },

    For {
        init: Option<Box<Statement>>,
        condition: Option<Expression>,
        update: Option<Box<Statement>>,
        body: Vec<Statement>,
    },

    Switch {
        discriminant: Expression,
        cases: Vec<(Expression, Vec<Statement>)>,
        default: Option<Vec<Statement>>,
    },

    ForOf {
        variable: String,
        iterable: Expression,
        body: Vec<Statement>,
    },

    ForIn {
        variable: String,
        object: Expression,
        body: Vec<Statement>,
    },

    TryCatch {
        try_body: Vec<Statement>,
        catch_param: Option<String>,
        catch_body: Vec<Statement>,
        finally_body: Vec<Statement>,
    },

    Block(Vec<Statement>),

    Class {
        name: String,
        super_class: Option<Expression>,
        constructor: Option<Box<Statement>>,
        methods: Vec<ClassMethod>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassMethod {
    pub key: String,
    pub value: Expression,
    pub body: Option<Vec<Statement>>,
    pub is_static: bool,
    pub kind: MethodKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MethodKind {
    Constructor,
    Method,
    Getter,
    Setter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AssignTarget {
    Variable(String),

    Register(u32),

    Member {
        object: Expression,
        property: String,
    },

    Index {
        object: Expression,
        key: Expression,
    },

    ClosureVar {
        level: u32,
        slot: u32,
    },

    DestructuringArray(Vec<Option<(AssignTarget, Option<Expression>)>>),

    DestructuringArrayRest {
        elements: Vec<Option<(AssignTarget, Option<Expression>)>>,
        rest: Box<AssignTarget>,
    },

    DestructuringObject(Vec<(String, AssignTarget, Option<Expression>)>),

    DestructuringObjectRest {
        properties: Vec<(String, AssignTarget, Option<Expression>)>,
        rest: Box<AssignTarget>,
    },

    Rest(Box<AssignTarget>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Terminator {
    Jump(BlockId),

    Branch {
        condition: Expression,
        true_target: BlockId,
        false_target: BlockId,
    },

    Return(Option<Expression>),

    Throw(Expression),

    Switch {
        value: Expression,
        cases: Vec<(Expression, BlockId)>,
        default: BlockId,
    },

    None,
}

impl Statement {
    pub fn expr(e: Expression) -> Self {
        Statement::Expr(e)
    }

    pub fn let_stmt(name: impl Into<String>, value: Expression) -> Self {
        Statement::Let {
            name: name.into(),
            value,
            kind: VarKind::Let,
        }
    }

    pub fn const_stmt(name: impl Into<String>, value: Expression) -> Self {
        Statement::Let {
            name: name.into(),
            value,
            kind: VarKind::Const,
        }
    }

    pub fn var_stmt(name: impl Into<String>, value: Expression) -> Self {
        Statement::Let {
            name: name.into(),
            value,
            kind: VarKind::Var,
        }
    }

    pub fn assign_var(name: impl Into<String>, value: Expression) -> Self {
        Statement::Assign {
            target: AssignTarget::Variable(name.into()),
            value,
        }
    }

    pub fn assign_reg(reg: u32, value: Expression) -> Self {
        Statement::Assign {
            target: AssignTarget::Register(reg),
            value,
        }
    }

    pub fn ret(value: Option<Expression>) -> Self {
        Statement::Return(value)
    }
}

impl Terminator {
    pub fn jump(target: BlockId) -> Self {
        Terminator::Jump(target)
    }

    pub fn branch(cond: Expression, true_: BlockId, false_: BlockId) -> Self {
        Terminator::Branch {
            condition: cond,
            true_target: true_,
            false_target: false_,
        }
    }

    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Jump(t) => vec![*t],
            Terminator::Branch {
                true_target,
                false_target,
                ..
            } => {
                vec![*true_target, *false_target]
            }
            Terminator::Switch { cases, default, .. } => {
                let mut targets: Vec<_> = cases.iter().map(|(_, t)| *t).collect();
                targets.push(*default);
                targets
            }
            Terminator::Return(_) | Terminator::Throw(_) | Terminator::None => vec![],
        }
    }

    pub fn is_return(&self) -> bool {
        matches!(self, Terminator::Return(_))
    }
}
