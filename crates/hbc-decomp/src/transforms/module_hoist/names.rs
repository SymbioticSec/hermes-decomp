// Binding-name allocation and occupancy for hoisted module locals.

use std::collections::{HashMap, HashSet};

use crate::ir::{AssignTarget, Expression, Statement};

use super::detect::loader_call;
use super::kinds::{HoistKey, LoaderKind, RESERVED_BINDINGS};

pub(super) fn is_reserved_binding(name: &str) -> bool {
    RESERVED_BINDINGS.contains(&name) || crate::constants::is_reserved_word(name)
}

pub(super) fn allocate_binding_name(
    preferred: Option<String>,
    id: u32,
    kind: LoaderKind,
    used: &mut HashSet<String>,
) -> String {
    if let Some(base) = preferred {
        if !used.contains(&base) && !is_reserved_binding(&base) {
            used.insert(base.clone());
            return base;
        }
        let mut i = 2u32;
        loop {
            let candidate = format!("{base}{i}");
            if !used.contains(&candidate) && !is_reserved_binding(&candidate) {
                used.insert(candidate.clone());
                return candidate;
            }
            i += 1;
            if i > 10_000 {
                break;
            }
        }
    }
    let prefix = match kind {
        LoaderKind::Require => "_mod",
        LoaderKind::ImportDefault => "_modDef",
        LoaderKind::ImportAll => "_modAll",
    };
    let mut n = format!("{prefix}{id}");
    while used.contains(&n) {
        n.push('_');
    }
    used.insert(n.clone());
    n
}

pub(super) fn binding_name_value(stmt: &Statement) -> Option<(&str, &Expression)> {
    match stmt {
        Statement::Let { name, value, .. } => Some((name.as_str(), value)),
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value,
        } => Some((name.as_str(), value)),
        _ => None,
    }
}

// True when `stmt` is the reused module binding whose name we adopted.
pub(super) fn is_reused_binding(
    stmt: &Statement,
    existing: &HashMap<HoistKey, String>,
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
) -> bool {
    if let Some((name, value)) = binding_name_value(stmt) {
        if let Some((id, kind, _)) = loader_call(value, aliases, deps) {
            return existing.get(&(id, kind)).map(String::as_str) == Some(name);
        }
    }
    false
}

// Collect names bound by let/const/assign in a body (including nested blocks).
pub(super) fn collect_existing_binding_names(stmts: &[Statement], out: &mut HashSet<String>) {
    for s in stmts {
        match s {
            Statement::Let { name, .. } => {
                out.insert(name.clone());
            }
            Statement::Assign {
                target: AssignTarget::Variable(name),
                ..
            } => {
                out.insert(name.clone());
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_existing_binding_names(then_body, out);
                collect_existing_binding_names(else_body, out);
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::Block(body) => {
                collect_existing_binding_names(body, out);
            }
            Statement::For {
                init,
                body,
                update,
                ..
            } => {
                if let Some(i) = init {
                    collect_existing_binding_names(std::slice::from_ref(i.as_ref()), out);
                }
                if let Some(u) = update {
                    collect_existing_binding_names(std::slice::from_ref(u.as_ref()), out);
                }
                collect_existing_binding_names(body, out);
            }
            Statement::ForIn { variable, body, .. } | Statement::ForOf { variable, body, .. } => {
                out.insert(variable.clone());
                collect_existing_binding_names(body, out);
            }
            Statement::TryCatch {
                try_body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                collect_existing_binding_names(try_body, out);
                if let Some(p) = catch_param {
                    out.insert(p.clone());
                }
                collect_existing_binding_names(catch_body, out);
                collect_existing_binding_names(finally_body, out);
            }
            Statement::Switch {
                cases, default, ..
            } => {
                for (_, body) in cases {
                    collect_existing_binding_names(body, out);
                }
                if let Some(d) = default {
                    collect_existing_binding_names(d, out);
                }
            }
            Statement::Class {
                name,
                constructor,
                methods,
                ..
            } => {
                out.insert(name.clone());
                if let Some(c) = constructor {
                    collect_existing_binding_names(std::slice::from_ref(c.as_ref()), out);
                }
                for m in methods {
                    out.extend(m.params.iter().cloned());
                    if let Some(body) = &m.body {
                        collect_existing_binding_names(body, out);
                    }
                }
            }
            _ => {}
        }
    }
}
