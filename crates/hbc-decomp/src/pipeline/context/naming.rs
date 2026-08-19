// Phase 3: module naming, closure resolution, export analysis, IPA.
use std::collections::BTreeMap;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::transforms;
use super::super::build_function_name_index;
use super::PipelineContext;

impl PipelineContext {
    pub(super) fn run_naming_pipeline(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        registry: &mut crate::analysis::MetroRegistry,
        closure_ctx: &mut Option<crate::analysis::ClosureContext>,
        file: &BytecodeFile,
        deep: bool,
        stable: bool,
    ) -> crate::analysis::GlobalAnalysis {
        // STAGE W4b: name modules from the source file encoded in their function
        // names. Hermes bakes `<fn>_<package>_<file>Ts<N>` (or `<file>Tsx<N>`) into
        // the name table for library and worklet functions, so the source file, which
        // IS the module, is recoverable ground truth. Runs before propagation so the
        // names flow into imports and closure captures.
        let named_src = name_modules_from_source_files(file, registry, closure_ctx.as_ref());
        if named_src > 0 {
            log::debug!("[pipeline] module naming from source files: {named_src} named");
        }

        // STAGE W5: Module Name Propagation
        let t = std::time::Instant::now();
        crate::analysis::metro::propagate_module_names(all_ir, registry, closure_ctx);
        log::debug!("[pipeline] module name propagation: {:.2?}", t.elapsed());

        // STAGE W6: Closure Resolution (first pass)
        // Re-analyze slots from current IR, then apply Metro roles only on
        // factory functions so children inherit `require`/`dependencyMap`.
        // Nested helpers that *reuse* the same slot index drop the role via
        // prefer_local_over_inherited (avoids `let require = Symbol_iterator`).
        let t = std::time::Instant::now();
        if closure_ctx.is_some() {
            // First resolve: rebuild slot maps from IR that still has ClosureVar stores,
            // then apply Metro factory param roles.
            if let Some(ctx) = closure_ctx.as_mut() {
                ctx.reanalyze_all(all_ir);
                ctx.apply_metro_factory_param_roles(|id| {
                    registry
                        .function_to_module
                        .get(&id)
                        .and_then(|mid| registry.modules.get(mid))
                        .map(|m| m.roles.clone())
                });
            }
            // `reanalyze_all` cleared the module names W5 propagated into the closure
            // slots (a `require(id)` store rebuilds as Unknown -> closure_N). Re-inject
            // them so a slot holding `require(id)` is named after the module again,
            // otherwise the import binding regresses to `closure_N`.
            crate::analysis::metro::propagate_module_names_to_closures(all_ir, registry, closure_ctx);
            if let Some(ctx) = closure_ctx.as_mut() {
                ctx.enrich_function_slot_names();
                // reanalyze already done above; only run the resolve loop now.
                Self::resolve_all_closures(all_ir, ctx, false, |_| {});
            }
        }
        log::debug!("[pipeline] closure resolution: {:.2?}", t.elapsed());

        // STAGE W7: Metro Export Analysis
        let t = std::time::Instant::now();
        let mut export_mod_ids: Vec<_> = registry.modules.keys().copied().collect();
        export_mod_ids.sort();
        for mid in export_mod_ids {
            if let Some(module) = registry.modules.get_mut(&mid) {
                crate::analysis::metro::exports::ExportAnalyzer::analyze(module, all_ir);
            }
        }
        log::debug!("[pipeline] metro export analysis: {:.2?}", t.elapsed());

        // STAGE W7b: Name still-unnamed modules from a single clear export, so imports
        // read `from "foo"` instead of `from "module_1234"`. Export names are stable
        // across builds (unlike the numeric ids), so this also cuts build-to-build diff
        // churn. Conservative: only when exactly one meaningful non-default export
        // exists, to keep the chosen name unambiguous and stable.
        {
            let unnamed: Vec<u32> = registry
                .modules
                .iter()
                .filter(|(_, m)| m.name.is_none())
                .map(|(id, _)| *id)
                .collect();
            let mut named_any = false;
            for id in unnamed {
                let inferred = registry
                    .modules
                    .get(&id)
                    .and_then(|m| name_from_single_export(&m.exports));
                if let Some(name) = inferred {
                    if let Some(m) = registry.modules.get_mut(&id) {
                        m.name = Some(name);
                        named_any = true;
                    }
                }
            }
            // Refresh closure slots so a `require(id)` capture picks up the new name
            // before the W9 resolve pass writes slot names into the IR.
            if named_any {
                crate::analysis::metro::propagate_module_names_to_closures(all_ir, registry, closure_ctx);
            }
        }

        // STAGE W8: Inter-Procedural Analysis (IPA)
        let t = std::time::Instant::now();
        let mut func_name_index = build_function_name_index(file);
        // Deep mode: a function that is anonymous in the name table but exported
        // under a real name (`exports.loginWithToken = <anon fn>`) is resolvable
        // through that ground-truth key. Feed the export keys into the callee
        // index so `obj.loginWithToken(...)` at an unresolved call site resolves
        // to the function, which then feeds its parameter names through IPA.
        if deep {
            let n = augment_name_index_with_exports(&mut func_name_index, registry);
            if n > 0 {
                log::debug!("[pipeline] name index augmented with {n} export keys");
            }
        }
        let mut global_analysis = crate::analysis::run_ipa(all_ir, registry, &func_name_index);
        log::debug!("[pipeline] IPA: {:.2?}", t.elapsed());

        // STAGE W9: IPA Closure Re-resolve
        let t = std::time::Instant::now();
        if let Some(ctx) = closure_ctx.as_mut() {
            // Second resolve: do NOT reanalyze (would wipe env stores already turned
            // into Variables). Only refresh names on existing slot maps + resolve
            // any residual ClosureVar.
            Self::resolve_all_closures(all_ir, ctx, false, |ctx| {
                ctx.update_with_ipa_names(&global_analysis.param_names);
                ctx.apply_metro_factory_param_roles(|id| {
                    registry
                        .function_to_module
                        .get(&id)
                        .and_then(|mid| registry.modules.get(mid))
                        .map(|m| m.roles.clone())
                });
            });
        }
        log::debug!("[pipeline] IPA closure re-resolve: {:.2?}", t.elapsed());

        // STAGE W10: Closure Property Naming (cross-function)
        let t = std::time::Instant::now();
        let closure_renames = if let Some(ctx) = closure_ctx.as_ref() {
            transforms::rename_closure_variables_cross_function(all_ir, ctx)
        } else {
            let mut count = 0;
            let mut fb_keys: Vec<_> = all_ir.keys().copied().collect();
            fb_keys.sort();
            for key in fb_keys {
                if let Some(stmts) = all_ir.get_mut(&key) {
                    count += transforms::rename_closure_variables(stmts);
                }
            }
            count
        };
        log::debug!("[pipeline] closure property naming: {:.2?} ({closure_renames} variables renamed)", t.elapsed());

        // STAGE W11: Definition-site closure naming
        let def_renames = transforms::rename_closures_from_definitions(all_ir);
        if def_renames > 0 {
            log::debug!("[pipeline] closure definition naming: {def_renames} variables renamed");
        }

        // STAGE W12: dependencyMap[N] → absolute module IDs.
        // After resolve_closures AND closure naming: heavily-indexed captures are
        // renamed to `dependencyMap` / `dependencyMap2` only in W10, so this must
        // run last among the naming stages.
        let t = std::time::Instant::now();
        crate::analysis::metro::rewrite_dependency_maps_late(all_ir, registry, closure_ctx);
        log::debug!("[pipeline] dependencyMap rewrite (post-naming): {:.2?}", t.elapsed());

        // STAGE W13: Inherit ancestor slot names for baked `closure_{level}_{slot}`
        // captures. resolve_closures froze these when the ancestor slot was still
        // Unknown; by now the ancestor slots are named (module names, stable captures),
        // so a descendant capture inside a `.then()`/`.catch()` callback inherits the
        // real name instead of staying `closure_1_9` / `closure_1_5`.
        let t = std::time::Instant::now();
        if let Some(ctx) = closure_ctx.as_ref() {
            let inherited = transforms::inherit_ancestor_closure_names(all_ir, ctx);
            log::debug!("[pipeline] ancestor closure inherit: {inherited} references renamed ({:.2?})", t.elapsed());
        }

        // STAGE W14 (deep mode only): converge naming to a fixed point. The closure and
        // ancestor renames above improved the names inside `all_ir`, so a second IPA over
        // the now-better-named bodies reads hints that were generic on the first pass (a
        // call argument that was `r5` or `closure_1_9` may now be `email`). Re-run IPA and
        // closure naming, merging any newly recovered parameter names, until a pass adds
        // nothing. Bounded by MAX_DEEP_NAMING_ITERATIONS.
        if deep {
            let t = std::time::Instant::now();

            // Alias elimination BEFORE the loop so IPA reads `require.email` /
            // `mixpanel.logEvents(...)` instead of `tmp.email`, giving real hints
            // (aliases that only get collapsed at the end never feed IPA).
            let fids: Vec<u32> = all_ir.keys().copied().collect();
            for fid in &fids {
                if let Some(slot) = all_ir.get_mut(fid) {
                    let stmts = std::mem::take(slot);
                    *slot = transforms::eliminate_immutable_aliases(stmts);
                }
            }

            for _ in 0..MAX_DEEP_NAMING_ITERATIONS {
                let pass = crate::analysis::run_ipa(all_ir, registry, &func_name_index);
                let added =
                    merge_param_names(&mut global_analysis.param_names, pass.param_names);

                // The missing feedback loop: parameter names live only in
                // `global_analysis.param_names` and were baked into the IR at render
                // time, so a re-run of IPA still saw `arg0` at every call site and
                // learned nothing new. Bake them into the bodies now, so the next IPA
                // reads `f(email)` and names the callee's parameter from it. Baking is
                // idempotent (an already-renamed parameter is a Variable, not a
                // Parameter, so a re-bake is a no-op).
                let pnames = global_analysis.param_names.clone();
                for (fid, names) in &pnames {
                    if let Some(stmts) = all_ir.get_mut(fid) {
                        transforms::exports::rename_param_registers(stmts, names);
                    }
                }

                // Propagate the newly recovered parameter names into the closure
                // slots so the next IPA pass reads them at capture sites. No re-bake
                // of closures (reanalyze stays off).
                if let Some(ctx) = closure_ctx.as_mut() {
                    Self::resolve_all_closures(all_ir, ctx, false, |ctx| {
                        ctx.update_with_ipa_names(&pnames);
                    });
                }
                let mut renamed = 0;
                if let Some(ctx) = closure_ctx.as_ref() {
                    renamed += transforms::rename_closure_variables_cross_function(all_ir, ctx);
                    // A slot that was Unknown / argN becomes inheritable once IPA has
                    // named it, so re-run the inherit pass each round (Piste 2).
                    renamed += transforms::inherit_ancestor_closure_names(all_ir, ctx);
                }
                if added == 0 && renamed == 0 {
                    break;
                }
            }
            log::debug!("[pipeline] deep naming convergence: {:.2?}", t.elapsed());
        }

        // STAGE W15 (stable mode): give every still-unnamed module a name derived
        // from a hash of its stable content (string constants, property keys,
        // function names) instead of the volatile Metro id, so the same module keeps
        // the same name across builds. Rename the baked `module_{id}` body variables
        // and set the registry name so the import specifier matches.
        if stable {
            let n = stabilize_unnamed_module_names(all_ir, registry, file);
            if n > 0 {
                log::debug!("[pipeline] stable module names: {n} modules hashed");
            }
        }

        global_analysis
    }
}

// Add every Metro export key to the callee-name index so an anonymous function
// exported under a real name becomes resolvable at unresolved call sites. Only
// commits a key that resolves uniquely (the index's own uniqueness gate rejects
// ambiguous ones), and skips `default` (never a distinctive method name) and
// non-identifier keys. Returns how many (key, fid) pairs were newly added.
fn augment_name_index_with_exports(
    index: &mut crate::analysis::FunctionNameIndex,
    registry: &crate::analysis::MetroRegistry,
) -> usize {
    // Deterministic order so the same bundle produces the same index.
    let mut mod_ids: Vec<u32> = registry.modules.keys().copied().collect();
    mod_ids.sort();
    let mut added = 0;
    for mid in mod_ids {
        let module = &registry.modules[&mid];
        let mut keys: Vec<&String> = module.exports.keys().collect();
        keys.sort();
        for key in keys {
            if key == "default" || !is_export_identifier(key) {
                continue;
            }
            let fid = module.exports[key];
            let ids = index.entry(key.clone()).or_default();
            if !ids.contains(&fid) {
                ids.push(fid);
                added += 1;
            }
        }
    }
    added
}

// A conservative JS identifier check for export keys used as callee names: starts
// with a letter, `_` or `$`, rest are identifier chars. Rejects computed / numeric
// keys that could never be a `.method` access.
fn is_export_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

// Replace the volatile `module_{id}` name of every unnamed module with
// `module_{content_hash}`, in both the registry (for the import specifier) and the
// baked body variables. Returns how many modules were renamed.
fn stabilize_unnamed_module_names(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &mut crate::analysis::MetroRegistry,
    file: &BytecodeFile,
) -> usize {
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    let mut ids: Vec<u32> = registry.modules.keys().copied().collect();
    ids.sort();
    for id in ids {
        let module = &registry.modules[&id];
        if module.name.is_some() {
            continue;
        }
        let dep_count = module.dependencies.len();
        let Some(stmts) = all_ir.get(&module.function_id) else { continue };
        let hash = module_content_hash(stmts, dep_count, file);
        let new_name = format!("module_{hash:08x}");
        renames.insert(format!("module_{id}"), new_name.clone());
        if let Some(m) = registry.modules.get_mut(&id) {
            m.name = Some(new_name);
        }
    }
    if renames.is_empty() {
        return 0;
    }
    let fids: Vec<u32> = all_ir.keys().copied().collect();
    for fid in fids {
        if let Some(stmts) = all_ir.get_mut(&fid) {
            crate::analysis::naming::rename_variables_in_stmts(stmts, &renames);
        }
    }
    renames.len()
}

// A stable fingerprint of a module from tokens that do not depend on volatile ids,
// register allocation, or offsets: string constants, member property keys, and the
// names of the functions it defines, plus its dependency count.
fn module_content_hash(stmts: &[Statement], dep_count: usize, file: &BytecodeFile) -> u32 {
    use crate::ir::{Constant, Expression, PropertyKey, Value, Visitor};
    struct C<'a> {
        file: &'a BytecodeFile,
        out: &'a mut Vec<String>,
    }
    impl<'b> Visitor<'b> for C<'_> {
        fn visit_expression(&mut self, e: &'b Expression) {
            match e {
                Expression::Value(Value::Constant(Constant::String(s))) if s.len() <= 60 => {
                    self.out.push(format!("s:{s}"));
                }
                Expression::Function { id, .. } => {
                    if let Some(entry) = self
                        .file
                        .function_headers
                        .get(id.0 as usize)
                        .and_then(|h| self.file.string_at(h.function_name()))
                    {
                        if !entry.value.is_empty() {
                            self.out.push(format!("f:{}", entry.value));
                        }
                    }
                }
                Expression::Member {
                    property: PropertyKey::Ident(p) | PropertyKey::String(p),
                    ..
                } => {
                    self.out.push(format!("p:{p}"));
                }
                _ => {}
            }
            self.walk_expression(e);
        }
    }
    let mut tokens = Vec::new();
    let mut c = C { file, out: &mut tokens };
    for s in stmts {
        c.visit_statement(s);
    }
    tokens.sort();
    tokens.dedup();

    fn mix(h: &mut u32, bytes: &[u8]) {
        for &b in bytes {
            *h ^= b as u32;
            *h = h.wrapping_mul(0x0100_0193);
        }
    }
    let mut h: u32 = 0x811c_9dc5;
    for t in &tokens {
        mix(&mut h, t.as_bytes());
        mix(&mut h, &[0]);
    }
    mix(&mut h, &(dep_count as u32).to_le_bytes());
    mix(&mut h, &(tokens.len() as u32).to_le_bytes());
    h
}

// Bounded fixed point for the deep-mode naming convergence loop. Convergence is
// typically 2-3 passes; the cap is a backstop.
const MAX_DEEP_NAMING_ITERATIONS: usize = 6;

// Fill empty parameter-name slots in `dst` from `src` (a later IPA pass), never
// overwriting a name already found. Returns how many new names were filled.
fn merge_param_names(
    dst: &mut BTreeMap<u32, Vec<Option<String>>>,
    src: BTreeMap<u32, Vec<Option<String>>>,
) -> usize {
    let mut added = 0;
    for (fid, names) in src {
        let entry = dst.entry(fid).or_default();
        if entry.len() < names.len() {
            entry.resize(names.len(), None);
        }
        for (i, name) in names.into_iter().enumerate() {
            if let Some(name) = name {
                if entry[i].is_none() {
                    entry[i] = Some(name);
                    added += 1;
                }
            }
        }
    }
    added
}

// Name unnamed Metro modules from the source file encoded in their function names.
// For each function whose name carries the Hermes source encoding, walk up the
// closure parent chain to the module factory and, if that module has no name yet,
// name it after the source file. Returns how many modules were named.
fn name_modules_from_source_files(
    file: &BytecodeFile,
    registry: &mut crate::analysis::MetroRegistry,
    closure_ctx: Option<&crate::analysis::ClosureContext>,
) -> usize {
    let Some(ctx) = closure_ctx else { return 0 };
    let count = file.function_headers.len() as u32;
    let mut named = 0;
    for fid in 0..count {
        let raw = file
            .function_headers
            .get(fid as usize)
            .and_then(|h| file.string_at(h.function_name()))
            .map(|e| e.value.clone());
        let Some(name) = raw else { continue };
        let Some(src_file) = source_file_from_fn_name(&name) else { continue };

        // Walk to the enclosing module factory.
        let mut cur = fid;
        let mut mod_id = None;
        for _ in 0..32 {
            if let Some(&m) = registry.function_to_module.get(&cur) {
                mod_id = Some(m);
                break;
            }
            match ctx.parent_function.get(&cur) {
                Some(&p) if p != cur => cur = p,
                _ => break,
            }
        }
        if let Some(mid) = mod_id {
            if let Some(module) = registry.modules.get_mut(&mid) {
                if module.name.is_none() {
                    module.name = Some(src_file);
                    named += 1;
                }
            }
        }
    }
    named
}

// Extract the source file from a Hermes function name of the form
// `<fn>_<package>_<file>Ts<N>` or `<file>Tsx<N>`: strip the trailing `Ts<digits>` /
// `Tsx<digits>` source marker, then take the last underscore segment (the file).
fn source_file_from_fn_name(name: &str) -> Option<String> {
    let no_digits = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if no_digits.len() == name.len() {
        return None; // no trailing dedup index, so not a source-encoded name
    }
    let base = no_digits
        .strip_suffix("Tsx")
        .or_else(|| no_digits.strip_suffix("Ts"))?;
    let src_file = base.rsplit('_').next()?;
    if src_file.len() >= 3
        && crate::util::is_valid_identifier(src_file)
        && !crate::analysis::metro::is_obviously_generic(src_file)
    {
        Some(src_file.to_string())
    } else {
        None
    }
}

// A module name inferred from its exports: only when exactly one meaningful
// non-default export exists, so the chosen name is unambiguous and stable across
// builds. Generic short names are rejected.
fn name_from_single_export(exports: &std::collections::HashMap<String, u32>) -> Option<String> {
    let mut names: Vec<&String> = exports
        .keys()
        // Reject generic export names (`value`, `config`, `index`, role names, ...)
        // via the shared metro filter so a module is never named after a
        // meaningless export.
        .filter(|k| {
            crate::util::is_valid_identifier(k)
                && k.len() >= 3
                && !crate::analysis::metro::is_obviously_generic(k)
        })
        .collect();
    names.sort();
    names.dedup();
    if names.len() == 1 {
        Some(names[0].clone())
    } else {
        None
    }
}
