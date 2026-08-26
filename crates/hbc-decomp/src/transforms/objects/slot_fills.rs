use crate::ir::{
    AssignTarget, Expression, ObjectProperty, Statement, Value, stmt_has_side_effects,
};
use super::is_reg_used;

// Fold `obj = { k0:v0, k1:<placeholder>, ... }; obj[N] = val` (a slot-index
// fill from PutOwnBySlotIdx) into the literal's Nth property. Only replaces a
// placeholder value (null/undefined/empty), which is what the shape-table form
// leaves for non-serializable property values, so a genuine numeric-key write
// is never absorbed. Matches both still-register objects and named
// Variable/Let objects (after register naming).
//
// Does NOT recurse into if/switch/try/loop: nested folding rewrote the v98
// generator result object (`obj = {value, done}; obj[0] = V; return obj`)
// into a shape the reconstructor used to miss, and left raw state machines
// in the dump. Nested `obj[N]` fills stay as-is.
pub fn fold_slot_index_fills(statements: &mut Vec<Statement>) {
    fold_slot_index_fills_here(statements);
}

pub(super) fn fold_slot_index_fills_here(statements: &mut Vec<Statement>) {
    // Mark consumed fills and drop them all in one pass. Removing each fill inside
    // the loop shifts the tail on every fill, which is O(n^2) on a large function.
    let mut consumed = vec![false; statements.len()];
    let mut i = 0;
    while i < statements.len() {
        let Some((obj, prop_count)) = object_literal_def(&statements[i]) else {
            i += 1;
            continue;
        };

        // Registers/variables defined AFTER the object literal while scanning
        // forward. A fill value that references one of these was produced later, so
        // hoisting it into the literal (which evaluates at the object's construction
        // point) would read it before it exists and changes behaviour. Such a
        // forward-referencing fill is left in place instead of folded.
        let mut defined_after_regs: Vec<u32> = Vec::new();
        let mut defined_after_vars: Vec<String> = Vec::new();

        let mut j = i + 1;
        while j < statements.len() {
            if let Some((slot, val)) = slot_index_fill(&statements[j], &obj, prop_count) {
                if val_refs_forward(&val, &defined_after_regs, &defined_after_vars) {
                    // Forward reference: leave this one fill in place (it assigns the
                    // slot after its value exists) but keep folding the object's other
                    // slots, whose clean literal values can still be hoisted safely.
                    j += 1;
                    continue;
                }
                if let Some(properties) = object_properties_mut(&mut statements[i]) {
                    if is_placeholder(&properties[slot].value) {
                        properties[slot].value = val;
                        consumed[j] = true;
                        record_defs(&statements[j], &mut defined_after_regs, &mut defined_after_vars);
                        j += 1;
                        continue;
                    }
                }
                break;
            } else if obj_reassigned(&statements[j], &obj)
                || obj_used(&statements[j], &obj)
                || stmt_has_side_effects(&statements[j])
            {
                break;
            }
            record_defs(&statements[j], &mut defined_after_regs, &mut defined_after_vars);
            j += 1;
        }
        i += 1;
    }
    if consumed.iter().any(|&c| c) {
        let mut idx = 0;
        statements.retain(|_| {
            let keep = !consumed[idx];
            idx += 1;
            keep
        });
    }
    fold_named_placeholder_fills_here(statements);
    // Fills we could not hoist (forward-ref values, or an `if` between slots)
    // still use a slot index. Rewrite `obj[N] = val` to `obj.key = val` from
    // the literal's Nth property name so the output matches the shape instead
    // of a numeric index. Evaluation order is unchanged.
    rewrite_slot_index_names(statements);
}

// `obj = { url: null, body: null }; obj.body = val` after slot-index fills have
// already become named members. Fold the placeholder property into the literal.
fn fold_named_placeholder_fills_here(statements: &mut Vec<Statement>) {
    let mut consumed = vec![false; statements.len()];
    let mut i = 0;
    while i < statements.len() {
        let Some((obj, _)) = object_literal_def(&statements[i]) else {
            i += 1;
            continue;
        };
        let mut defined_after_vars: Vec<String> = Vec::new();
        let mut defined_after_regs: Vec<u32> = Vec::new();
        let mut j = i + 1;
        while j < statements.len() {
            if let Some((prop, val)) = named_member_fill(&statements[j], &obj) {
                if val_refs_forward(&val, &defined_after_regs, &defined_after_vars) {
                    j += 1;
                    continue;
                }
                if let Some(properties) = object_properties_mut(&mut statements[i]) {
                    if let Some(slot) = properties.iter().position(|p| prop_key_is(&p.key, &prop)) {
                        if is_placeholder(&properties[slot].value) {
                            properties[slot].value = val;
                            consumed[j] = true;
                            record_defs(&statements[j], &mut defined_after_regs, &mut defined_after_vars);
                            j += 1;
                            continue;
                        }
                    }
                }
                break;
            } else if obj_reassigned(&statements[j], &obj)
                || obj_used(&statements[j], &obj)
                || stmt_has_side_effects(&statements[j])
            {
                break;
            }
            record_defs(&statements[j], &mut defined_after_regs, &mut defined_after_vars);
            j += 1;
        }
        i += 1;
    }
    if consumed.iter().any(|&c| c) {
        let mut idx = 0;
        statements.retain(|_| {
            let keep = !consumed[idx];
            idx += 1;
            keep
        });
    }
}

fn named_member_fill(stmt: &Statement, obj: &ObjRef) -> Option<(String, Expression)> {
    let Statement::Assign {
        target: AssignTarget::Member { object, property },
        value,
    } = stmt
    else {
        return None;
    };
    let obj_now = match object {
        Expression::Value(Value::Register(r)) => ObjRef::Register(*r),
        Expression::Value(Value::Variable(n)) => ObjRef::Name(n.clone()),
        _ => return None,
    };
    if !obj_ref_eq(&obj_now, obj) {
        return None;
    }
    Some((property.clone(), value.clone()))
}

fn prop_key_is(key: &crate::ir::PropertyKey, name: &str) -> bool {
    matches!(key, crate::ir::PropertyKey::Ident(s) | crate::ir::PropertyKey::String(s) if s == name)
}

fn rewrite_slot_index_names(statements: &mut [Statement]) {
    let mut shapes: Vec<(ObjRef, Vec<String>)> = Vec::new();
    for stmt in statements.iter_mut() {
        if let Some((obj, keys)) = object_ident_keys(stmt) {
            shapes.retain(|(o, _)| !obj_ref_eq(o, &obj));
            shapes.push((obj, keys));
            continue;
        }
        let rewritten = (|| {
            let Statement::Assign {
                target: AssignTarget::Index { object, key },
                ..
            } = stmt
            else {
                return None;
            };
            let n = match key {
                Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) if *n >= 0 => {
                    *n as usize
                }
                _ => return None,
            };
            let obj_now = match object {
                Expression::Value(Value::Register(r)) => ObjRef::Register(*r),
                Expression::Value(Value::Variable(name)) => ObjRef::Name(name.clone()),
                _ => return None,
            };
            shapes.iter().find(|(o, _)| obj_ref_eq(o, &obj_now)).and_then(|(_, keys)| {
                keys.get(n).map(|name| AssignTarget::Member {
                    object: object.clone(),
                    property: name.clone(),
                })
            })
        })();
        if let Some(new_target) = rewritten {
            if let Statement::Assign { target, .. } = stmt {
                *target = new_target;
            }
            continue;
        }
        if let Some((obj, _)) = shapes.iter().find(|(o, _)| obj_reassigned(stmt, o)) {
            let obj = match obj {
                ObjRef::Register(r) => ObjRef::Register(*r),
                ObjRef::Name(n) => ObjRef::Name(n.clone()),
            };
            shapes.retain(|(o, _)| !obj_ref_eq(o, &obj));
        }
    }
}

fn object_ident_keys(stmt: &Statement) -> Option<(ObjRef, Vec<String>)> {
    let (obj, props) = match stmt {
        Statement::Assign {
            target: AssignTarget::Register(r),
            value: Expression::Object { properties },
        } if !properties.is_empty() => (ObjRef::Register(*r), properties),
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value: Expression::Object { properties },
        } if !properties.is_empty() => (ObjRef::Name(name.clone()), properties),
        Statement::Let {
            name,
            value: Expression::Object { properties },
            ..
        } if !properties.is_empty() => (ObjRef::Name(name.clone()), properties),
        _ => return None,
    };
    let mut keys = Vec::with_capacity(props.len());
    for p in props {
        match &p.key {
            crate::ir::PropertyKey::Ident(s) => keys.push(s.clone()),
            _ => return None,
        }
    }
    Some((obj, keys))
}

fn obj_ref_eq(a: &ObjRef, b: &ObjRef) -> bool {
    match (a, b) {
        (ObjRef::Register(x), ObjRef::Register(y)) => x == y,
        (ObjRef::Name(x), ObjRef::Name(y)) => x == y,
        _ => false,
    }
}

enum ObjRef {
    Register(u32),
    Name(String),
}

fn object_literal_def(stmt: &Statement) -> Option<(ObjRef, usize)> {
    match stmt {
        Statement::Assign {
            target: AssignTarget::Register(r),
            value: Expression::Object { properties },
        } if !properties.is_empty() => Some((ObjRef::Register(*r), properties.len())),
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value: Expression::Object { properties },
        } if !properties.is_empty() => Some((ObjRef::Name(name.clone()), properties.len())),
        Statement::Let {
            name,
            value: Expression::Object { properties },
            ..
        } if !properties.is_empty() => Some((ObjRef::Name(name.clone()), properties.len())),
        _ => None,
    }
}

fn object_properties_mut(stmt: &mut Statement) -> Option<&mut Vec<ObjectProperty>> {
    match stmt {
        Statement::Assign {
            value: Expression::Object { properties },
            ..
        }
        | Statement::Let {
            value: Expression::Object { properties },
            ..
        } => Some(properties),
        _ => None,
    }
}

// `obj[N] = val` with a constant N < prop_count → (N, val).
fn slot_index_fill(stmt: &Statement, obj: &ObjRef, prop_count: usize) -> Option<(usize, Expression)> {
    let Statement::Assign {
        target: AssignTarget::Index { object, key },
        value,
    } = stmt
    else {
        return None;
    };
    let matches_obj = match (obj, object) {
        (ObjRef::Register(r), Expression::Value(Value::Register(r2))) => r == r2,
        (ObjRef::Name(n), Expression::Value(Value::Variable(n2))) => n == n2,
        _ => false,
    };
    if !matches_obj {
        return None;
    }
    let n = match key {
        Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) if *n >= 0 => {
            *n as usize
        }
        _ => return None,
    };
    if n < prop_count {
        Some((n, value.clone()))
    } else {
        None
    }
}

// Record the register/variable a statement binds, so a later fill value can be
// tested for referencing it. Only simple register/variable/let targets create a
// name a fill value could read; member/index writes mutate an existing binding.
fn record_defs(stmt: &Statement, regs: &mut Vec<u32>, vars: &mut Vec<String>) {
    match stmt {
        Statement::Assign { target: AssignTarget::Register(r), .. } => regs.push(*r),
        Statement::Assign { target: AssignTarget::Variable(n), .. } => vars.push(n.clone()),
        Statement::Let { name, .. } => vars.push(name.clone()),
        _ => {}
    }
}

// Whether a fill value reads any register/variable defined after the object
// literal. Such a value cannot be hoisted into the literal without reading it
// before it is assigned.
fn val_refs_forward(val: &Expression, regs: &[u32], vars: &[String]) -> bool {
    use crate::ir::Visitor;
    struct C<'a> { regs: &'a [u32], vars: &'a [String], found: bool }
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            match e {
                Expression::Value(Value::Register(r)) if self.regs.contains(r) => self.found = true,
                Expression::Value(Value::Variable(n)) if self.vars.iter().any(|v| v == n) => {
                    self.found = true
                }
                _ => {}
            }
            if !self.found {
                self.walk_expression(e);
            }
        }
    }
    let mut c = C { regs, vars, found: false };
    c.visit_expression(val);
    c.found
}

fn obj_reassigned(stmt: &Statement, obj: &ObjRef) -> bool {
    match (obj, stmt) {
        (
            ObjRef::Register(r),
            Statement::Assign {
                target: AssignTarget::Register(r2),
                ..
            },
        ) => r == r2,
        (
            ObjRef::Name(n),
            Statement::Assign {
                target: AssignTarget::Variable(n2),
                ..
            },
        ) => n == n2,
        (ObjRef::Name(n), Statement::Let { name, .. }) => name == n,
        _ => false,
    }
}

fn obj_used(stmt: &Statement, obj: &ObjRef) -> bool {
    match obj {
        ObjRef::Register(r) => is_reg_used(stmt, *r),
        ObjRef::Name(n) => stmt_uses_var(stmt, n),
    }
}

fn stmt_uses_var(stmt: &Statement, name: &str) -> bool {
    match stmt {
        Statement::Assign { target, value } => {
            target_uses_var(target, name) || expr_uses_var(value, name)
        }
        Statement::Let { value, .. } => expr_uses_var(value, name),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            expr_uses_var(e, name)
        }
        Statement::If { condition, .. }
        | Statement::While { condition, .. }
        | Statement::DoWhile { condition, .. } => expr_uses_var(condition, name),
        _ => false,
    }
}

fn target_uses_var(target: &AssignTarget, name: &str) -> bool {
    match target {
        AssignTarget::Member { object, .. } => expr_uses_var(object, name),
        AssignTarget::Index { object, key } => {
            expr_uses_var(object, name) || expr_uses_var(key, name)
        }
        _ => false,
    }
}

fn expr_uses_var(expr: &Expression, name: &str) -> bool {
    use crate::ir::Visitor;
    struct C<'a>(&'a str, bool);
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                if n == self.0 {
                    self.1 = true;
                    return;
                }
            }
            if !self.1 {
                self.walk_expression(e);
            }
        }
    }
    let mut c = C(name, false);
    c.visit_expression(expr);
    c.1
}

fn is_placeholder(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(
            crate::ir::Constant::Null | crate::ir::Constant::Undefined
        ))
    )
}
