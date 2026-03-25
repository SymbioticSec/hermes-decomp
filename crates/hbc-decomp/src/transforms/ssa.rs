use crate::analysis::liveness::LivenessInfo;
use crate::ir::{AssignTarget, Expression, MutVisitor, Statement, Value, CFG};
use std::collections::BTreeMap;

// Transform the function to Static Single Assignment (SSA) form.
//
// Strictly speaking, this is **Live Range Splitting** rather than full SSA.
//
// **Why NOT full SSA with Phi nodes?**
//
// 1.  **Decompilation vs Compilation**:
//     *   **Compilers** love Phi nodes (`v3 = phi(v1, v2)`) because they make dataflow explicit and simplify optimizations (Constant Propagation, GVN).
//     *   **Decompilers** target *readability*. JavaScript (and most high-level languages) has no concept of Phi nodes. To emit valid JS, we would need a "De-SSA" pass (Out-of-SSA) to convert Phis back into control flow (`if` statements with copies).
//
// 2.  **The "De-SSA" Problem**:
//     *   Converting Phi nodes back to variables is hard. A naive approach introduces many temporary variables (`temp_1`, `temp_2`) and copy instructions (`dst = src`) along edges.
//     *   To get clean code, we would need advanced **Register Coalescing** to merge these temporaries back together.
//     *   *Our Approach*: By using Live Range Splitting without Phis, we get 90% of the benefit (separating independent uses of `r0`) without the complexity of "De-SSA".
//
// 3.  **Result Quality**:
//     *   **With Phis**: Potentially more powerful analysis, but risks generating "Spaghetti Code" or verbose variable copying if De-SSA is imperfect.
//     *   **Our Implementation**: We assume that if `r0` is re-assigned in a new block, it's likely a new variable. We trust the Control Flow Recovery phase to handle the branching logic naturally.
//
// **Algorithm:**
// 1. Analyze Liveness: Determine where each register is live.
// 2. Renaming Pass: Iterate instructions.
//    - On Use (Read): Replace register with its current "version".
//    - On Def (Write): Generate a NEW "version" (virtual register).
pub fn transform_to_ssa(cfg: &mut CFG) {
    // 1. Analyze liveness on the original CFG
    // We need to know where variables are live to decide if a new definition
    // should shadow the old one or if it's a completely new variable.
    let liveness = LivenessInfo::analyze(cfg);

    // 2. Compute new variable mappings
    let mut renamer = SSARenamer::new();
    renamer.run(cfg, &liveness);
}

struct SSARenamer {
    // Map (original_reg, version) -> new_virtual_reg_id
    // We use high numbers for virtual registers to avoid conflict with physical ones (0-255 usually)
    next_virtual_reg: u32,

    // Map physical reg -> current virtual reg
    current_mapping: BTreeMap<u32, u32>,
}

impl SSARenamer {
    fn new() -> Self {
        Self {
            next_virtual_reg: 10000, // Start high to avoid collision
            current_mapping: BTreeMap::new(),
        }
    }

    fn run(&mut self, cfg: &mut CFG, _liveness: &LivenessInfo) {
        // Simple 1-pass approach for straight-line code and basic blocks.
        // Every time we see an assignment to rX without a preceding use in the same live-range,
        // we create a new version.
        
        // Iterate blocks in roughly topological order (RPO) to propagate definitions
        let blocks = cfg.reverse_postorder();

        for block_id in blocks {
            if let Some(block) = cfg.get_mut(block_id) {
                // Use MutVisitor to rename within statements
                self.visit_statement_list(&mut block.statements);

                // Rewrite terminator manually since it's not a node in MutVisitor hierarchy yet.
                match &mut block.terminator {
                    crate::ir::Terminator::Return(Some(e)) | crate::ir::Terminator::Throw(e) => {
                        self.visit_expression(e);
                    }
                    crate::ir::Terminator::Branch { condition, .. } => self.visit_expression(condition),
                    crate::ir::Terminator::Switch { value, .. } => self.visit_expression(value),
                    _ => {}
                }
            }
        }
    }

    fn new_version(&mut self, reg: u32) -> u32 {
        let v = self.next_virtual_reg;
        self.next_virtual_reg += 1;
        self.current_mapping.insert(reg, v);
        v
    }
}

impl MutVisitor for SSARenamer {
    fn visit_statement(&mut self, stmt: &mut Statement) {
        match stmt {
            Statement::Assign { target, value } => {
                // Must visit VALUE (which uses current mapping) BEFORE Target (which defines new mapping)
                self.visit_expression(value);
                self.visit_assign_target(target);
            }
            _ => self.walk_statement(stmt),
        }
    }

    fn visit_expression(&mut self, expr: &mut Expression) {
        match expr {
            Expression::Value(Value::Register(r)) => {
                if let Some(&new_reg) = self.current_mapping.get(r) {
                    *r = new_reg;
                }
                // Leaves don't need walk_expression
            }
            Expression::Assignment { target, value } => {
                // Visit value first
                self.visit_expression(value);
                
                // Expr Assignment is tricky, usually target is use-def.
                if let Expression::Value(Value::Register(r)) = &mut **target {
                    // Defines a new version
                    *r = self.new_version(*r);
                } else {
                    self.visit_expression(target);
                }
            }
            _ => self.walk_expression(expr),
        }
    }

    fn visit_assign_target(&mut self, target: &mut AssignTarget) {
        if let AssignTarget::Register(r) = target {
            // Determine if this is a new definition.
            // In SSA, every assignment is a new definition.
            *r = self.new_version(*r);
        } else {
            self.walk_assign_target(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CFGBuilder, Constant, Expression, Statement, Value};

    #[test]
    fn test_ssa_splitting() {
        // r0 = 1
        // r1 = r0 + 1  (use r0_v0)
        // r0 = 2       (define r0_v1)
        // r2 = r0 + 1  (use r0_v1)

        let mut builder = CFGBuilder::new();
        let r0 = 0;
        let r1 = 1;
        let r2 = 2;

        // r0 = 1
        builder.emit(Statement::assign_reg(
            r0,
            Expression::constant(Constant::Integer(1)),
        ));
        // r1 = r0 + 1
        builder.emit(Statement::assign_reg(
            r1,
            Expression::binary(
                crate::ir::BinaryOp::Add,
                Expression::register(r0),
                Expression::constant(Constant::Integer(1)),
            ),
        ));
        // r0 = 2
        builder.emit(Statement::assign_reg(
            r0,
            Expression::constant(Constant::Integer(2)),
        ));
        // r2 = r0 + 1
        builder.emit(Statement::assign_reg(
            r2,
            Expression::binary(
                crate::ir::BinaryOp::Add,
                Expression::register(r0),
                Expression::constant(Constant::Integer(1)),
            ),
        ));

        builder.emit_return(None);

        let mut cfg = builder.finish();
        transform_to_ssa(&mut cfg);

        // Inspect the SSA form
        let entry = cfg.entry;
        let block = cfg.get(entry).unwrap();

        // Collect assignments to checking targets
        let mut assignments = Vec::new();
        for stmt in &block.statements {
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                value,
            } = stmt
            {
                assignments.push((*r, value.clone()));
            }
        }

        // Assignment 0: r0_v1 = 1
        let (def1, _) = &assignments[0];
        // Assignment 1: r1 = r0_v1 + 1
        let (_, val1) = &assignments[1];
        // Assignment 2: r0_v2 = 2
        let (def2, _) = &assignments[2];
        // Assignment 3: r2 = r0_v2 + 1
        let (_, val3) = &assignments[3];

        // Check that definitions define different registers
        assert_ne!(def1, def2, "r0 should be split into different versions");

        // Check that uses refer to correct versions
        if let Expression::Binary { left, .. } = val1 {
            if let Expression::Value(Value::Register(u)) = **left {
                assert_eq!(u, *def1, "First use should refer to first definition");
            } else {
                panic!("Expected register use")
            }
        }

        if let Expression::Binary { left, .. } = val3 {
            if let Expression::Value(Value::Register(u)) = **left {
                assert_eq!(u, *def2, "Second use should refer to second definition");
            } else {
                panic!("Expected register use")
            }
        }
    }
}
