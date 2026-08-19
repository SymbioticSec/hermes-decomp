// Resolve baked `closure_{level}_{slot}` ancestor captures by inheriting the
// resolved name of the ancestor's slot.
//
// `resolve_closures` bakes a raw `closure_{level}_{slot}` variable when the ancestor
// slot was still `Unknown` at resolution time (the ancestor is named later by module
// propagation / semantic naming). By the end of the naming pipeline the ancestor slot
// often has a real name (a module name, a stable capture), so a descendant capture can
// inherit it. This turns noise like `closure_1_9.default.logEvents(closure_1_5.EVENT)`
// into `logger.default.logEvents(constants.EVENT)`.
//
// Accuracy guard: a name is inherited only when it is unambiguous. If two distinct
// ancestor slots resolve to the same name in one function, or the target name is
// already a live local there, the capture is left as `closure_{level}_{slot}` rather
// than aliased. An honest generic name is preferred over two different values sharing
// one identifier.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::analysis::{ClosureContext, ClosureInfo};
use crate::analysis::naming::rename_variables_in_stmts;
use crate::ir::{Expression, Statement, Value, Visitor};

pub fn inherit_ancestor_closure_names(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &ClosureContext,
) -> usize {
    let fids: Vec<u32> = all_ir.keys().copied().collect();
    // `get_closure_info_for` rebuilds and clones the whole ancestor-slot map, so
    // memoize it: a given ancestor id is resolved once and reused across every
    // descendant capture in every function.
    let mut info_cache: HashMap<u32, ClosureInfo> = HashMap::new();
    let mut total = 0;

    // Reused across functions (cleared each iteration) to avoid per-function allocation.
    let mut all_names: HashSet<String> = HashSet::new();
    let mut closure_names: HashSet<String> = HashSet::new();

    for fid in fids {
        all_names.clear();
        closure_names.clear();
        if let Some(stmts) = all_ir.get(&fid) {
            collect_names(stmts, &mut all_names, &mut closure_names);
        }
        if closure_names.is_empty() {
            continue;
        }

        // Resolve each capture to a candidate name, and count how many distinct
        // captures land on each target so ambiguous targets can be dropped.
        let mut candidates: Vec<(String, String)> = Vec::new();
        let mut target_counts: HashMap<String, u32> = HashMap::new();
        for name in &closure_names {
            let Some((level, slot)) = parse_level_slot(name) else { continue };
            if level == 0 {
                continue;
            }
            let Some(ancestor) = closure_ctx.ancestor_at(fid, level) else { continue };
            let info = info_cache
                .entry(ancestor)
                .or_insert_with(|| closure_ctx.get_closure_info_for(ancestor));
            let ancestor_name = info.get_slot_name(slot);
            if is_inheritable(&ancestor_name) && ancestor_name != *name {
                *target_counts.entry(ancestor_name.clone()).or_insert(0) += 1;
                candidates.push((name.clone(), ancestor_name));
            }
        }

        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        for (old, target) in candidates {
            // Ambiguous: two different captures want the same name in this scope.
            if target_counts.get(&target).copied().unwrap_or(0) > 1 {
                continue;
            }
            // Collision: the name is already a live variable in this function
            // (a real local, or another capture). Do not shadow it.
            if all_names.contains(&target) {
                continue;
            }
            renames.insert(old, target);
        }

        if !renames.is_empty() {
            if let Some(stmts) = all_ir.get_mut(&fid) {
                rename_variables_in_stmts(stmts, &renames);
                total += renames.len();
            }
        }
    }
    total
}

// Parse the baked two-number form `closure_{level}_{slot}` into (level, slot).
// Single-number `closure_{slot}` is a local slot (handled elsewhere) and returns None.
fn parse_level_slot(name: &str) -> Option<(u32, u32)> {
    let rest = name.strip_prefix("closure_")?;
    let (level, slot) = rest.split_once('_')?;
    let l: u32 = level.parse().ok()?;
    let s: u32 = slot.parse().ok()?;
    Some((l, s))
}

// A slot name worth inheriting: not one of the generic fallbacks
// (`closure_N`, `outerN`, `argN`, `rN`, `tmp*`, `cN`, `reN`, `fN`).
fn is_inheritable(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    if name.starts_with("closure_") || name.starts_with("outer") {
        return false;
    }
    if name.starts_with("tmp") {
        return false;
    }
    // prefix + all-digits fallbacks: argN, rN, cN, fN, reN
    for pre in ["arg", "re", "r", "c", "f"] {
        if let Some(rest) = name.strip_prefix(pre) {
            if !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit()) {
                return false;
            }
        }
    }
    true
}

// Collect every referenced variable name (for collision detection) and the subset
// that are `closure_*` captures (the rename candidates).
fn collect_names(stmts: &[Statement], all: &mut HashSet<String>, closures: &mut HashSet<String>) {
    struct C<'a>(&'a mut HashSet<String>, &'a mut HashSet<String>);
    impl<'b> Visitor<'b> for C<'_> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                self.0.insert(n.clone());
                if n.starts_with("closure_") {
                    self.1.insert(n.clone());
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(all, closures);
    for s in stmts {
        c.visit_statement(s);
    }
}
