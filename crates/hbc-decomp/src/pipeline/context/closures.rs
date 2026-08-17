// Closure re-analysis + IR resolve for all functions.
use std::collections::BTreeMap;
use crate::analysis::ClosureContext;
use crate::ir::Statement;
use super::PipelineContext;

impl PipelineContext {
    /// Resolve `ClosureVar` nodes using the closure context slot maps.
    ///
    /// `reanalyze`: when true, rebuild slot maps + parent edges from the current
    /// IR (must still contain `AssignTarget::ClosureVar` / raw env stores). Use
    /// this on the **first** resolve pass only. A second pass after variables are
    /// already renamed must set `reanalyze: false` — re-scanning then would drop
    /// env-slot stores (they became plain `Variable` names) and wipe parent maps.
    pub(super) fn resolve_all_closures(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        closure_ctx: &mut ClosureContext,
        reanalyze: bool,
        prepare: impl FnOnce(&mut ClosureContext),
    ) {
        if reanalyze {
            closure_ctx.reanalyze_all(all_ir);
        }
        prepare(closure_ctx);
        if reanalyze {
            // Metro/IPA may have renamed factory slots after reanalyze.
            closure_ctx.enrich_function_slot_names();
        }

        let mut keys: Vec<_> = all_ir.keys().copied().collect();
        keys.sort();
        for i in keys {
            let closure_info = closure_ctx.get_closure_info_for(i);
            let Some(stmts) = all_ir.get_mut(&i) else {
                continue;
            };
            let needs = !closure_info.slots.is_empty() || Self::body_has_closure_var(stmts);
            if needs {
                let old = std::mem::take(stmts);
                *stmts = crate::analysis::resolve_closures(old, &closure_info);
            }
        }
    }

    pub(super) fn body_has_closure_var(stmts: &[Statement]) -> bool {
        use crate::ir::{AssignTarget, Visitor};
        struct HasClosure(bool);
        impl Visitor<'_> for HasClosure {
            fn visit_expression(&mut self, e: &crate::ir::Expression) {
                if matches!(
                    e,
                    crate::ir::Expression::Value(crate::ir::Value::ClosureVar { .. })
                ) {
                    self.0 = true;
                    return;
                }
                if !self.0 {
                    self.walk_expression(e);
                }
            }
            fn visit_assign_target(&mut self, t: &AssignTarget) {
                if matches!(t, AssignTarget::ClosureVar { .. }) {
                    self.0 = true;
                    return;
                }
                if !self.0 {
                    self.walk_assign_target(t);
                }
            }
        }
        let mut v = HasClosure(false);
        for s in stmts {
            v.visit_statement(s);
            if v.0 {
                return true;
            }
        }
        false
    }

}
