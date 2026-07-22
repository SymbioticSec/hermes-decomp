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
            if let Some(Expression::Function { id, is_generator: true, .. }) = arguments.first() {
                results.push(id.0);
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

    None
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
