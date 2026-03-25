mod rename;

use crate::ir::{Statement, Expression, AssignTarget, Value};

pub use rename::rename_param_registers;

pub fn infer_commonjs_names(statements: &mut [Statement], num_params: u32) -> Option<Vec<Option<String>>> {
    // Metro modules typically have 4 params: (global, require, module, exports).
    // Or 3 params: (require, module, exports) ?
    // Let's assume the common signature: function(global, require, module, exports).
    // If num_params >= 3, we can try to guess.

    // Heuristic 1: If param N is used as `paramN.exports = ...`, then N is likely `module`.
    // Heuristic 2: If param M is used as `paramM.prop = ...` and M != N, then M is likely `exports`.
    // Heuristic 3: If param K is called `paramK("string")`, it might be `require`.

    // Build param_map: register → param index, from two possible IR representations.
    let mut param_map = std::collections::HashMap::new();

    for stmt in statements.iter() {
        if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
            match value {
                // v2 IR: LoadParam produces Value::Parameter(idx)
                Expression::Value(Value::Parameter(idx)) => {
                    param_map.insert(*r, *idx);
                }
                // Legacy v1 IR: Expression::Unknown { opcode: "LoadParam", operands }
                Expression::Unknown { opcode, operands } if opcode == "LoadParam" => {
                    if let Ok(idx) = operands[0].parse::<u32>() {
                        param_map.insert(*r, idx);
                    }
                }
                _ => {}
            }
        }
    }

    if param_map.is_empty() {
        return None;
    }

    // Scan usages
    let mut module_reg = None;

    // Simple scan for `r.exports = ...` pattern
    for stmt in statements.iter() {
        if let Statement::Assign { target: AssignTarget::Member { object, property }, .. } = stmt {
             if property == "exports" {
                 match object {
                     Expression::Value(Value::Register(r)) => {
                         if let Some(&p_idx) = param_map.get(r) {
                             module_reg = Some(p_idx);
                         }
                     }
                     // Also match Value::Parameter directly (when register wasn't propagated)
                     Expression::Value(Value::Parameter(idx)) => {
                         module_reg = Some(*idx);
                     }
                     // Also match Variable("argN") after param renaming
                     Expression::Value(Value::Variable(name)) => {
                         if let Some(rest) = name.strip_prefix("arg") {
                             if let Ok(idx) = rest.parse::<u32>() {
                                 module_reg = Some(idx);
                             }
                         }
                     }
                     _ => {}
                 }
             }
        }
    }

    // If we found module, we can guess exports is likely another param.
    // If param structure matches Metro (p0, p1, p2=module, p3=exports), we can assign names.

    let mut names = vec![None; num_params as usize];

    if let Some(mod_idx) = module_reg {
        if (mod_idx as usize) < names.len() {
            names[mod_idx as usize] = Some("module".to_string());
        }
    }

    // If we have 4 params and p2 is module, p3 is likely exports, p1 require, p0 global.
    if num_params == 4
         && module_reg == Some(2) {
             names[0] = Some("global".to_string());
             names[1] = Some("require".to_string());
             names[3] = Some("exports".to_string());
         }

    // If we have 3 params and p1 is module
    if num_params == 3
        && module_reg == Some(1) {
             names[0] = Some("require".to_string());
             names[2] = Some("exports".to_string());
        }

    // Verify if we found anything interesting
    if names.iter().all(|n| n.is_none()) {
        return None;
    }

    Some(names)
}
