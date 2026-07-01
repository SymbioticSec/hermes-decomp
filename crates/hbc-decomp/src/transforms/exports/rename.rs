use crate::ir::{Statement, Expression, AssignTarget, Value, PropertyKey};

pub fn rename_param_registers(statements: &mut [Statement], names: &[Option<String>]) {
    // 1. Map of Reg -> Name
    let mut reg_rename_map = std::collections::BTreeMap::new();
    
    // 2. Map of VarName -> Name
    let mut var_rename_map = std::collections::HashMap::new();
    
    // Scan for LoadParam to get registers (both v2 IR and legacy formats)
    for stmt in statements.iter() {
        if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
            match value {
                // v2 IR: LoadParam produces Value::Parameter(idx)
                Expression::Value(Value::Parameter(idx)) => {
                    let idx = *idx as usize;
                    if idx < names.len() {
                        if let Some(name) = &names[idx] {
                            reg_rename_map.insert(*r, name.clone());
                        }
                    }
                }
                // Legacy v1 IR: Expression::Unknown { opcode: "LoadParam", operands }
                Expression::Unknown { opcode, operands } if opcode == "LoadParam" => {
                    if let Some(idx) = operands.first().and_then(|o| o.parse::<usize>().ok()) {
                        if idx < names.len() {
                            if let Some(name) = &names[idx] {
                                reg_rename_map.insert(*r, name.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Always include argN -> Name mappings
    for (idx, name_opt) in names.iter().enumerate() {
        if let Some(name) = name_opt {
            var_rename_map.insert(format!("arg{idx}"), name.clone());
        }
    }
    
    if reg_rename_map.is_empty() && var_rename_map.is_empty() { return; }
    
    // 3. Rename usages
    for stmt in statements.iter_mut() {
        rename_stmt(stmt, &reg_rename_map, &var_rename_map);
    }
}

fn rename_stmt(
    stmt: &mut Statement, 
    reg_map: &std::collections::BTreeMap<u32, String>,
    var_map: &std::collections::HashMap<String, String>,
) {
    match stmt {
        Statement::Let { name, value, .. } => {
            if let Some(new_name) = var_map.get(name) {
                *name = new_name.clone();
            }
            rename_expr(value, reg_map, var_map);
        }
        Statement::Assign { target, value } => {
            rename_target(target, reg_map, var_map);
            rename_expr(value, reg_map, var_map);
        }
        Statement::Expr(e) => rename_expr(e, reg_map, var_map),
        Statement::Return(Some(e)) | Statement::Throw(e) => rename_expr(e, reg_map, var_map),
        Statement::If { condition, then_body, else_body } => {
            rename_expr(condition, reg_map, var_map);
            for s in then_body { rename_stmt(s, reg_map, var_map); }
            for s in else_body { rename_stmt(s, reg_map, var_map); }
        }
        Statement::While { condition, body } => {
             rename_expr(condition, reg_map, var_map);
             for s in body { rename_stmt(s, reg_map, var_map); }
        }
        Statement::Block(body) => {
             for s in body { rename_stmt(s, reg_map, var_map); }
        }
        Statement::For { init, condition, update, body } => {
             if let Some(s) = init { rename_stmt(s, reg_map, var_map); }
             if let Some(e) = condition { rename_expr(e, reg_map, var_map); }
             if let Some(s) = update { rename_stmt(s, reg_map, var_map); }
             for s in body { rename_stmt(s, reg_map, var_map); }
        }
        Statement::Switch { discriminant, cases, default } => {
             rename_expr(discriminant, reg_map, var_map);
             for (val, body) in cases {
                 rename_expr(val, reg_map, var_map);
                 for s in body { rename_stmt(s, reg_map, var_map); }
             }
             if let Some(body) = default {
                 for s in body { rename_stmt(s, reg_map, var_map); }
             }
        }
        Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => {
             for s in try_body { rename_stmt(s, reg_map, var_map); }
             if let Some(p) = catch_param {
                  if let Some(new_p) = var_map.get(p) {
                      *catch_param = Some(new_p.clone());
                  }
             }
             for s in catch_body { rename_stmt(s, reg_map, var_map); }
             for s in finally_body { rename_stmt(s, reg_map, var_map); }
        }
        Statement::Class { name, super_class, methods, .. } => {
             if let Some(new_name) = var_map.get(name) {
                 *name = new_name.clone();
             }
             if let Some(sc) = super_class { rename_expr(sc, reg_map, var_map); }
             for m in methods {
                  if let Some(b) = &mut m.body {
                      for s in b { rename_stmt(s, reg_map, var_map); }
                  }
             }
        }
        _ => {}
    }
}

fn rename_target(
    target: &mut AssignTarget, 
    reg_map: &std::collections::BTreeMap<u32, String>,
    var_map: &std::collections::HashMap<String, String>,
) {
    match target {
        AssignTarget::Register(r) => {
            if let Some(name) = reg_map.get(r) {
                *target = AssignTarget::Variable(name.clone());
            }
        }
        AssignTarget::Variable(v) => {
            if let Some(name) = var_map.get(v) {
                *v = name.clone();
            }
        }
        AssignTarget::Member { object, .. } => rename_expr(object, reg_map, var_map),
        AssignTarget::Index { object, key } => {
            rename_expr(object, reg_map, var_map);
            rename_expr(key, reg_map, var_map);
        }
        AssignTarget::DestructuringArray(targets) => {
             for entry in targets.iter_mut().flatten() {
                 rename_target(&mut entry.0, reg_map, var_map);
                 if let Some(def) = &mut entry.1 {
                     rename_expr(def, reg_map, var_map);
                 }
             }
        }
        AssignTarget::DestructuringObject(targets) => {
             for (_, t, def) in targets {
                 rename_target(t, reg_map, var_map);
                 if let Some(d) = def {
                     rename_expr(d, reg_map, var_map);
                 }
             }
        }
        _ => {}
    }
}

fn rename_expr(
    expr: &mut Expression, 
    reg_map: &std::collections::BTreeMap<u32, String>,
    var_map: &std::collections::HashMap<String, String>,
) {
    match expr {
        Expression::Value(Value::Register(r)) => {
            if let Some(name) = reg_map.get(r) {
                *expr = Expression::Value(Value::Variable(name.clone()));
            }
        }
        Expression::Value(Value::Variable(v)) => {
            if let Some(name) = var_map.get(v) {
                *v = name.clone();
            }
        }
        Expression::Value(Value::Parameter(idx)) => {
            // This is direct Parameter access (argN)
            // We can rename it to Variable(name) if we have it
            if let Some(Some(name)) = var_map.get(&format!("arg{idx}")).map(Some) {
                 // Wait, var_map.get returns &String. 
                 // If we have a name, convert to Variable.
                 *expr = Expression::Value(Value::Variable(name.clone()));
            }
        }
        Expression::Binary { left, right, .. } => {
            rename_expr(left, reg_map, var_map);
            rename_expr(right, reg_map, var_map);
        }
        Expression::Unary { operand, .. } => rename_expr(operand, reg_map, var_map),
        Expression::Member { object, property, .. } => {
            rename_expr(object, reg_map, var_map);
            if let PropertyKey::Computed(k) = property {
                rename_expr(k, reg_map, var_map);
            }
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
             rename_expr(callee, reg_map, var_map);
             for a in arguments { rename_expr(a, reg_map, var_map); }
        }
        Expression::Object { properties } => {
             for p in properties {
                 rename_expr(&mut p.value, reg_map, var_map);
                 if let PropertyKey::Computed(k) = &mut p.key {
                     rename_expr(k, reg_map, var_map);
                 }
             }
        }
        Expression::Array { elements } => {
             for e in elements.iter_mut().flatten() {
                 rename_expr(e, reg_map, var_map);
             }
        }
        Expression::Assignment { target, value } => {
             rename_expr(target, reg_map, var_map);
             rename_expr(value, reg_map, var_map);
        }
        Expression::Spread(e) => rename_expr(e, reg_map, var_map),
        Expression::TemplateLiteral { expressions, .. } => {
             for e in expressions { rename_expr(e, reg_map, var_map); }
        }
        Expression::Yield { value, .. } | Expression::Await(value) => rename_expr(value, reg_map, var_map),
        Expression::Conditional { condition, then_expr, else_expr } => {
             rename_expr(condition, reg_map, var_map);
             rename_expr(then_expr, reg_map, var_map);
             rename_expr(else_expr, reg_map, var_map);
        }
        _ => {}
    }
}
