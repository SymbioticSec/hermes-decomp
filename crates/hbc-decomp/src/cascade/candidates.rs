// Extraction of the functions worth proposing a name for.
//
// A candidate is a function whose body has one of the shapes verification can
// judge, and whose current name carries no information. Functions that already
// have a recovered name are left out: a proposal could only overwrite something
// the bytecode already established, which the naming doctrine does not allow.
//
// Each candidate is described by what the decompiler observed, never by a guess.
// The observation is what a reader, model or otherwise, has to work from, and it
// is also what verification will re-derive independently when the proposal comes
// back.

use super::Role;
use crate::ir::{Constant, Expression, Statement, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub function_id: u32,
    // The name the function currently renders under, when it has one.
    pub current_name: Option<String>,
    // The shape the body was observed to have, which is the role a proposal for
    // this function has to claim.
    pub shape: Role,
    // For a decoder, how many strings its table holds and a few of them, so the
    // reader can tell what the function is for without the whole array.
    pub sample: Vec<String>,
    pub sample_of: usize,
}

const SAMPLE_SIZE: usize = 8;

pub fn extract(
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    names: &BTreeMap<u32, String>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (id, body) in all_ir {
        let current_name = names.get(id).cloned();
        if current_name.as_deref().is_some_and(is_informative_name) {
            continue;
        }
        let Some((shape, strings)) = classify(body) else {
            continue;
        };
        let sample_of = strings.len();
        let sample = strings.into_iter().take(SAMPLE_SIZE).collect();
        out.push(Candidate {
            function_id: *id,
            current_name,
            shape,
            sample,
            sample_of,
        });
    }
    out
}

// A name the pipeline produced for want of anything better carries no information,
// so a function wearing one is still worth proposing a name for.
fn is_informative_name(name: &str) -> bool {
    !crate::analysis::metro::is_obviously_generic(name)
}

fn classify(body: &[Statement]) -> Option<(Role, Vec<String>)> {
    if let Some(strings) = super::verify::decoded_strings(body) {
        if !strings.is_empty() {
            return Some((Role::StringDecoder, strings));
        }
    }
    let effective: Vec<&Statement> = body
        .iter()
        .filter(|s| !matches!(s, Statement::Comment(_)))
        .collect();
    match effective.as_slice() {
        [] | [Statement::Return(None)] => Some((Role::Noop, Vec::new())),
        [Statement::Return(Some(Expression::Value(Value::Constant(Constant::Undefined))))] => {
            Some((Role::Noop, Vec::new()))
        }
        [Statement::Return(Some(Expression::Value(Value::Parameter(_))))] => {
            Some((Role::Identity, Vec::new()))
        }
        [Statement::Return(Some(Expression::Value(Value::Constant(_))))] => {
            Some((Role::ConstantReturner, Vec::new()))
        }
        _ => None,
    }
}
