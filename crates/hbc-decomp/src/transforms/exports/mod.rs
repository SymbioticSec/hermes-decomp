mod rename;

use crate::analysis::metro::registry::FactoryRoles;
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};
use std::collections::HashMap;

pub use rename::rename_param_registers;

// Resolve an expression to the factory parameter index it refers to, across the
// several forms a parameter can take in the IR: a direct `Value::Parameter`, a
// register that was loaded from a parameter, or an already-named `argN` / `pN`
// variable.
fn resolve_param_idx(expr: &Expression, param_map: &HashMap<u32, u32>) -> Option<u32> {
    match expr {
        Expression::Value(Value::Parameter(idx)) => Some(*idx),
        Expression::Value(Value::Register(r)) => param_map.get(r).copied(),
        Expression::Value(Value::Variable(n)) => FactoryRoles::extract_param_index(n),
        _ => None,
    }
}

// Signals collected from the factory body that anchor the role assignment to
// actual usage rather than relying purely on parameter position.
#[derive(Default)]
struct FactorySignals {
    // Parameter used as `param.exports = ...` → this is `module`.
    module: Option<u32>,
    // Parameter called with another parameter indexed as an argument
    // (`require(undefined, dependencyMap[k])`) → callee is `require`, indexed is `dependencyMap`.
    require: Option<u32>,
    deps: Option<u32>,
    // Any parameter indexed with a constant (`param[k]`) → candidate dependency map.
    indexed: Option<u32>,
}

fn is_const_index(key: &PropertyKey) -> bool {
    match key {
        PropertyKey::Index(_) => true,
        PropertyKey::Computed(e) => matches!(
            e.as_ref(),
            Expression::Value(Value::Constant(crate::ir::Constant::Integer(_)))
                | Expression::Value(Value::Constant(crate::ir::Constant::Number(_)))
        ),
        _ => false,
    }
}

fn collect_expr_signals(
    expr: &Expression,
    param_map: &HashMap<u32, u32>,
    sig: &mut FactorySignals,
) {
    match expr {
        // `param[k]` → candidate dependency map.
        Expression::Member { object, property, .. } => {
            if is_const_index(property) {
                if let Some(idx) = resolve_param_idx(object, param_map) {
                    sig.indexed.get_or_insert(idx);
                }
            }
            collect_expr_signals(object, param_map, sig);
        }
        // `require(..., dependencyMap[k], ...)` — callee param + an arg that
        // indexes another param. This is Metro's canonical require-of-dep shape.
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            if let Some(callee_idx) = resolve_param_idx(callee, param_map) {
                for arg in arguments {
                    if let Expression::Member { object, property, .. } = arg {
                        if is_const_index(property) {
                            if let Some(dep_idx) = resolve_param_idx(object, param_map) {
                                sig.require.get_or_insert(callee_idx);
                                sig.deps.get_or_insert(dep_idx);
                            }
                        }
                    }
                }
            }
            collect_expr_signals(callee, param_map, sig);
            for arg in arguments {
                collect_expr_signals(arg, param_map, sig);
            }
        }
        Expression::Binary { left, right, .. } => {
            collect_expr_signals(left, param_map, sig);
            collect_expr_signals(right, param_map, sig);
        }
        Expression::Unary { operand, .. } => collect_expr_signals(operand, param_map, sig),
        Expression::Assignment { target, value } => {
            collect_expr_signals(target, param_map, sig);
            collect_expr_signals(value, param_map, sig);
        }
        Expression::Conditional { condition, then_expr, else_expr } => {
            collect_expr_signals(condition, param_map, sig);
            collect_expr_signals(then_expr, param_map, sig);
            collect_expr_signals(else_expr, param_map, sig);
        }
        _ => {}
    }
}

fn collect_stmt_signals(stmt: &Statement, param_map: &HashMap<u32, u32>, sig: &mut FactorySignals) {
    // `param.exports = ...` → `module`.
    if let Statement::Assign { target: AssignTarget::Member { object, property }, value } = stmt {
        if property == "exports" {
            if let Some(idx) = resolve_param_idx(object, param_map) {
                sig.module.get_or_insert(idx);
            }
        }
        collect_expr_signals(object, param_map, sig);
        collect_expr_signals(value, param_map, sig);
        return;
    }
    match stmt {
        Statement::Assign { value, .. } => collect_expr_signals(value, param_map, sig),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Let { value: e, .. } => {
            collect_expr_signals(e, param_map, sig)
        }
        Statement::If { condition, then_body, else_body } => {
            collect_expr_signals(condition, param_map, sig);
            for s in then_body.iter().chain(else_body) {
                collect_stmt_signals(s, param_map, sig);
            }
        }
        Statement::While { condition, body } => {
            collect_expr_signals(condition, param_map, sig);
            for s in body {
                collect_stmt_signals(s, param_map, sig);
            }
        }
        Statement::For { body, .. } => {
            for s in body {
                collect_stmt_signals(s, param_map, sig);
            }
        }
        Statement::DoWhile { body, condition } => {
            collect_expr_signals(condition, param_map, sig);
            for s in body {
                collect_stmt_signals(s, param_map, sig);
            }
        }
        Statement::Switch { discriminant, cases, default } => {
            collect_expr_signals(discriminant, param_map, sig);
            for (_, body) in cases {
                for s in body {
                    collect_stmt_signals(s, param_map, sig);
                }
            }
            if let Some(body) = default {
                for s in body {
                    collect_stmt_signals(s, param_map, sig);
                }
            }
        }
        Statement::ForOf { iterable: e, body, .. } | Statement::ForIn { object: e, body, .. } => {
            collect_expr_signals(e, param_map, sig);
            for s in body {
                collect_stmt_signals(s, param_map, sig);
            }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body.iter().chain(catch_body).chain(finally_body) {
                collect_stmt_signals(s, param_map, sig);
            }
        }
        _ => {}
    }
}

// Infer canonical names for the parameters of a Metro factory function.
//
// `param_count` is the raw Hermes parameter count (which includes the implicit
// `this`); the declared-parameter count is therefore `param_count - 1` and maps
// directly to the `Value::Parameter` / `argN` index space.
//
// The arity of a Metro factory determines its convention (classic
// `(global, require, module, exports)` vs modern
// `(global, require, importDefault, importAll, module, exports, dependencyMap)`),
// but we additionally anchor the assignment to observed usage (the `.exports`
// assignment that identifies `module`, and the `require(dependencyMap[k])` call
// shape that identifies `require`/`dependencyMap`). When the usage disagrees
// with the positional convention, usage wins.
//
// Naming is gated on at least one strong Metro-specific signal so that ordinary
// functions with several parameters are never mistaken for factories.
pub fn infer_commonjs_names(statements: &mut [Statement], param_count: u32) -> Option<Vec<Option<String>>> {
    let declared = param_count.saturating_sub(1);
    if declared < 3 {
        return None;
    }

    // Map registers loaded from a parameter back to the parameter index.
    let mut param_map: HashMap<u32, u32> = HashMap::new();
    for stmt in statements.iter() {
        if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
            match value {
                Expression::Value(Value::Parameter(idx)) => {
                    param_map.insert(*r, *idx);
                }
                Expression::Unknown { opcode, operands } if opcode == "LoadParam" => {
                    if let Some(idx) = operands.first().and_then(|o| o.parse::<u32>().ok()) {
                        param_map.insert(*r, idx);
                    }
                }
                _ => {}
            }
        }
    }

    let mut sig = FactorySignals::default();
    for stmt in statements.iter() {
        collect_stmt_signals(stmt, &param_map, &mut sig);
    }

    // Factory gate: require a strong, Metro-specific signal.
    let looks_like_factory =
        sig.module.is_some() || (sig.require.is_some() && sig.deps.is_some());
    if !looks_like_factory {
        return None;
    }

    // Positional convention from the arity.
    let mut roles = FactoryRoles::from_param_count(declared);

    // Usage overrides: `module` (and the adjacent `exports`) anchored on the
    // observed `.exports` assignment.
    if let Some(m) = sig.module {
        if m != roles.module_idx && m + 1 < declared {
            roles.module_idx = m;
            roles.exports_idx = m + 1;
        }
    }
    if let Some(r) = sig.require {
        roles.require_idx = r;
    }
    if let Some(d) = sig.deps.or(sig.indexed) {
        // Only trust an indexed parameter as the dependency map if it is past
        // `exports` (avoids mistaking e.g. an indexed `module`/`exports`).
        if d > roles.exports_idx {
            roles.deps_idx = Some(d);
        }
    }

    let mut names = vec![None; declared as usize];
    let set = |names: &mut Vec<Option<String>>, idx: u32, name: &str| {
        if (idx as usize) < names.len() {
            names[idx as usize] = Some(name.to_string());
        }
    };
    set(&mut names, roles.global_idx, "global");
    set(&mut names, roles.require_idx, "require");
    set(&mut names, roles.module_idx, "module");
    set(&mut names, roles.exports_idx, "exports");
    if let Some(idx) = roles.import_default_idx {
        set(&mut names, idx, "importDefault");
    }
    if let Some(idx) = roles.import_all_idx {
        set(&mut names, idx, "importAll");
    }
    if let Some(idx) = roles.deps_idx {
        set(&mut names, idx, "dependencyMap");
    }

    if names.iter().all(|n| n.is_none()) {
        return None;
    }

    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    fn param(idx: u32) -> Expression {
        Expression::Value(Value::Parameter(idx))
    }

    // dependencyMap[k]
    fn dep_index(deps_idx: u32, k: i64) -> Expression {
        Expression::Member {
            object: Box::new(param(deps_idx)),
            property: PropertyKey::Index(k),
            optional: false,
        }
    }

    #[test]
    fn modern_7_param_factory_named_from_usage() {
        // function(global, require, importDefault, importAll, module, exports, deps) {
        //   module.exports = require(undefined, deps[0]);
        // }
        let stmts = vec![Statement::Assign {
            target: AssignTarget::Member {
                object: param(4),
                property: "exports".into(),
            },
            value: Expression::Call {
                callee: Box::new(param(1)),
                arguments: vec![
                    Expression::Value(Value::Constant(Constant::Undefined)),
                    dep_index(6, 0),
                ],
            },
        }];
        let mut stmts = stmts;
        let names = infer_commonjs_names(&mut stmts, 8).expect("factory should be detected");
        assert_eq!(names[0].as_deref(), Some("global"));
        assert_eq!(names[1].as_deref(), Some("require"));
        assert_eq!(names[2].as_deref(), Some("importDefault"));
        assert_eq!(names[3].as_deref(), Some("importAll"));
        assert_eq!(names[4].as_deref(), Some("module"));
        assert_eq!(names[5].as_deref(), Some("exports"));
        assert_eq!(names[6].as_deref(), Some("dependencyMap"));
    }

    #[test]
    fn classic_4_param_factory_named() {
        // function(global, require, module, exports) { module.exports = ...; }
        let mut stmts = vec![Statement::Assign {
            target: AssignTarget::Member {
                object: param(2),
                property: "exports".into(),
            },
            value: Expression::Value(Value::Constant(Constant::Integer(1))),
        }];
        let names = infer_commonjs_names(&mut stmts, 5).expect("classic factory detected");
        assert_eq!(names[2].as_deref(), Some("module"));
        assert_eq!(names[3].as_deref(), Some("exports"));
        assert_eq!(names[1].as_deref(), Some("require"));
    }

    #[test]
    fn non_factory_function_not_renamed() {
        // An ordinary 4-arg function with no Metro signals must be left alone.
        let mut stmts = vec![Statement::Assign {
            target: AssignTarget::Register(0),
            value: Expression::Binary {
                op: crate::ir::BinaryOp::Add,
                left: Box::new(param(0)),
                right: Box::new(param(1)),
            },
        }];
        assert!(infer_commonjs_names(&mut stmts, 5).is_none());
    }
}
