mod conversion;
mod exceptions;
mod loops;
mod recovery;

use crate::analysis::loops::LoopInfo;
use crate::ir::{BlockId, CfgExceptionHandler, Expression, Statement};
use std::collections::{BTreeMap, HashSet};

// Context passed through recursive structure recovery, avoiding 8-parameter functions.
pub(crate) struct RecoveryCtx<'a> {
    pub cfg: &'a crate::ir::CFG,
    pub loops: &'a [LoopInfo],
    pub visited: &'a mut HashSet<BlockId>,
    pub try_starts: &'a BTreeMap<BlockId, &'a CfgExceptionHandler>,
}

#[derive(Debug, Clone)]
pub enum Structure {
    Block(BlockId, Vec<Statement>),
    Sequence(Vec<Structure>),
    If {
        condition: Expression,
        then_: Box<Structure>,
        else_: Box<Structure>,
    },
    While {
        condition: Expression,
        body: Box<Structure>,
    },
    DoWhile {
        body: Box<Structure>,
        condition: Expression,
    },
    For {
        init: Box<Structure>,
        condition: Expression,
        update: Box<Structure>,
        body: Box<Structure>,
    },
    Switch {
        discriminant: Expression,
        cases: Vec<(Expression, Structure)>,
        default: Box<Structure>,
    },
    TryCatch {
        try_body: Box<Structure>,
        catch_param: Option<String>,
        catch_body: Box<Structure>,
    },
    Return(Option<Expression>),
    Break(Option<String>),
    Continue(Option<String>),
    Label(String, Box<Structure>),
}

pub struct StructureAnalysis {
    pub root: Structure,
    pub loops: Vec<LoopInfo>,
}

impl StructureAnalysis {
    pub fn analyze(cfg: &crate::ir::CFG) -> Self {
        let (root, loops) = recovery::analyze(cfg);
        StructureAnalysis { root, loops }
    }
}
