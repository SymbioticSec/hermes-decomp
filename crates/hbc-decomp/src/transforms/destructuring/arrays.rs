use super::utils::{exprs_equal, get_index};
use crate::ir::{AssignTarget, Constant, Expression, PropertyKey, Statement, Value};

// Check if all properties can form a valid array destructuring.
pub fn try_array_destructuring(properties: &[(PropertyKey, AssignTarget, Option<Expression>)]) -> bool {
    let indices: Vec<i64> = properties
        .iter()
        .filter_map(|(k, _, _)| get_index(k))
        .collect();
    if indices.len() != properties.len() {
        return false;
    }
    // Check for consecutive indices starting from 0
    let mut sorted = indices.clone();
    sorted.sort();
    sorted.iter().enumerate().all(|(i, &idx)| idx == i as i64)
}

// Transform array destructuring + slice into rest destructuring.
// Detects patterns like:
//   [a, b] = arr;
//   rest = arr.slice(2);
// And transforms to:
//   [a, b, ...rest] = arr;
pub fn transform_rest_destructuring(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i < stmts.len() {
        // Look for array destructuring followed by a slice call on the same source
        if let Statement::Assign {
            target: AssignTarget::DestructuringArray(elements),
            value: source,
        } = &stmts[i]
        {
            let num_elements = elements.len();
            let source_clone = source.clone();

            // Look for slice call in the next few statements
            let mut slice_idx = None;
            let mut rest_target = None;

            let search_end = std::cmp::min(i + 5, stmts.len());
            for (offset, stmt) in stmts[(i + 1)..search_end].iter().enumerate() {
                if let Some((target, slice_source, start_idx)) = extract_slice_call(stmt) {
                    // Check if it's a slice on the same source with matching start index
                    if exprs_equal(&source_clone, &slice_source) && start_idx == num_elements as i64
                    {
                        slice_idx = Some(i + 1 + offset);
                        rest_target = Some(target);
                        break;
                    }
                }
            }

            if let (Some(j), Some(rest)) = (slice_idx, rest_target) {
                // Convert to DestructuringArrayRest
                let new_target = AssignTarget::DestructuringArrayRest {
                    elements: elements.clone(),
                    rest: Box::new(rest),
                };

                stmts[i] = Statement::Assign {
                    target: new_target,
                    value: source_clone,
                };

                // Remove the slice statement
                stmts.remove(j);
                continue; // Don't increment i, re-check this position
            }
        }
        i += 1;
    }
}

// Extract slice call pattern: target = source.slice(N)
fn extract_slice_call(stmt: &Statement) -> Option<(AssignTarget, Expression, i64)> {
    let (target, value) = match stmt {
        Statement::Assign { target, value } => (target.clone(), value),
        Statement::Let { name, value, .. } => (AssignTarget::Variable(name.clone()), value),
        _ => return None,
    };

    // Pattern: source.slice(N)
    if let Expression::Call { callee, arguments } = value {
        if let Expression::Member {
            object,
            property: PropertyKey::Ident(method),
            optional: false,
        } = callee.as_ref()
        {
            if method == "slice" && arguments.len() == 1 {
                if let Expression::Value(Value::Constant(Constant::Integer(n))) = &arguments[0] {
                    return Some((target, *object.clone(), *n as i64));
                }
            }
        }
    }

    None
}
