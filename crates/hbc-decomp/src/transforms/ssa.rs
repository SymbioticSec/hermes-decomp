use crate::analysis::reaching::{DefSite, ReachingDefs};
use crate::ir::{AssignTarget, Expression, MutVisitor, Statement, Terminator, Value, Visitor, CFG};
use std::collections::{BTreeMap, HashMap, HashSet};

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
    split_live_ranges(cfg);
}

// Live Range Splitting via Reaching Definitions + Union-Find.
//
// Each register definition is a node. Two definitions are *unioned* when some
// use reads both of them, i.e. they reconverge at that use and therefore must
// share one name (the no-phi equivalent of a φ-node, whether the convergence is
// an if/else merge or a loop back-edge). Every union-find class then becomes a
// distinct SSA variable: independent live ranges of a reused register split
// apart, while merge-/loop-carried ranges stay unified.
//
// This replaces the earlier "freeze the whole register" heuristic, which
// over-froze, collapsing a register's *independent* live ranges (e.g. one that
// held `globalThis` and was later reused for a string, or an object built via
// PutOwnBySlotIdx whose register was reused) into a single name. The HBC >=97
// allocator reuses registers far more aggressively, so precise splitting matters.
fn split_live_ranges(cfg: &mut CFG) {
    let rd = ReachingDefs::analyze(cfg);

    // Enumerate every register definition site, in CFG order (deterministic).
    let mut def_list: Vec<DefSite> = Vec::new();
    let mut def_id: HashMap<DefSite, usize> = HashMap::new();
    for block in cfg.blocks() {
        for (i, stmt) in block.statements.iter().enumerate() {
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = stmt
            {
                let site = DefSite {
                    block: block.id,
                    stmt_index: i,
                    register: *r,
                };
                def_id.insert(site, def_list.len());
                def_list.push(site);
            }
        }
    }
    if def_list.is_empty() {
        return;
    }
    let mut uf = UnionFind::new(def_list.len());

    // Pass 1, union the reaching definitions of every use.
    for block in cfg.blocks() {
        let mut cur = block_entry_reaching(&rd, block.id);
        for (i, stmt) in block.statements.iter().enumerate() {
            for r in stmt_reads(stmt) {
                union_reaching(&mut uf, &def_id, cur.get(&r));
            }
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                ..
            } = stmt
            {
                cur.insert(
                    *r,
                    vec![DefSite {
                        block: block.id,
                        stmt_index: i,
                        register: *r,
                    }],
                );
            }
        }
        for r in terminator_reads(&block.terminator) {
            union_reaching(&mut uf, &def_id, cur.get(&r));
        }
    }

    // Assign a fresh register number per union-find class (deterministic order).
    // Numbers start high to avoid colliding with physical registers / parameters.
    let mut class_reg: HashMap<usize, u32> = HashMap::new();
    let mut next: u32 = 10000;
    for id in 0..def_list.len() {
        let root = uf.find(id);
        class_reg.entry(root).or_insert_with(|| {
            let v = next;
            next += 1;
            v
        });
    }
    let version_of = |uf: &mut UnionFind, site: &DefSite| -> Option<u32> {
        def_id.get(site).map(|&id| {
            let root = uf.find(id);
            class_reg[&root]
        })
    };

    // Pass 2, rewrite uses and definitions to their class's register number.
    for block_id in cfg.block_ids().collect::<Vec<_>>() {
        let mut cur = block_entry_reaching(&rd, block_id);
        let stmts = match cfg.get_mut(block_id) {
            Some(b) => std::mem::take(&mut b.statements),
            None => continue,
        };
        let mut new_stmts = Vec::with_capacity(stmts.len());
        for (i, mut stmt) in stmts.into_iter().enumerate() {
            // A read of register r resolves to the version of the class its
            // current reaching defs belong to (they were all unioned in pass 1).
            let read_map: BTreeMap<u32, u32> = cur
                .iter()
                .filter_map(|(r, defs)| {
                    let first = defs.first()?;
                    version_of(&mut uf, first).map(|v| (*r, v))
                })
                .collect();

            let def_orig = match &stmt {
                Statement::Assign {
                    target: AssignTarget::Register(r),
                    ..
                } => Some(*r),
                _ => None,
            };

            rewrite_reads_in_stmt(&mut stmt, &read_map);

            if let Some(r) = def_orig {
                let site = DefSite {
                    block: block_id,
                    stmt_index: i,
                    register: r,
                };
                if let Some(v) = version_of(&mut uf, &site) {
                    if let Statement::Assign {
                        target: AssignTarget::Register(t),
                        ..
                    } = &mut stmt
                    {
                        *t = v;
                    }
                }
                cur.insert(r, vec![site]);
            }
            new_stmts.push(stmt);
        }

        let read_map: BTreeMap<u32, u32> = cur
            .iter()
            .filter_map(|(r, defs)| {
                let first = defs.first()?;
                version_of(&mut uf, first).map(|v| (*r, v))
            })
            .collect();
        if let Some(block) = cfg.get_mut(block_id) {
            block.statements = new_stmts;
            rewrite_reads_in_terminator(&mut block.terminator, &read_map);
        }
    }
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

// Reaching definitions on entry to a block, grouped by register.
fn block_entry_reaching(rd: &ReachingDefs, block: crate::ir::BlockId) -> HashMap<u32, Vec<DefSite>> {
    let mut cur: HashMap<u32, Vec<DefSite>> = HashMap::new();
    if let Some(in_set) = rd.reaching_in.get(&block) {
        for d in in_set {
            cur.entry(d.register).or_default().push(*d);
        }
    }
    cur
}

// Union together all definitions that reach a single use.
fn union_reaching(uf: &mut UnionFind, def_id: &HashMap<DefSite, usize>, defs: Option<&Vec<DefSite>>) {
    if let Some(defs) = defs {
        let ids: Vec<usize> = defs.iter().filter_map(|d| def_id.get(d).copied()).collect();
        for w in ids.windows(2) {
            uf.union(w[0], w[1]);
        }
    }
}

// Registers READ by a statement (value expressions + read sub-expressions of an
// assignment target, Member object, Index object/key, but NOT the target
// register itself, which is a definition).
fn stmt_reads(stmt: &Statement) -> HashSet<u32> {
    let mut regs = HashSet::new();
    match stmt {
        Statement::Assign { target, value } => {
            collect_reg_reads(value, &mut regs);
            match target {
                AssignTarget::Member { object, .. } => collect_reg_reads(object, &mut regs),
                AssignTarget::Index { object, key } => {
                    collect_reg_reads(object, &mut regs);
                    collect_reg_reads(key, &mut regs);
                }
                _ => {}
            }
        }
        Statement::Let { value, .. } => collect_reg_reads(value, &mut regs),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_reg_reads(e, &mut regs)
        }
        _ => {}
    }
    regs
}

fn terminator_reads(term: &Terminator) -> HashSet<u32> {
    let mut regs = HashSet::new();
    match term {
        Terminator::Return(Some(e)) | Terminator::Throw(e) => collect_reg_reads(e, &mut regs),
        Terminator::Branch { condition, .. } => collect_reg_reads(condition, &mut regs),
        Terminator::Switch { value, .. } => collect_reg_reads(value, &mut regs),
        _ => {}
    }
    regs
}

fn collect_reg_reads(expr: &Expression, out: &mut HashSet<u32>) {
    struct C<'a>(&'a mut HashSet<u32>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Register(r)) = e {
                self.0.insert(*r);
            }
            self.walk_expression(e);
        }
    }
    C(out).visit_expression(expr);
}

// Rewrite register reads (NOT the assignment-target register) using `map`.
fn rewrite_reads_in_stmt(stmt: &mut Statement, map: &BTreeMap<u32, u32>) {
    let mut rw = ReadRewriter(map);
    match stmt {
        Statement::Assign { target, value } => {
            rw.visit_expression(value);
            match target {
                AssignTarget::Member { object, .. } => rw.visit_expression(object),
                AssignTarget::Index { object, key } => {
                    rw.visit_expression(object);
                    rw.visit_expression(key);
                }
                _ => {}
            }
        }
        Statement::Let { value, .. } => rw.visit_expression(value),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            rw.visit_expression(e)
        }
        _ => {}
    }
}

fn rewrite_reads_in_terminator(term: &mut Terminator, map: &BTreeMap<u32, u32>) {
    let mut rw = ReadRewriter(map);
    match term {
        Terminator::Return(Some(e)) | Terminator::Throw(e) => rw.visit_expression(e),
        Terminator::Branch { condition, .. } => rw.visit_expression(condition),
        Terminator::Switch { value, .. } => rw.visit_expression(value),
        _ => {}
    }
}

struct ReadRewriter<'a>(&'a BTreeMap<u32, u32>);
impl MutVisitor for ReadRewriter<'_> {
    fn visit_expression(&mut self, expr: &mut Expression) {
        if let Expression::Value(Value::Register(r)) = expr {
            if let Some(&v) = self.0.get(r) {
                *r = v;
            }
            return;
        }
        self.walk_expression(expr);
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
