// Deterministic verification of proposals against the bytecode.
//
// Nothing here consults a model or a network. A proposal is a claim about what a
// function is, and each check below is a way the binary can refute that claim. A
// claim that survives every applicable check is believed, and one that does not is
// recorded with the reason.

use super::{Artifact, BundleFingerprint, Proposal, Rejection, Role, VerifiedArtifact};
use crate::ir::{Constant, Expression, PropertyKey, Statement, Value};
use std::collections::{BTreeMap, HashSet};

pub fn verify_against_file(
    artifact: &Artifact,
    file: &crate::BytecodeFile,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
) -> VerifiedArtifact {
    let table: HashSet<&str> = file.strings.iter().map(|s| s.value.as_str()).collect();
    verify(artifact, &BundleFingerprint::of(file), &table, all_ir)
}

// Verification depends on the bundle's data, not on how it was read, so the whole
// chain can be exercised without a `.hbc` on disk.
pub fn verify(
    artifact: &Artifact,
    fingerprint: &BundleFingerprint,
    table: &HashSet<&str>,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
) -> VerifiedArtifact {
    let mut out = VerifiedArtifact::default();

    if artifact.version != super::ARTIFACT_VERSION {
        let reason = Rejection::ArtifactVersion {
            found: artifact.version,
        };
        for proposal in &artifact.proposals {
            out.rejected.push((proposal.clone(), reason.clone()));
        }
        return out;
    }
    if artifact.bundle != *fingerprint {
        for proposal in &artifact.proposals {
            out.rejected
                .push((proposal.clone(), Rejection::WrongBundle));
        }
        return out;
    }

    for proposal in &artifact.proposals {
        match check(proposal, all_ir, table) {
            Ok(()) => {
                out.names
                    .insert(proposal.function_id, proposal.name.clone());
            }
            Err(reason) => out.rejected.push((proposal.clone(), reason)),
        }
    }
    out
}

fn check(
    proposal: &Proposal,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    table: &HashSet<&str>,
) -> Result<(), Rejection> {
    if !crate::util::is_valid_identifier(&proposal.name) {
        return Err(Rejection::NameNotAnIdentifier);
    }
    if crate::constants::is_reserved_word(&proposal.name) {
        return Err(Rejection::NameIsReserved);
    }
    let body = all_ir
        .get(&proposal.function_id)
        .ok_or(Rejection::UnknownFunction)?;

    match proposal.role {
        Role::Noop => confirm_noop(body),
        Role::Identity => confirm_identity(body),
        Role::ConstantReturner => confirm_constant_returner(body),
        Role::StringDecoder => confirm_string_decoder(body, table),
    }
}

// What the body actually is, for a rejection message that says something useful.
fn describe(body: &[Statement]) -> &'static str {
    match effective_statements(body).as_slice() {
        [] => "empty",
        [Statement::Return(None)] => "a bare return",
        [Statement::Return(Some(_))] => "a single return of something else",
        [_] => "a single statement of another kind",
        _ => "more than one statement",
    }
}

// Comments and empty blocks carry nothing, so a body is judged on what is left.
fn effective_statements(body: &[Statement]) -> Vec<&Statement> {
    body.iter()
        .filter(|s| match s {
            Statement::Comment(_) => false,
            Statement::Block(inner) => !inner.is_empty(),
            _ => true,
        })
        .collect()
}

fn confirm_noop(body: &[Statement]) -> Result<(), Rejection> {
    let stmts = effective_statements(body);
    let ok = matches!(
        stmts.as_slice(),
        [] | [Statement::Return(None)]
            | [Statement::Return(Some(Expression::Value(Value::Constant(
                Constant::Undefined
            ))))]
    );
    if ok {
        Ok(())
    } else {
        Err(Rejection::RoleNotConfirmed {
            observed: describe(body),
        })
    }
}

fn confirm_identity(body: &[Statement]) -> Result<(), Rejection> {
    let stmts = effective_statements(body);
    if let [Statement::Return(Some(Expression::Value(Value::Parameter(_))))] = stmts.as_slice() {
        return Ok(());
    }
    Err(Rejection::RoleNotConfirmed {
        observed: describe(body),
    })
}

fn confirm_constant_returner(body: &[Statement]) -> Result<(), Rejection> {
    let stmts = effective_statements(body);
    if let [Statement::Return(Some(Expression::Value(Value::Constant(_))))] = stmts.as_slice() {
        return Ok(());
    }
    Err(Rejection::RoleNotConfirmed {
        observed: describe(body),
    })
}

// The check the paper leans on: a claimed decoder is decoded here, and every
// string it can yield has to be one this binary actually holds. A function that
// returns strings from somewhere else is not this bundle's decoder, whatever it
// looks like.
fn confirm_string_decoder(body: &[Statement], table: &HashSet<&str>) -> Result<(), Rejection> {
    let strings = decoded_strings(body).ok_or(Rejection::RoleNotConfirmed {
        observed: describe(body),
    })?;
    if strings.is_empty() {
        return Err(Rejection::RoleNotConfirmed {
            observed: "an empty table",
        });
    }
    let missing = strings
        .iter()
        .filter(|s| !table.contains(s.as_str()))
        .count();
    if missing > 0 {
        return Err(Rejection::StringsAbsentFromTable {
            missing,
            checked: strings.len(),
        });
    }
    Ok(())
}

// Every string a body of the decoder shape can return, or `None` when it does not
// have that shape. The shape is a return of an indexed read whose base is an array
// of string constants, either written inline or bound in the same body.
pub(super) fn decoded_strings(body: &[Statement]) -> Option<Vec<String>> {
    let stmts = effective_statements(body);
    let returned = stmts.iter().rev().find_map(|s| match s {
        Statement::Return(Some(expr)) => Some(expr),
        _ => None,
    })?;

    let Expression::Member {
        object, property, ..
    } = returned
    else {
        return None;
    };
    // An indexed read. A fixed property name is a field access, not a decoder.
    match property {
        PropertyKey::Computed(_) | PropertyKey::Index(_) => {}
        _ => return None,
    }

    let array = resolve_array(object, &stmts)?;
    let mut out = Vec::with_capacity(array.len());
    for element in array {
        // A hole, or anything that is not a string constant, and this is not a
        // string table.
        match element {
            Some(Expression::Value(Value::Constant(Constant::String(s)))) => out.push(s.clone()),
            _ => return None,
        }
    }
    Some(out)
}

fn resolve_array<'a>(
    expr: &'a Expression,
    stmts: &[&'a Statement],
) -> Option<&'a Vec<Option<Expression>>> {
    match expr {
        Expression::Array { elements } => Some(elements),
        Expression::Value(Value::Binding(crate::ir::Binding::Variable(name))) => {
            stmts.iter().rev().find_map(|s| match s {
                Statement::Let { name: n, value, .. } if n == name => match value {
                    Expression::Array { elements } => Some(elements),
                    _ => None,
                },
                Statement::Assign {
                    target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(n)),
                    value,
                } if n == name => match value {
                    Expression::Array { elements } => Some(elements),
                    _ => None,
                },
                _ => None,
            })
        }
        _ => None,
    }
}
