// Async detection: Babel async-to-generator pattern detection and unwrapping.

// Maximum depth for following single-return wrapper chains to find the innermost body.
const MAX_WRAPPER_CHAIN_DEPTH: usize = 10;

use std::collections::BTreeMap;
use crate::analysis::ClosureContext;
use crate::file::BytecodeFile;
use crate::ir::Statement;

// Detect the Babel async-to-generator pattern: `asyncGeneratorStep.default(function*() { ... })`
// or `_asyncToGenerator(function*() { ... })`. Returns the function IDs of generator functions
// that are actually async function bodies.
pub(super) fn detect_async_generator_wrappers(all_ir: &BTreeMap<u32, Vec<Statement>>) -> Vec<u32> {
    let mut async_func_ids = Vec::new();

    for stmts in all_ir.values() {
        for stmt in stmts {
            collect_async_generators_from_stmt(stmt, &mut async_func_ids);
        }
    }

    async_func_ids
}

fn collect_async_generators_from_stmt(stmt: &Statement, results: &mut Vec<u32>) {

    match stmt {
        Statement::Assign { value, .. } | Statement::Let { value, .. } => {
            collect_async_generators_from_expr(value, results);
        }
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_async_generators_from_expr(e, results);
        }
        Statement::If { condition, then_body, else_body } => {
            collect_async_generators_from_expr(condition, results);
            for s in then_body { collect_async_generators_from_stmt(s, results); }
            for s in else_body { collect_async_generators_from_stmt(s, results); }
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            collect_async_generators_from_expr(condition, results);
            for s in body { collect_async_generators_from_stmt(s, results); }
        }
        Statement::For { init, condition, update, body } => {
            if let Some(s) = init { collect_async_generators_from_stmt(s, results); }
            if let Some(e) = condition { collect_async_generators_from_expr(e, results); }
            if let Some(s) = update { collect_async_generators_from_stmt(s, results); }
            for s in body { collect_async_generators_from_stmt(s, results); }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body { collect_async_generators_from_stmt(s, results); }
            for s in catch_body { collect_async_generators_from_stmt(s, results); }
            for s in finally_body { collect_async_generators_from_stmt(s, results); }
        }
        Statement::Block(inner) => {
            for s in inner { collect_async_generators_from_stmt(s, results); }
        }
        _ => {}
    }
}

fn collect_async_generators_from_expr(expr: &crate::ir::Expression, results: &mut Vec<u32>) {
    use crate::ir::Expression;

    match expr {
        // Pattern 1: asyncGeneratorStep.default(function*() { ... })
        // Pattern 2: _asyncToGenerator(function*() { ... })
        Expression::Call { callee, arguments } => {
            // Check for any call with a generator function as first argument
            // In Babel async, this is the _asyncToGenerator(function*() {...}) pattern
            let helper = is_async_helper_callee(callee);
            for arg in arguments {
                if let Expression::Function { id, is_generator, .. } = arg {
                    if *is_generator || helper {
                        results.push(id.0);
                    }
                }
            }
            // Recurse into callee and arguments
            collect_async_generators_from_expr(callee, results);
            for arg in arguments {
                collect_async_generators_from_expr(arg, results);
            }
        }
        Expression::Binary { left, right, .. } => {
            collect_async_generators_from_expr(left, results);
            collect_async_generators_from_expr(right, results);
        }
        Expression::Unary { operand, .. } => {
            collect_async_generators_from_expr(operand, results);
        }
        Expression::Conditional { condition, then_expr, else_expr } => {
            collect_async_generators_from_expr(condition, results);
            collect_async_generators_from_expr(then_expr, results);
            collect_async_generators_from_expr(else_expr, results);
        }
        Expression::Member { object, .. } => {
            collect_async_generators_from_expr(object, results);
        }
        Expression::Assignment { target, value } => {
            collect_async_generators_from_expr(target, results);
            collect_async_generators_from_expr(value, results);
        }
        Expression::Array { elements } => {
            for e in elements.iter().flatten() {
                collect_async_generators_from_expr(e, results);
            }
        }
        Expression::Object { properties } => {
            for p in properties {
                collect_async_generators_from_expr(&p.value, results);
            }
        }
        Expression::Spread(inner) | Expression::Await(inner) => {
            collect_async_generators_from_expr(inner, results);
        }
        Expression::Yield { value, .. } => {
            collect_async_generators_from_expr(value, results);
        }
        Expression::New { callee, arguments } => {
            collect_async_generators_from_expr(callee, results);
            for arg in arguments {
                collect_async_generators_from_expr(arg, results);
            }
        }
        _ => {}
    }
}

fn is_async_helper_callee(callee: &crate::ir::Expression) -> bool {
    use crate::ir::{Expression, PropertyKey, Value};
    match callee {
        Expression::Value(Value::Variable(n)) => looks_like_async_helper(n),
        Expression::Member {
            object,
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } => {
            // `helper.default(...)` from a transpiled bundle, or a member whose own
            // name is the helper: HBC >=97 emits `HermesBuiltin.spawnAsync(...)` for
            // a native `async function`, so the property carries the name.
            if p == "default" {
                is_async_helper_callee(object)
            } else {
                looks_like_async_helper(p)
            }
        }
        _ => false,
    }
}

fn looks_like_async_helper(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("async") || n.contains("generator") || n.contains("awaiter")
}

// `return asyncGeneratorStep(async () => { ... })()` → `return (async () => { ... })()`
// once the inner function is already async. The Babel helper is then redundant.
pub(super) fn strip_redundant_async_helpers(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    ctx: &crate::analysis::ClosureContext,
) {
    for body in all_ir.values_mut() {
        for stmt in body.iter_mut() {
            strip_redundant_async_helper_stmt(stmt, ctx);
        }
    }
}

fn strip_redundant_async_helper_stmt(stmt: &mut Statement, ctx: &crate::analysis::ClosureContext) {
    match stmt {
        Statement::Return(Some(e)) | Statement::Expr(e) | Statement::Throw(e) => {
            strip_redundant_async_helper_expr(e, ctx);
        }
        Statement::Let { value, .. } | Statement::Assign { value, .. } => {
            strip_redundant_async_helper_expr(value, ctx);
        }
        Statement::If { then_body, else_body, condition, .. } => {
            strip_redundant_async_helper_expr(condition, ctx);
            for s in then_body.iter_mut().chain(else_body.iter_mut()) {
                strip_redundant_async_helper_stmt(s, ctx);
            }
        }
        _ => {}
    }
}

fn strip_redundant_async_helper_expr(expr: &mut crate::ir::Expression, ctx: &crate::analysis::ClosureContext) {
    use crate::ir::{Expression, FunctionId, Value};

    if let Expression::Call { callee, arguments } = expr {
        let empty_or_undef = arguments.is_empty()
            || (arguments.len() == 1
                && matches!(
                    &arguments[0],
                    Expression::Value(Value::Constant(crate::ir::Constant::Undefined))
                ));
        if empty_or_undef {
            if let Expression::Call { callee: helper, arguments: inner_args } = callee.as_ref() {
                if is_async_helper_callee(helper) {
                    if let Some(Expression::Function { id, name, is_arrow, .. }) =
                        inner_args.iter().find(|a| matches!(a, Expression::Function { .. }))
                    {
                        if ctx.is_async(id.0) {
                            *expr = Expression::Call {
                                callee: Box::new(Expression::Function {
                                    id: FunctionId(id.0),
                                    name: name.clone(),
                                    is_arrow: *is_arrow,
                                    is_async: true,
                                    is_generator: false,
                                }),
                                arguments: Vec::new(),
                            };
                            return;
                        }
                    }
                }
            }
        }
        strip_redundant_async_helper_expr(callee, ctx);
        for a in arguments.iter_mut() {
            strip_redundant_async_helper_expr(a, ctx);
        }
    }
}

// Unwrap Babel async-to-generator wrapper functions.
//
// Pattern (after variable inlining):
// ```js
// function _foo(arg0) {
//   return asyncGeneratorStep.default(function*() {...})(...arguments);
// }
// ```
// or (before inlining):
// ```js
// function _foo(arg0) {
//   const defaultResult = asyncGeneratorStep.default(function*() {...});
//   return defaultResult(...arguments);
// }
// ```
//
// These wrappers are replaced with the body of the inner generator function,
// and the wrapper is marked as async.
pub(super) fn unwrap_async_wrappers(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &mut ClosureContext,
    param_names: &mut BTreeMap<u32, Vec<Option<String>>>,
    file: &BytecodeFile,
) -> usize {
    // Step 1: Detect wrappers, collect (wrapper_id, inner_generator_id)
    let mut wrappers: Vec<(u32, u32)> = Vec::new();

    let mut async_keys: Vec<_> = all_ir.keys().copied().collect();
    async_keys.sort();
    for func_id in async_keys {
        let stmts = &all_ir[&func_id];
        if let Some(inner_id) = detect_async_wrapper_pattern(stmts) {
            wrappers.push((func_id, inner_id));
        }
    }

    let count = wrappers.len();

    for (wrapper_id, inner_id) in wrappers {
        // Step 2: Follow the chain, if inner's body is just Return(Function{C}), use C
        let body_id = find_innermost_body(all_ir, inner_id);

        // Step 3: Copy body's IR to the wrapper function
        if let Some(body_stmts) = all_ir.get(&body_id).cloned() {
            all_ir.insert(wrapper_id, body_stmts);
        }

        // Step 4: Mark wrapper as async
        closure_ctx.mark_async(wrapper_id);

        // Step 5: Ensure wrapper has enough params to cover the body's usage.
        // The inner function may have more params than the outer wrapper
        // (e.g., generator context params). Override wrapper's param count
        // so the rendered signature includes all referenced params.
        let body_param_count = file
            .function_headers
            .get(body_id as usize)
            .map(|h| h.param_count())
            .unwrap_or(0) as usize;
        let wrapper_param_count = file
            .function_headers
            .get(wrapper_id as usize)
            .map(|h| h.param_count())
            .unwrap_or(0) as usize;

        if body_param_count > wrapper_param_count {
            // Copy inner function's IPA names if available, otherwise generate defaults
            let names: Vec<Option<String>> = (0..body_param_count)
                .map(|i| {
                    param_names
                        .get(&body_id)
                        .and_then(|n| n.get(i).cloned())
                        .flatten()
                })
                .collect();
            param_names.insert(wrapper_id, names);
        }
    }

    count
}

// Follow the chain of single-return-function bodies to find the innermost real body.
// B's body might be just `Return(Function{C})`, and C has the actual code.
fn find_innermost_body(all_ir: &BTreeMap<u32, Vec<Statement>>, start_id: u32) -> u32 {
    let mut current = start_id;
    for _ in 0..MAX_WRAPPER_CHAIN_DEPTH {
        if let Some(stmts) = all_ir.get(&current) {
            if let Some(inner) = extract_single_return_function_id(stmts) {
                current = inner;
                continue;
            }
        }
        break;
    }
    current
}

// Detect the async wrapper pattern in a function body.
// Returns the inner generator function ID if the pattern matches.
fn detect_async_wrapper_pattern(stmts: &[Statement]) -> Option<u32> {
    use crate::ir::AssignTarget;

    // Case 1: Single statement (after variable inlining)
    // return CALL(..., Function{B})(..arguments)
    if stmts.len() == 1 {
        if let Statement::Return(Some(outer_call)) = &stmts[0] {
            return extract_wrapper_from_nested_call(outer_call);
        }
    }

    // Case 2: Two+ statements (before inlining)
    // let/assign X = CALL(..., Function{B, is_generator: true})
    // return X(...arguments) or return X.apply(this, arguments)
    if stmts.len() >= 2 && stmts.len() <= 4 {
        let (var_name, inner_id) = match &stmts[0] {
            Statement::Let { name, value, .. } => {
                extract_generator_from_call(value).map(|id| (name.clone(), id))?
            }
            Statement::Assign {
                target: AssignTarget::Variable(name),
                value,
            } => extract_generator_from_call(value).map(|id| (name.clone(), id))?,
            _ => return None,
        };

        // Check remaining statements for the return with arguments forwarding
        for stmt in &stmts[1..] {
            if let Statement::Return(Some(expr)) = stmt {
                if is_arguments_forward_call(expr, &var_name) {
                    return Some(inner_id);
                }
            }
        }
    }

    // Case 3: Hermes `_asyncToGenerator(fn).apply(this, arguments)` with a
    // typeof-apply / applyArguments fallback, plus env-slot stores of the
    // helper result. `_resolveGiftCode` is this shape (6+ statements).
    detect_apply_forwarded_async_helper(stmts)
}

fn detect_apply_forwarded_async_helper(stmts: &[Statement]) -> Option<u32> {
    let mut helper_var: Option<String> = None;
    let mut inner_id: Option<u32> = None;
    for stmt in stmts {
        if let Some((name, id)) = async_helper_assignment(stmt) {
            if helper_var.is_some() {
                return None;
            }
            helper_var = Some(name);
            inner_id = Some(id);
        }
    }
    let var = helper_var?;
    let id = inner_id?;
    if stmts.iter().all(|s| {
        async_helper_assignment(s).is_some() || is_apply_forward_boilerplate(s, &var)
    }) {
        Some(id)
    } else {
        None
    }
}

fn async_helper_assignment(stmt: &Statement) -> Option<(String, u32)> {
    use crate::ir::AssignTarget;
    match stmt {
        Statement::Let { name, value, .. } => {
            extract_function_from_async_helper_call(value).map(|id| (name.clone(), id))
        }
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value,
        } => extract_function_from_async_helper_call(value).map(|id| (name.clone(), id)),
        _ => None,
    }
}

fn extract_function_from_async_helper_call(expr: &crate::ir::Expression) -> Option<u32> {
    use crate::ir::Expression;
    let Expression::Call { callee, arguments } = expr else {
        return extract_generator_from_call(expr);
    };
    if !is_async_helper_callee(callee) {
        return extract_generator_from_call(expr);
    }
    for arg in arguments {
        if let Expression::Function { id, .. } = arg {
            return Some(id.0);
        }
    }
    None
}

fn is_apply_forward_boilerplate(stmt: &Statement, helper_var: &str) -> bool {
    use crate::ir::{AssignTarget, Expression, Value};
    match stmt {
        Statement::Comment(_) => true,
        Statement::Let { name, value, .. } => {
            is_apply_forward_value(name, value, helper_var)
        }
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value,
        } => is_apply_forward_value(name, value, helper_var),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            is_typeof_apply_check(condition)
                && then_body
                    .iter()
                    .all(|s| is_apply_forward_boilerplate(s, helper_var))
                && else_body
                    .iter()
                    .all(|s| is_apply_forward_boilerplate(s, helper_var))
        }
        Statement::Return(Some(e)) => {
            is_arguments_forward_call(e, helper_var)
                || matches!(e, Expression::Value(Value::Variable(n)) if n == helper_var || n == "applyArgumentsResult" || n == "apply")
                || is_apply_or_apply_arguments_call(e, helper_var)
        }
        Statement::Expr(e) => is_apply_or_apply_arguments_call(e, helper_var),
        _ => false,
    }
}

fn is_apply_forward_value(name: &str, value: &crate::ir::Expression, helper_var: &str) -> bool {
    use crate::ir::{Expression, PropertyKey, Value};
    if matches!(value, Expression::Value(Value::This)) {
        return true;
    }
    if is_env_slot_name(name)
        && matches!(value, Expression::Value(Value::Variable(v)) if v == helper_var)
    {
        return true;
    }
    if is_env_slot_name(name)
        && matches!(
            value,
            Expression::Value(Value::Constant(
                crate::ir::Constant::Integer(0) | crate::ir::Constant::Undefined
            ))
        )
    {
        return true;
    }
    if matches!(
        value,
        Expression::Member {
            object,
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } if p == "apply"
            && matches!(object.as_ref(), Expression::Value(Value::Variable(v)) if v == helper_var)
    ) {
        return true;
    }
    is_apply_or_apply_arguments_call(value, helper_var)
}

fn is_env_slot_name(name: &str) -> bool {
    name.starts_with("closure_")
        || (name.len() >= 2
            && name.starts_with('c')
            && name[1..].chars().all(|c| c.is_ascii_digit()))
}

fn is_typeof_apply_check(expr: &crate::ir::Expression) -> bool {
    use crate::ir::{BinaryOp, Constant, Expression, UnaryOp, Value};
    let Expression::Binary {
        op: BinaryOp::Eq | BinaryOp::StrictEq | BinaryOp::Neq | BinaryOp::StrictNeq,
        left,
        right,
    } = expr
    else {
        return false;
    };
    let is_unknown = |e: &Expression| {
        matches!(
            e,
            Expression::Value(Value::Constant(Constant::String(s)))
                if s == "unknown" || s == "undefined" || s == "function"
        )
    };
    let is_typeof = |e: &Expression| {
        matches!(e, Expression::Unary { op: UnaryOp::TypeOf, .. })
    };
    (is_typeof(left) && is_unknown(right)) || (is_typeof(right) && is_unknown(left))
}

fn is_apply_or_apply_arguments_call(expr: &crate::ir::Expression, helper_var: &str) -> bool {
    use crate::ir::{Expression, PropertyKey, Value};
    let Expression::Call { callee, arguments } = expr else {
        return false;
    };
    if is_arguments_forward_call(expr, helper_var) {
        return true;
    }
    match callee.as_ref() {
        Expression::Value(Value::Variable(n)) if n == "apply" || n == helper_var => {
            arguments.iter().any(|a| match a {
                Expression::Value(Value::Arguments) => true,
                Expression::Spread(inner) => {
                    matches!(&**inner, Expression::Value(Value::Arguments))
                }
                _ => false,
            })
        }
        Expression::Member {
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } if p == "applyArguments" || p == "apply" => true,
        _ => false,
    }
}

// Extract generator function ID from a nested call pattern (after inlining):
// `CALL(..., Function{B})(..arguments)` → B's ID
fn extract_wrapper_from_nested_call(expr: &crate::ir::Expression) -> Option<u32> {
    use crate::ir::{Expression, Value};

    if let Expression::Call { callee, arguments } = expr {
        // Outer call must forward arguments via spread
        let has_arg_spread = arguments.iter().any(|a| {
            matches!(
                a,
                Expression::Spread(inner) if matches!(&**inner, Expression::Value(Value::Arguments))
            )
        });
        if !has_arg_spread {
            return None;
        }

        // The callee should be a Call with a generator Function argument
        if let Expression::Call {
            arguments: inner_args,
            ..
        } = &**callee
        {
            for arg in inner_args {
                if let Expression::Function {
                    id,
                    is_generator: true,
                    ..
                } = arg
                {
                    return Some(id.0);
                }
            }
        }
    }
    None
}

// Extract a generator function ID from a Call expression that has a Function{is_generator: true} argument.
fn extract_generator_from_call(expr: &crate::ir::Expression) -> Option<u32> {
    use crate::ir::Expression;

    if let Expression::Call { arguments, .. } = expr {
        for arg in arguments {
            if let Expression::Function {
                id,
                is_generator: true,
                ..
            } = arg
            {
                return Some(id.0);
            }
        }
    }
    None
}

// Check if an expression calls a variable with `...arguments` forwarding.
// Matches `VAR(...arguments)` or `VAR.apply(this, arguments)`.
fn is_arguments_forward_call(expr: &crate::ir::Expression, var_name: &str) -> bool {
    use crate::ir::{Expression, PropertyKey, Value};

    if let Expression::Call { callee, arguments } = expr {
        match &**callee {
            // Pattern 1: VAR(...arguments)
            Expression::Value(Value::Variable(name)) if name == var_name => {
                return arguments.iter().any(|a| {
                    matches!(
                        a,
                        Expression::Spread(inner)
                            if matches!(&**inner, Expression::Value(Value::Arguments))
                    )
                });
            }
            // Pattern 2: VAR.apply(this, arguments)
            Expression::Member {
                object,
                property: PropertyKey::Ident(prop),
                ..
            } if prop == "apply" => {
                if let Expression::Value(Value::Variable(name)) = &**object {
                    if name == var_name {
                        return arguments
                            .iter()
                            .any(|a| matches!(a, Expression::Value(Value::Arguments)));
                    }
                }
            }
            _ => {}
        }
    }
    false
}

// If a function body is just `Return(Function { id: C })`, extract C's ID.
// Handles both direct returns and assign-then-return patterns.
fn extract_single_return_function_id(stmts: &[Statement]) -> Option<u32> {
    use crate::ir::{AssignTarget, Expression, Value};

    // Filter out comments
    let meaningful: Vec<_> = stmts
        .iter()
        .filter(|s| !matches!(s, Statement::Comment(_)))
        .collect();

    // Single return of a function expression
    if meaningful.len() == 1 {
        if let Statement::Return(Some(Expression::Function { id, .. })) = meaningful[0] {
            return Some(id.0);
        }
    }

    // Assign to register, then return that register
    if meaningful.len() == 2 {
        if let Statement::Assign {
            target: AssignTarget::Register(r),
            value: Expression::Function { id, .. },
        } = meaningful[0]
        {
            if let Statement::Return(Some(Expression::Value(Value::Register(r2)))) = meaningful[1] {
                if *r == *r2 {
                    return Some(id.0);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        AssignTarget, BinaryOp, Constant, Expression, FunctionId, PropertyKey, Statement,
        UnaryOp, Value, VarKind,
    };

    fn async_helper_call(inner: u32) -> Expression {
        Expression::Call {
            callee: Box::new(Expression::Value(Value::Variable(
                "asyncGeneratorStep".into(),
            ))),
            arguments: vec![Expression::Function {
                id: FunctionId(inner),
                name: None,
                is_arrow: true,
                is_async: true,
                is_generator: false,
            }],
        }
    }

    #[test]
    fn detects_hermes_apply_forward_wrapper() {
        let apply_member = Expression::Member {
            object: Box::new(Expression::Value(Value::Variable("tmp".into()))),
            property: PropertyKey::Ident("apply".into()),
            optional: false,
        };
        let typeof_apply = Expression::Unary {
            op: UnaryOp::TypeOf,
            operand: Box::new(Expression::Value(Value::Variable("apply".into()))),
        };
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Variable("self".into()),
                value: Expression::Value(Value::This),
            },
            Statement::Let {
                name: "tmp".into(),
                value: async_helper_call(42),
                kind: VarKind::Const,
            },
            Statement::Assign {
                target: AssignTarget::Variable("closure_18".into()),
                value: Expression::Value(Value::Variable("tmp".into())),
            },
            Statement::Let {
                name: "apply".into(),
                value: apply_member,
                kind: VarKind::Const,
            },
            Statement::If {
                condition: Expression::Binary {
                    op: BinaryOp::StrictEq,
                    left: Box::new(typeof_apply),
                    right: Box::new(Expression::Value(Value::Constant(Constant::String(
                        "unknown".into(),
                    )))),
                },
                then_body: vec![Statement::Let {
                    name: "applyArgumentsResult".into(),
                    value: Expression::Call {
                        callee: Box::new(Expression::Member {
                            object: Box::new(Expression::Value(Value::Variable(
                                "HermesBuiltin".into(),
                            ))),
                            property: PropertyKey::Ident("applyArguments".into()),
                            optional: false,
                        }),
                        arguments: vec![Expression::Value(Value::Variable("self".into()))],
                    },
                    kind: VarKind::Let,
                }],
                else_body: vec![Statement::Assign {
                    target: AssignTarget::Variable("applyArgumentsResult".into()),
                    value: Expression::Call {
                        callee: Box::new(Expression::Value(Value::Variable("apply".into()))),
                        arguments: vec![
                            Expression::Value(Value::Variable("self".into())),
                            Expression::Value(Value::Arguments),
                        ],
                    },
                }],
            },
            Statement::Return(Some(Expression::Value(Value::Variable(
                "applyArgumentsResult".into(),
            )))),
        ];
        assert_eq!(detect_async_wrapper_pattern(&stmts), Some(42));
    }
}
