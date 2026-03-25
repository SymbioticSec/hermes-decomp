use super::arrays::try_array_destructuring;
use super::utils::{exprs_equal, extract_property_access, get_index};
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement};

// Main in-place destructuring transform.
pub fn transform_destructuring(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i < stmts.len() {
        // Look for start of a sequence
        if let Some((obj_expr, prop, target)) = extract_property_access(&stmts[i]) {
            // Check if obj_expr is simple (register/variable) to avoid side effects
            if !obj_expr.is_simple() {
                i += 1;
                continue;
            }

            let mut properties = Vec::new();
            properties.push((prop, target.clone(), None)); // (PropertyKey, AssignTarget, Option<Expression>)

            let mut j = i + 1;
            while j < stmts.len() {
                // Skip comments
                if let Statement::Comment(_) = &stmts[j] {
                    j += 1;
                    continue;
                }

                // Check for default assignment for the LAST property
                if let Some(last_prop) = properties.last_mut() {
                    if last_prop.2.is_none() {
                        if let Some(default_val) = extract_default_assignment(&stmts[j], &last_prop.1) {
                            last_prop.2 = Some(default_val);
                            j += 1;
                            continue;
                        }
                    }
                }

                if let Some((next_obj, next_prop, next_target)) = extract_property_access(&stmts[j])
                {
                    if exprs_equal(&obj_expr, &next_obj) {
                        properties.push((next_prop, next_target, None));
                        j += 1;
                        continue;
                    }
                }
                break;
            }

            if properties.len() > 1 {
                // Detect Array vs Object based on property types
                let all_indices = properties
                    .iter()
                    .all(|(k, _, _)| matches!(k, PropertyKey::Index(_) | PropertyKey::Computed(_)));
                let all_members = properties
                    .iter()
                    .all(|(k, _, _)| matches!(k, PropertyKey::String(_) | PropertyKey::Ident(_)));

                if all_indices && try_array_destructuring(&properties) {
                    // Array Destructuring
                    let mut indexed_props: Vec<(i64, AssignTarget, Option<Expression>)> = properties
                        .iter()
                        .filter_map(|(k, t, def)| get_index(k).map(|idx| (idx, t.clone(), def.clone())))
                        .collect();

                    if indexed_props.is_empty() {
                        i += 1;
                        continue;
                    }

                    indexed_props.sort_by_key(|(idx, _, _)| *idx);
                    let max_idx = indexed_props.last().map(|(idx, _, _)| *idx).unwrap_or(0);

                    // Only create array destructuring if indices are consecutive from 0
                    let expected_count = (max_idx + 1) as usize;
                    if indexed_props.len() == expected_count && indexed_props[0].0 == 0 {
                        let mut targets: Vec<Option<(AssignTarget, Option<Expression>)>> = vec![None; expected_count];
                        for (idx, t, def) in indexed_props {
                            if idx >= 0 && (idx as usize) < targets.len() {
                                targets[idx as usize] = Some((t, def));
                            }
                        }

                        stmts[i] = Statement::Assign {
                            target: AssignTarget::DestructuringArray(targets),
                            value: obj_expr,
                        };

                        // Remove merged statements, keeping comments
                        let mut to_remove = Vec::new();
                        for (offset, stmt) in stmts[(i + 1)..j].iter().enumerate() {
                            if !matches!(stmt, Statement::Comment(_)) {
                                to_remove.push(i + 1 + offset);
                            }
                        }
                        for idx in to_remove.into_iter().rev() {
                            stmts.remove(idx);
                        }
                        i += 1;
                        continue;
                    }
                } else if all_members {
                    // Object Destructuring
                    let props: Vec<(String, AssignTarget, Option<Expression>)> = properties
                        .into_iter()
                        .map(|(k, t, def)| {
                            let key = match k {
                                PropertyKey::String(s) => s,
                                PropertyKey::Ident(s) => s,
                                _ => String::new(),
                            };
                            (key, t, def)
                        })
                        .filter(|(k, _, _)| !k.is_empty())
                        .collect();

                    if props.len() > 1 {
                        stmts[i] = Statement::Assign {
                            target: AssignTarget::DestructuringObject(props),
                            value: obj_expr,
                        };

                        // Remove merged statements, keeping comments
                        let mut to_remove = Vec::new();
                        for (offset, stmt) in stmts[(i + 1)..j].iter().enumerate() {
                            if !matches!(stmt, Statement::Comment(_)) {
                                to_remove.push(i + 1 + offset);
                            }
                        }
                        for idx in to_remove.into_iter().rev() {
                            stmts.remove(idx);
                        }
                        i += 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
}

// Check if a statement is a default assignment for a given target.
// E.g. `if (target === undefined) target = default_val;`
fn extract_default_assignment(stmt: &Statement, target: &AssignTarget) -> Option<Expression> {
    if let Statement::If { condition, then_body, else_body } = stmt {
        if else_body.is_empty() && then_body.len() == 1 {
            if let Expression::Binary { op, left, right } = condition {
                use crate::ir::{BinaryOp, Value, Constant};
                // Check if op is strict or loose equality
                if *op == BinaryOp::StrictEq || *op == BinaryOp::Eq {
                    // Check if one side is the target and the other is `undefined`
                    let (t_side, val_side) = (left.as_ref(), right.as_ref());
                    
                    let mut is_undefined_check = false;
                    let is_target = |expr: &Expression| -> bool {
                        match (expr, target) {
                            (Expression::Value(Value::Variable(v1)), AssignTarget::Variable(v2)) => v1 == v2,
                            (Expression::Value(Value::Register(r1)), AssignTarget::Register(r2)) => r1 == r2,
                            _ => false,
                        }
                    };
                    let is_undefined = |expr: &Expression| -> bool {
                        matches!(expr, Expression::Value(Value::Constant(Constant::Undefined)))
                    };

                    if (is_target(t_side) && is_undefined(val_side))
                        || (is_target(val_side) && is_undefined(t_side))
                    {
                        is_undefined_check = true;
                    }

                    if is_undefined_check {
                        if let Statement::Assign { target: then_target, value } = &then_body[0] {
                            // Check if the assignment target matches the checked target
                            let does_target_match = match (then_target, target) {
                                (AssignTarget::Variable(v1), AssignTarget::Variable(v2)) => v1 == v2,
                                (AssignTarget::Register(r1), AssignTarget::Register(r2)) => r1 == r2,
                                _ => false,
                            };
                            if does_target_match {
                                return Some(value.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}
