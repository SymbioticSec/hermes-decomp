// Rendering: inline body building and this-usage detection helpers.

// Maximum multi-pass iterations for inline body rendering.
// Handles deep nesting chains; 5 passes covers most real-world depths.
const MAX_INLINE_BODY_PASSES: usize = 5;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use rayon::prelude::*;
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

        // Reverse dependency map: child function id -> parents that reference it by
        // id (`Expression::Function { id }`). A parent's rendered string only
        // changes between passes when one of the inline bodies it references
        // changed, so later passes re-render only the affected parents instead of
        // every function. This turns the multi-pass cost from passes x all
        // functions into all functions plus a quickly shrinking dirty set.
        let mut referenced_by: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (&func_id, (_, body_stmts)) in &prepared {
            let mut refs = Vec::new();
            collect_function_refs(body_stmts, &mut refs);
            refs.sort_unstable();
            refs.dedup();
            for child in refs {
                referenced_by.entry(child).or_default().push(func_id);
            }
        }

        // Pass 1: render every function against an empty inline map. Everything
        // produced counts as changed relative to that empty starting point.
        let mut current =
            self.render_prepared_bodies(file, &prepared, &Arc::new(BTreeMap::new()), None);
        let mut changed: Vec<u32> = current.keys().copied().collect();

        for _ in 1..MAX_INLINE_BODY_PASSES {
            // Dirty set: parents that reference a body which changed last pass.
            let mut dirty: BTreeSet<u32> = BTreeSet::new();
            for c in &changed {
                if let Some(parents) = referenced_by.get(c) {
                    dirty.extend(parents.iter().copied());
                }
            }
            if dirty.is_empty() {
                break;
            }

            let shared = Arc::new(std::mem::take(&mut current));
            let updated = self.render_prepared_bodies(file, &prepared, &shared, Some(&dirty));
            current = Arc::try_unwrap(shared).unwrap_or_else(|a| (*a).clone());

            changed.clear();
            for (id, body) in updated {
                if current.get(&id).is_none_or(|prev| prev != &body) {
                    changed.push(id);
                }
                current.insert(id, body);
            }
            if changed.is_empty() {
                break;
            }
        }

        self.inline_bodies = Arc::new(current);
    }

    // The recovered worklet source for `func_id`, looked up by the function's
    // bytecode name (the join key, both the name and the source come from the
    // binary). Returns the source already shaped as a function expression.
    pub(super) fn worklet_source_for(&self, file: &BytecodeFile, func_id: u32) -> Option<String> {
        if self.worklet_sources.is_empty() {
            return None;
        }
        let name = file
            .function_headers
            .get(func_id as usize)
            .and_then(|h| file.string_at(h.function_name()))
            .map(|e| e.value.clone())?;
        let src = self.worklet_sources.get(&name)?;
        Some(format!("/* worklet (recovered source) */ {src}"))
    }

    // Precompute IPA-renamed, cleaned, and declaration-inserted statements for all
    // non-factory functions. Each function is prepared independently from read only
    // shared state, so fan the work out across cores.
    fn prepare_render_bodies(&self, file: &BytecodeFile) -> BTreeMap<u32, (Vec<String>, Vec<Statement>)> {
        // Env-slot names only (not every ancestor local), precomputed once on the
        // context so each function is O(slots), not O(ancestor-body-size).
        let ancestor_env = &self.ancestor_env_slots;
        self.all_ir
            .par_iter()
            .filter(|(func_id, _)| !self.registry.function_to_module.contains_key(func_id))
            .map(|(&func_id, stmts)| {
                let params: Vec<String> = if let Some(names) = self.global_analysis.param_names.get(&func_id) {
                    names.iter().enumerate()
                        .map(|(idx, n)| {
                            let raw = n.clone().unwrap_or_else(|| format!("arg{idx}"));
                            crate::util::sanitize_identifier(&raw)
                        })
                        .collect()
                } else {
                    get_function_params(file, func_id)
                        .into_iter()
                        .map(|p| crate::util::sanitize_identifier(&p))
                        .collect()
                };

                let mut body_stmts = stmts.clone();
                if let Some(param_names) = self.global_analysis.param_names.get(&func_id) {
                    transforms::exports::rename_param_registers(&mut body_stmts, param_names);
                }
                body_stmts = transforms::cleanup_noise(body_stmts);
                transforms::rename_reserved_words(&mut body_stmts);
                let extra = self.extra_writes_for_function(func_id);
                let empty = HashSet::new();
                let outer = ancestor_env.get(&func_id).unwrap_or(&empty);
                transforms::insert_declarations_with_outer(
                    &mut body_stmts,
                    &params,
                    &extra,
                    outer,
                    true,
                );

                (func_id, (params, body_stmts))
            })
            .collect()
    }

    // Render pre-prepared function bodies, using `existing_inline` for nested
    // function references. When `only` is `Some`, render just that subset (the
    // parents whose referenced bodies changed in the previous pass); otherwise
    // render every function.
    fn render_prepared_bodies(
        &self,
        file: &BytecodeFile,
        prepared: &BTreeMap<u32, (Vec<String>, Vec<Statement>)>,
        existing_inline: &Arc<BTreeMap<u32, String>>,
        only: Option<&BTreeSet<u32>>,
    ) -> BTreeMap<u32, String> {
        // Each function body renders independently from the shared `existing_inline`
        // map (read only) into its own entry, so a pass over the functions is
        // embarrassingly parallel. On a large bundle this is the dominant cost, so
        // fan it out across cores.
        prepared
            .par_iter()
            .filter(|(func_id, _)| only.is_none_or(|s| s.contains(func_id)))
            .map(|(&func_id, (params, body_stmts))| {
                (func_id, self.render_one_body(file, func_id, params, body_stmts, existing_inline))
            })
            .collect()
    }

    // Render a single function body to its final source string, using
    // `existing_inline` for any nested function references.
    fn render_one_body(
        &self,
        file: &BytecodeFile,
        func_id: u32,
        params: &[String],
        body_stmts: &[Statement],
        existing_inline: &Arc<BTreeMap<u32, String>>,
    ) -> String {
        {
            // If this is a Reanimated worklet, emit its original source recovered
            // from the embedded `__initData.code` string instead of decompiling
            // (the compiled form often mis-renders, e.g. as a `class`).
            if let Some(src) = self.worklet_source_for(file, func_id) {
                return src;
            }
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

            rendered
        }
    }

    // Env-slot names owned by ancestors of `func_id`. Generic ancestor locals
    // (`obj`, `_Error`) are excluded so children still get their own `let`. Reads the
    // map precomputed once on the context rather than rebuilding it per call.
    pub(super) fn ancestor_env_slot_names(&self, func_id: u32) -> HashSet<String> {
        self.ancestor_env_slots.get(&func_id).cloned().unwrap_or_default()
    }

    // Top-down: names(fn) = names(parent) ∪ slot names of parent. One pass, O(n).
    pub(in crate::pipeline) fn precompute_ancestor_env_slot_names(&self) -> BTreeMap<u32, HashSet<String>> {
        let Some(ctx) = self.closure_ctx.as_ref() else {
            return BTreeMap::new();
        };

        let mut own: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
        for (&fid, info) in &ctx.function_closures {
            let mut names = HashSet::new();
            for &key in info.slots.keys() {
                names.insert(info.get_slot_name(key));
            }
            own.insert(fid, names);
        }

        let mut memo: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
        let mut visiting = HashSet::new();
        for &id in self.all_ir.keys() {
            fill_ancestor_env_slots(id, &ctx.parent_function, &own, &mut memo, &mut visiting);
        }
        memo
    }
}

fn fill_ancestor_env_slots(
    id: u32,
    parent_of: &BTreeMap<u32, u32>,
    own: &BTreeMap<u32, HashSet<String>>,
    memo: &mut BTreeMap<u32, HashSet<String>>,
    visiting: &mut HashSet<u32>,
) {
    if memo.contains_key(&id) {
        return;
    }
    if !visiting.insert(id) {
        memo.insert(id, HashSet::new());
        return;
    }
    if let Some(&parent) = parent_of.get(&id) {
        fill_ancestor_env_slots(parent, parent_of, own, memo, visiting);
        let mut set = memo.get(&parent).cloned().unwrap_or_default();
        if let Some(parent_own) = own.get(&parent) {
            set.extend(parent_own.iter().cloned());
        }
        memo.insert(id, set);
    } else {
        memo.insert(id, HashSet::new());
    }
    visiting.remove(&id);
}

// Collect the ids of every function referenced by id (`Expression::Function { id }`)
// anywhere in `stmts`. These are the direct inline-body dependencies of the owning
// function: it re-renders when one of their rendered bodies changes.
fn collect_function_refs(stmts: &[crate::ir::Statement], out: &mut Vec<u32>) {
    use crate::ir::{Expression, Visitor};
    struct C<'a>(&'a mut Vec<u32>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Function { id, .. } = e {
                self.0.push(id.0);
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(out);
    for s in stmts {
        c.visit_statement(s);
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
