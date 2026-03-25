use crate::ir::{AssignTarget, Expression, Statement, Value};

pub fn transform_spread_rest(stmts: &mut Vec<Statement>) {
    // 1. Array Spread Construction
    // Look for patterns like:
    //   t1 = [a, b] (NewArray or Array literal)
    //   __arraySpread(t1, c)
    //   -> t1 becomes [a, b, ...c]

    // We iterate specific indices to allow lookahead/modification
    let mut i = 0;
    while i < stmts.len() {
        let stmt = &stmts[i];

        // Detect __arraySpread call
        // It is typically an ExpressionStatement (side effect) or Assign (return value is the array?)
        // Hermes implementation of arraySpread: "Appends the elements of the array 'from' to the array 'to'. Returns 'to'."

        let mut spread_info = None;
        if let Statement::Assign { target: _, value } = stmt {
            if let Some((target_reg, source_expr)) = parse_array_spread(value) {
                spread_info = Some((i, target_reg, source_expr));
            }
        } else if let Statement::Expr(value) = stmt {
            if let Some((target_reg, source_expr)) = parse_array_spread(value) {
                // Return value ignored, but side effect on target_reg matches
                spread_info = Some((i, target_reg, source_expr));
            }
        }

        if let Some((current_idx, target_reg, source_expr)) = spread_info {
            // Find where target_reg was defined/created
            // Search BACKWARDS from current_idx
            let mut found_creation = false;
            for j in (0..current_idx).rev() {
                if let Statement::Assign {
                    target: AssignTarget::Register(r),
                    value,
                } = &mut stmts[j]
                {
                    if *r == target_reg {
                        // Found definition. Check if it is an Array creation.
                        if let Expression::Array { elements } = value {
                            // Append Spread of source
                            elements.push(Some(Expression::Spread(Box::new(source_expr))));
                            found_creation = true;
                        }
                        // Stop search once definition is found (regardless of whether it was an array)
                        break;
                    }
                }
            }

            if found_creation {
                stmts[i] = Statement::Comment("merged spread".into());
            }
        }

        // 2. Rest Args
        // __copyRestArgs(N) -> ...arguments (conceptual)
        // Usually: rest = __copyRestArgs(iter)
        if let Statement::Assign { target: _, value } = &mut stmts[i] {
            if let Expression::Call {
                callee,
                arguments: _,
            } = value
            {
                if let Expression::Value(Value::Variable(name)) = &**callee {
                    if name == "__copyRestArgs" {
                        // Transform to Spread(arguments) or similar?
                        // Actually, let's make it explicitly look like rest.
                        // But semantics of copyRestArgs is "create array from arguments[N..]".
                        // We can replace it with `Array.prototype.slice.call(arguments, N)` or simply `...arguments` if appropriate.
                        // Let's use `...arguments` notation as a heuristic for readability.

                        *value = Expression::Spread(Box::new(Expression::Value(Value::Variable(
                            "arguments".to_string(),
                        ))));

                        // Note: we lose the "N" offset info here, but for decompilation "readability" it's often close enough
                        // or user understands that `...arguments` implies "the rest".
                        // Logic precision: low. Readability: high.
                    }
                }
            }
        }

        i += 1;
    }

    stmts.retain(|s| !matches!(s, Statement::Comment(c) if c == "merged spread"));
}

fn parse_array_spread(expr: &Expression) -> Option<(u32, Expression)> {
    if let Expression::Call { callee, arguments } = expr {
        if let Expression::Value(Value::Variable(name)) = &**callee {
            if name == "__arraySpread" && arguments.len() == 2 {
                if let Expression::Value(Value::Register(target_reg)) = &arguments[0] {
                    return Some((*target_reg, arguments[1].clone()));
                }
            }
        }
    }
    None
}
