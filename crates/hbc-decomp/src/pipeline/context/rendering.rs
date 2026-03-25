// Rendering: inline body building and this-usage detection helpers.

// Maximum multi-pass iterations for inline body rendering.
// Handles deep nesting chains; 5 passes covers most real-world depths.
const MAX_INLINE_BODY_PASSES: usize = 5;

use std::collections::BTreeMap;
use std::sync::Arc;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::transforms::{self, Codegen, CodegenOptions};

use super::super::get_function_params;
use super::PipelineContext;

impl PipelineContext {
    // Build pre-rendered inline function bodies for ALL functions.
    // Multi-pass approach for multi-level nesting support.
    pub(super) fn build_all_inline_bodies(&mut self, file: &BytecodeFile) {
        // Precompute IPA-renamed + cleaned statements once to avoid cloning per rendering pass
        let prepared = self.prepare_render_bodies(file);

        // Multi-pass rendering for deep nesting support
        let mut current = Arc::new(BTreeMap::new());
        for _ in 0..MAX_INLINE_BODY_PASSES {
            current = Arc::new(self.render_prepared_bodies(file, &prepared, &current));
        }
        self.inline_bodies = current;
    }

    // Precompute IPA-renamed, cleaned, and declaration-inserted statements for all non-factory functions.
    fn prepare_render_bodies(&self, file: &BytecodeFile) -> BTreeMap<u32, (Vec<String>, Vec<Statement>)> {
        let mut prepared = BTreeMap::new();
        for (&func_id, stmts) in &self.all_ir {
            if self.registry.function_to_module.contains_key(&func_id) {
                continue;
            }
            let params: Vec<String> = if let Some(names) = self.global_analysis.param_names.get(&func_id) {
                names.iter().enumerate()
                    .map(|(idx, n)| n.clone().unwrap_or_else(|| format!("arg{idx}")))
                    .collect()
            } else {
                get_function_params(file, func_id)
            };

            let mut body_stmts = stmts.clone();
            if let Some(param_names) = self.global_analysis.param_names.get(&func_id) {
                transforms::exports::rename_param_registers(&mut body_stmts, param_names);
            }
            body_stmts = transforms::cleanup_noise(body_stmts);
            transforms::rename_reserved_words(&mut body_stmts);
            transforms::insert_declarations(&mut body_stmts, &params);

            prepared.insert(func_id, (params, body_stmts));
        }
        prepared
    }

    // Render all pre-prepared function bodies, using `existing_inline` for nested function references.
    fn render_prepared_bodies(
        &self,
        file: &BytecodeFile,
        prepared: &BTreeMap<u32, (Vec<String>, Vec<Statement>)>,
        existing_inline: &Arc<BTreeMap<u32, String>>,
    ) -> BTreeMap<u32, String> {
        let mut result = BTreeMap::new();

        for (&func_id, (params, body_stmts)) in prepared {
            // Render the body with existing inline bodies for nested functions
            let mut inner_codegen = Codegen::new(CodegenOptions::default())
                .with_inline_bodies(Arc::clone(existing_inline));
            let module = self.resolve_module_for_function(func_id);
            if let Some(m) = module {
                inner_codegen = inner_codegen.with_imports(self.build_import_map(m));
            }
            let body = inner_codegen.generate_statements(body_stmts);
            // Indent body by one level (2 spaces) for proper nesting inside function { }
            let body_trimmed: String = body
                .trim_end()
                .lines()
                .map(|line| if line.is_empty() { String::new() } else { format!("  {line}") })
                .collect::<Vec<_>>()
                .join("\n");

            // Get function properties (is_arrow, etc.) from closure context and bytecode header
            let is_async = self.closure_ctx.as_ref().is_some_and(|c| c.is_async(func_id));
            let is_generator = self.closure_ctx.as_ref().is_some_and(|c| c.is_generator(func_id));
            // Async generators (Babel pattern) should render as async, not function*
            let is_generator = is_generator && !is_async;
            // Arrow heuristic: anonymous, not generator, doesn't use `this`
            let uses_this = stmts_use_this(body_stmts);
            let is_arrow = !is_generator && !uses_this;
            let func_name = file.function_headers
                .get(func_id as usize)
                .and_then(|h| file.string_at(h.function_name()))
                .filter(|e| !e.value.is_empty() && crate::util::is_valid_identifier(&e.value))
                .map(|e| e.value.clone());

            let async_prefix = if is_async { "async " } else { "" };
            let gen_star = if is_generator { "*" } else { "" };
            let params_str = params.join(", ");

            let rendered = if is_arrow && func_name.is_none() {
                // Arrow function rendering
                if body_stmts.len() == 1 {
                    if let Statement::Return(Some(expr)) = &body_stmts[0] {
                        // Concise arrow: (params) => expr
                        let expr_str = inner_codegen.generate_statements(&[Statement::Expr(expr.clone())]);
                        let expr_trimmed = expr_str.trim().trim_end_matches(';');
                        // Wrap in parens if it starts with { (object literal ambiguity)
                        if expr_trimmed.starts_with('{') {
                            format!("{async_prefix}({params_str}) => ({expr_trimmed})")
                        } else {
                            format!("{async_prefix}({params_str}) => {expr_trimmed}")
                        }
                    } else {
                        // Single non-return statement: block arrow
                        format!("{async_prefix}({params_str}) => {{\n{body_trimmed}\n}}")
                    }
                } else {
                    // Block arrow
                    format!("{async_prefix}({params_str}) => {{\n{body_trimmed}\n}}")
                }
            } else {
                // Regular function rendering
                match &func_name {
                    Some(n) => format!(
                        "{async_prefix}function{gen_star} {n}({params_str}) {{\n{body_trimmed}\n}}"
                    ),
                    None => format!(
                        "{async_prefix}function{gen_star}({params_str}) {{\n{body_trimmed}\n}}"
                    ),
                }
            };

            result.insert(func_id, rendered);
        }

        result
    }
}

// Check if any statement in the list references `this` (non-recursing into nested functions).
pub(super) fn stmts_use_this(stmts: &[crate::ir::Statement]) -> bool {
    for stmt in stmts {
        if stmt_uses_this(stmt) {
            return true;
        }
    }
    false
}

fn stmt_uses_this(stmt: &crate::ir::Statement) -> bool {
    use crate::ir::Statement;
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => expr_uses_this(e),
        Statement::Assign { target, value } => target_uses_this(target) || expr_uses_this(value),
        Statement::Let { value, .. } => expr_uses_this(value),
        Statement::If { condition, then_body, else_body } => {
            expr_uses_this(condition) || stmts_use_this(then_body) || stmts_use_this(else_body)
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            expr_uses_this(condition) || stmts_use_this(body)
        }
        Statement::For { init, condition, update, body } => {
            init.as_ref().is_some_and(|s| stmt_uses_this(s))
                || condition.as_ref().is_some_and(expr_uses_this)
                || update.as_ref().is_some_and(|s| stmt_uses_this(s))
                || stmts_use_this(body)
        }
        Statement::ForIn { object, body, .. } => expr_uses_this(object) || stmts_use_this(body),
        Statement::ForOf { iterable, body, .. } => expr_uses_this(iterable) || stmts_use_this(body),
        Statement::Block(inner) => stmts_use_this(inner),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            stmts_use_this(try_body) || stmts_use_this(catch_body) || stmts_use_this(finally_body)
        }
        Statement::Switch { discriminant, cases, default } => {
            expr_uses_this(discriminant)
                || cases.iter().any(|(e, body)| expr_uses_this(e) || stmts_use_this(body))
                || default.as_ref().is_some_and(|d| stmts_use_this(d))
        }
        _ => false,
    }
}

fn expr_uses_this(expr: &crate::ir::Expression) -> bool {
    use crate::ir::{Expression, Value};
    match expr {
        Expression::Value(Value::This) => true,
        Expression::Binary { left, right, .. } => expr_uses_this(left) || expr_uses_this(right),
        Expression::Unary { operand, .. } => expr_uses_this(operand),
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            expr_uses_this(callee) || arguments.iter().any(expr_uses_this)
        }
        Expression::Member { object, .. } => expr_uses_this(object),
        Expression::Conditional { condition, then_expr, else_expr } => {
            expr_uses_this(condition) || expr_uses_this(then_expr) || expr_uses_this(else_expr)
        }
        Expression::Array { elements } => elements.iter().flatten().any(expr_uses_this),
        Expression::Object { properties } => properties.iter().any(|p| expr_uses_this(&p.value)),
        Expression::Assignment { target, value } => expr_uses_this(target) || expr_uses_this(value),
        Expression::Spread(inner) | Expression::Await(inner) => expr_uses_this(inner),
        Expression::Yield { value, .. } => expr_uses_this(value),
        Expression::TemplateLiteral { expressions, .. } => expressions.iter().any(expr_uses_this),
        Expression::Function { .. } => false, // Don't recurse into nested functions
        _ => false,
    }
}

fn target_uses_this(target: &crate::ir::AssignTarget) -> bool {
    use crate::ir::AssignTarget;
    match target {
        AssignTarget::Member { object, .. } => expr_uses_this(object),
        AssignTarget::Index { object, key } => expr_uses_this(object) || expr_uses_this(key),
        _ => false,
    }
}
