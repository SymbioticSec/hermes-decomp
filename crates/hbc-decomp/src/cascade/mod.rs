// Proposal and verification, after the CASCADE paper (arXiv 2507.17691).
//
// The paper's idea is a split: a model is good at recognising what an obfuscated
// helper is for, and terrible at being trusted about it, so it only ever proposes
// an identity and the deterministic half decides whether to believe it. That split
// is what makes the approach compatible with the naming doctrine here, which
// forbids guessing a name.
//
// The chain is extraction, proposal, verification, application:
//
//   1. `candidates` pulls functions with a shape worth naming out of the IR and
//      describes each one in terms a reader can judge.
//   2. A model proposes a name and a role for some of them, offline. Nothing in
//      this module talks to a network, and the decompile path never does.
//   3. `verify` checks every proposal against the bytecode. A proposal survives
//      only when the binary confirms it.
//   4. `apply` renames the confirmed ones. A rejected proposal changes nothing and
//      the function keeps the generic name it had.
//
// The artifact is bound to the bundle it was produced for, so a proposal for one
// build cannot be applied to another and rename the wrong function.

use serde::{Deserialize, Serialize};

pub mod candidates;
pub mod verify;

#[cfg(test)]
mod tests;

pub const ARTIFACT_VERSION: u32 = 1;

// Identifies the bundle an artifact was produced for. Function ids are positions
// in a particular build and mean nothing in another one, so applying an artifact
// to a bundle it was not made for would rename by coincidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFingerprint {
    pub hbc_version: u32,
    pub function_count: usize,
    pub string_count: usize,
    // FNV-1a over the string table, in order. Cheap, deterministic, and enough to
    // tell two builds apart when the counts happen to match.
    pub string_digest: u64,
}

impl BundleFingerprint {
    pub fn of(file: &crate::BytecodeFile) -> Self {
        let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
        for entry in &file.strings {
            for byte in entry.value.as_bytes() {
                digest ^= u64::from(*byte);
                digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
            }
            digest ^= 0xff;
            digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
        }
        BundleFingerprint {
            hbc_version: file.header.version,
            function_count: file.function_headers.len(),
            string_count: file.strings.len(),
            string_digest: digest,
        }
    }
}

// What a proposal claims a function is. Every role here is one the bytecode can
// confirm or refute on its own. A role that could only be taken on trust does not
// belong in this list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    // Returns an element of an array of string constants, which is the shape a
    // string obfuscator leaves behind. Confirmed by decoding the array and
    // checking every string it yields against the bundle's string table.
    StringDecoder,
    // Returns the same constant whatever it is given.
    ConstantReturner,
    // Returns one of its own parameters unchanged.
    Identity,
    // Returns nothing and does nothing observable.
    Noop,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::StringDecoder => "string_decoder",
            Role::ConstantReturner => "constant_returner",
            Role::Identity => "identity",
            Role::Noop => "noop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub function_id: u32,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub version: u32,
    pub bundle: BundleFingerprint,
    pub proposals: Vec<Proposal>,
}

// Why a proposal was not believed. Kept as data so the confirmation rate can be
// reported per reason rather than as a single number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    ArtifactVersion { found: u32 },
    WrongBundle,
    UnknownFunction,
    NameNotAnIdentifier,
    NameIsReserved,
    // The function does not have the shape the role claims.
    RoleNotConfirmed { observed: &'static str },
    // A decoder was claimed, and some of the strings it yields are not in the
    // bundle's string table, so it does not decode this binary's strings.
    StringsAbsentFromTable { missing: usize, checked: usize },
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::ArtifactVersion { found } => {
                write!(f, "artifact version {found}, expected {ARTIFACT_VERSION}")
            }
            Rejection::WrongBundle => write!(f, "artifact was produced for another bundle"),
            Rejection::UnknownFunction => write!(f, "no such function in this bundle"),
            Rejection::NameNotAnIdentifier => write!(f, "proposed name is not an identifier"),
            Rejection::NameIsReserved => write!(f, "proposed name is a reserved word"),
            Rejection::RoleNotConfirmed { observed } => {
                write!(f, "role not confirmed, the body is {observed}")
            }
            Rejection::StringsAbsentFromTable { missing, checked } => write!(
                f,
                "{missing} of {checked} decoded strings are not in the string table"
            ),
        }
    }
}

#[derive(Debug, Default)]
pub struct VerifiedArtifact {
    // Function id to the confirmed name.
    pub names: std::collections::BTreeMap<u32, String>,
    pub rejected: Vec<(Proposal, Rejection)>,
}

impl VerifiedArtifact {
    pub fn confirmed(&self) -> usize {
        self.names.len()
    }

    pub fn total(&self) -> usize {
        self.names.len() + self.rejected.len()
    }

    // Share of proposals the bytecode confirmed, which is the number worth
    // watching: a chain that confirms everything is not verifying anything.
    pub fn confirmation_rate(&self) -> f64 {
        if self.total() == 0 {
            return 0.0;
        }
        self.names.len() as f64 / self.total() as f64
    }
}

// Set the confirmed names on the IR. A function keeps whatever name it had unless
// a proposal for it survived verification.
pub fn apply(
    all_ir: &mut std::collections::BTreeMap<u32, Vec<crate::ir::Statement>>,
    verified: &VerifiedArtifact,
) -> usize {
    use crate::ir::{Expression, MutVisitor};

    struct Rename<'a> {
        names: &'a std::collections::BTreeMap<u32, String>,
        applied: usize,
    }
    impl MutVisitor for Rename<'_> {
        fn visit_expression(&mut self, expr: &mut Expression) {
            if let Expression::Function { id, name, .. } = expr {
                if let Some(new) = self.names.get(&id.0) {
                    if name.as_deref() != Some(new.as_str()) {
                        *name = Some(new.clone());
                        self.applied += 1;
                    }
                }
            }
            self.walk_expression(expr);
        }
    }

    let mut rename = Rename {
        names: &verified.names,
        applied: 0,
    };
    for stmts in all_ir.values_mut() {
        rename.visit_statement_list(stmts);
    }
    rename.applied
}
