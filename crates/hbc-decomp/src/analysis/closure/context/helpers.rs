// Shared predicates for env-slot naming / stability.
use super::super::info::ClosureSlotValue;

pub(super) fn binding_name_is_slot_worthy(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    if name.starts_with('r') && name[1..].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if name.starts_with("arg") && name[3..].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if name.starts_with("tmp") || name.starts_with("closure_") || name.starts_with("outer") {
        return false;
    }
    // Single-letter or numeric-ish temps
    if name.len() <= 2 && name.chars().all(|c| c.is_ascii_alphanumeric()) {
        // allow common short names like `id`, `fs` — reject pure `c0`/`r1` above
        if name.chars().next().is_some_and(|c| c == 'c')
            && name[1..].chars().all(|c| c.is_ascii_digit())
        {
            return false;
        }
    }
    true
}

/// Intermediate SSA temps that must not rename a captured env slot.
pub(super) fn is_ephemeral_name(name: &str) -> bool {
    if name == "tmp"
        || name
            .strip_prefix("tmp")
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    if name
        .strip_prefix('r')
        .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    matches!(
        name,
        "sum" | "diff" | "product" | "quotient" | "text" | "result" | "value" | "ret"
            | "tmpResult" | "callResult"
    ) || name.ends_with("Result")
        || name.ends_with("Return")
}

pub(super) fn slot_value_is_stable(val: &ClosureSlotValue) -> bool {
    match val {
        ClosureSlotValue::Constant(_) | ClosureSlotValue::Function { .. } => true,
        ClosureSlotValue::Variable(v) => !is_ephemeral_name(v),
        ClosureSlotValue::RegExp | ClosureSlotValue::Unknown => false,
    }
}
