// Declare a closure once where hermesc creates it at every use.
//
// The optimizer turns a function declaration that never escapes into a fresh
// `CreateClosure` at each call site, so the bytecode literally reads
// `(function getRawTag(v) { ... })(x)` seventy times across a module. Rendered
// as written, every site embeds the whole body and the text of a big module
// grows with the number of sites rather than with the source.
//
// A function created in several places, or created only to be called on the
// spot under its own name, gets one declaration in the nearest scope shared
// by the sites (the common closure parent when there is one) and a plain
// name everywhere else. Anonymous one-off closures, callbacks passed once,
// stay as they are.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{Binding, Expression, FunctionId, MutVisitor, Statement, Value, VarKind, Visitor};

// Where one function id shows up: the function holding the site, and whether
// the site is `(function ...)(...)`.
struct Site {
    holder: u32,
    iife: bool,
}

struct Shape {
    name: Option<String>,
    is_arrow: bool,
    is_async: bool,
    is_generator: bool,
}

pub fn hoist_repeated_closures(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    parent_of: &BTreeMap<u32, u32>,
    factories: &BTreeMap<u32, u32>,
) -> usize {
    let mut sites: BTreeMap<u32, Vec<Site>> = BTreeMap::new();
    let mut shapes: BTreeMap<u32, Shape> = BTreeMap::new();
    for (&holder, stmts) in all_ir.iter() {
        let mut c = Collect {
            holder,
            sites: &mut sites,
            shapes: &mut shapes,
        };
        for s in stmts {
            c.visit_statement(s);
        }
    }

    // Strongly connected components of the creation graph. A function inside
    // a cycle (it creates a closure of itself, or of a function that creates
    // one of it back) can never be embedded in place: the renderer would
    // recurse through the cycle without end.
    let scc = strongly_connected(&sites, all_ir);

    // The names each function reads or writes that it does not bind itself,
    // its nested functions included: what a hoisted copy would still need in
    // scope. A closure of `arrayPush` reading the holder's `index` and
    // `array` was declared once at the module level, out of their reach.
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (&child, &parent) in parent_of {
        if all_ir.contains_key(&child) && all_ir.contains_key(&parent) {
            children.entry(parent).or_default().push(child);
        }
    }
    let mut bound_of: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    let mut refs_of: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for (&fid, stmts) in all_ir.iter() {
        let mut bound = BTreeSet::new();
        let mut refs = BTreeSet::new();
        {
            let mut b = Bound(&mut bound);
            let mut r = Refs(&mut refs);
            for st in stmts {
                b.visit_statement(st);
                r.visit_statement(st);
            }
        }
        bound_of.insert(fid, bound);
        refs_of.insert(fid, refs);
    }
    let mut free_memo: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for &fid in sites.keys() {
        free_names(fid, &children, &refs_of, &bound_of, &mut free_memo, 0);
    }

    // function id -> (scopes that declare it, binding name)
    let mut plan: BTreeMap<u32, (Vec<u32>, String)> = BTreeMap::new();
    // scope -> names this pass already gave out there. Three different
    // helpers all named `T` must not all become `T2`.
    let mut planned: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for (&fid, found) in &sites {
        if factories.contains_key(&fid) {
            continue;
        }
        let shape = &shapes[&fid];
        let named_iife = found.iter().any(|s| s.iife) && shape.name.is_some();
        let cyclic = scc.cyclic.contains(&scc.component[&fid]);
        if found.len() < 2 && !named_iife && !cyclic {
            continue;
        }
        // The declaration goes outside the function itself and outside its
        // cycle; inside, the name is in scope through the declaring ancestor.
        let own = scc.component[&fid];
        let holders: BTreeSet<u32> = found
            .iter()
            .map(|s| s.holder)
            .filter(|h| *h != fid && scc.component.get(h) != Some(&own))
            .collect();
        let scopes: Vec<u32> = match common_scope(&holders, parent_of, all_ir) {
            Some(scope) if scc.component.get(&scope) != Some(&own) => vec![scope],
            // No shared ancestor is known: declare in every holder. That keeps
            // one copy per holder, which is finite, where a cycle is not.
            _ if !holders.is_empty() => holders.iter().copied().collect(),
            _ => {
                let parents: BTreeSet<u32> = scc
                    .members(own)
                    .filter_map(|m| parent_of.get(&m).copied())
                    .filter(|p| all_ir.contains_key(p) && scc.component.get(p) != Some(&own))
                    .collect();
                match common_scope(&parents, parent_of, all_ir) {
                    Some(scope) => vec![scope],
                    // Nothing outside the cycle refers to it: the names stay
                    // free and the members are rendered on their own.
                    None if cyclic => Vec::new(),
                    None => continue,
                }
            }
        };
        // A hoisted declaration must not leave behind a function whose
        // locals the closure captures. Between each holder and the scope
        // that declares it, no function on the way may bind a free name of
        // the closure. A cycle keeps its hoist regardless: embedding it never
        // ends, and a broken capture is the lesser evil there.
        if !cyclic {
            let free = free_memo.get(&fid).cloned().unwrap_or_default();
            let captures_on_the_way = found.iter().any(|site| {
                scopes.iter().any(|&scope| {
                    let mut cur = site.holder;
                    let mut hops = 0;
                    while cur != scope && hops < 64 {
                        if bound_of
                            .get(&cur)
                            .is_some_and(|b| b.iter().any(|n| free.contains(n)))
                        {
                            return true;
                        }
                        match parent_of.get(&cur) {
                            Some(&p) => cur = p,
                            None => break,
                        }
                        hops += 1;
                    }
                    false
                })
            });
            if captures_on_the_way {
                continue;
            }
        }
        let base = shape
            .name
            .as_deref()
            .filter(|n| crate::util::is_valid_identifier(n))
            .filter(|n| !crate::constants::is_reserved_word(n))
            .map(str::to_string)
            .unwrap_or_else(|| format!("f{fid}"));
        let mut check: BTreeSet<u32> = holders.clone();
        check.extend(scopes.iter().copied());
        check.insert(fid);
        let taken: BTreeSet<String> = scopes
            .iter()
            .filter_map(|s| planned.get(s))
            .flatten()
            .cloned()
            .collect();
        let name = free_name(base, &check, &taken, all_ir);
        for scope in &scopes {
            planned.entry(*scope).or_default().insert(name.clone());
        }
        log::debug!(
            target: "hoist",
            "{fid} as {name}: holders {:?} scopes {scopes:?} cyclic {cyclic}",
            holders
        );
        plan.insert(fid, (scopes, name));
    }
    if plan.is_empty() {
        return 0;
    }

    // Replace every site by the name.
    let by_holder: BTreeMap<u32, BTreeMap<u32, String>> = {
        let mut m: BTreeMap<u32, BTreeMap<u32, String>> = BTreeMap::new();
        for (&fid, found) in &sites {
            if let Some((_, name)) = plan.get(&fid) {
                for s in found {
                    m.entry(s.holder).or_default().insert(fid, name.clone());
                }
            }
        }
        m
    };
    for (holder, names) in &by_holder {
        if let Some(stmts) = all_ir.get_mut(holder) {
            let mut r = Replace { names };
            r.visit_statement_list(stmts);
        }
    }

    // Declare once, at the top of the shared scope, in a stable order.
    let mut decls: BTreeMap<u32, Vec<Statement>> = BTreeMap::new();
    for (&fid, (scopes, name)) in &plan {
        let shape = &shapes[&fid];
        for scope in scopes {
            decls.entry(*scope).or_default().push(Statement::Let {
                name: name.clone(),
                value: Expression::Function {
                    id: FunctionId(fid),
                    name: shape.name.clone(),
                    is_arrow: shape.is_arrow,
                    is_async: shape.is_async,
                    is_generator: shape.is_generator,
                },
                kind: VarKind::Const,
            });
        }
    }
    for (scope, mut new_decls) in decls {
        if let Some(stmts) = all_ir.get_mut(&scope) {
            let mut body = std::mem::take(stmts);
            new_decls.append(&mut body);
            *stmts = new_decls;
        }
    }
    plan.len()
}

struct Components {
    // function id -> component index
    component: BTreeMap<u32, usize>,
    // component index -> its members
    groups: Vec<Vec<u32>>,
    // components that contain a cycle: two or more members, or a self edge
    cyclic: BTreeSet<usize>,
}

impl Components {
    fn members(&self, c: usize) -> impl Iterator<Item = u32> + '_ {
        self.groups[c].iter().copied()
    }
}

// Tarjan's algorithm, iterative, over the graph whose edges go from a holder
// to every function it creates.
fn strongly_connected(
    sites: &BTreeMap<u32, Vec<Site>>,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
) -> Components {
    let mut edges: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut self_edge: BTreeSet<u32> = BTreeSet::new();
    for (&fid, found) in sites {
        for s in found {
            if s.holder == fid {
                self_edge.insert(fid);
            }
            edges.entry(s.holder).or_default().push(fid);
        }
    }
    let nodes: Vec<u32> = all_ir.keys().copied().collect();
    let mut index: BTreeMap<u32, usize> = BTreeMap::new();
    let mut low: BTreeMap<u32, usize> = BTreeMap::new();
    let mut on_stack: BTreeSet<u32> = BTreeSet::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut component: BTreeMap<u32, usize> = BTreeMap::new();
    let mut groups: Vec<Vec<u32>> = Vec::new();
    let mut next = 0usize;
    let empty: Vec<u32> = Vec::new();
    for &root in &nodes {
        if index.contains_key(&root) {
            continue;
        }
        // (node, position in its edge list)
        let mut work: Vec<(u32, usize)> = vec![(root, 0)];
        index.insert(root, next);
        low.insert(root, next);
        next += 1;
        stack.push(root);
        on_stack.insert(root);
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            let out = edges.get(&v).unwrap_or(&empty);
            if *i < out.len() {
                let w = out[*i];
                *i += 1;
                if let std::collections::btree_map::Entry::Vacant(slot) = index.entry(w) {
                    slot.insert(next);
                    low.insert(w, next);
                    next += 1;
                    stack.push(w);
                    on_stack.insert(w);
                    work.push((w, 0));
                } else if on_stack.contains(&w) {
                    let lw = index[&w];
                    let lv = low[&v];
                    low.insert(v, lv.min(lw));
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                let lv = low[&v];
                let lp = low[&parent];
                low.insert(parent, lp.min(lv));
            }
            if low[&v] == index[&v] {
                let c = groups.len();
                let mut members = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack.remove(&w);
                    component.insert(w, c);
                    members.push(w);
                    if w == v {
                        break;
                    }
                }
                groups.push(members);
            }
        }
    }
    let cyclic: BTreeSet<usize> = groups
        .iter()
        .enumerate()
        .filter(|(_, g)| g.len() > 1 || g.iter().any(|m| self_edge.contains(m)))
        .map(|(c, _)| c)
        .collect();
    Components {
        component,
        groups,
        cyclic,
    }
}

// The innermost function that contains every holder: a holder itself when
// there is one, else the closure parent they all share, walking up as far
// as the chain goes. A set with no common ancestor is left alone.
fn common_scope(
    holders: &BTreeSet<u32>,
    parent_of: &BTreeMap<u32, u32>,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
) -> Option<u32> {
    let mut it = holders.iter();
    let first = *it.next()?;
    if holders.len() == 1 && all_ir.contains_key(&first) {
        return Some(first);
    }
    let mut chain: Vec<u32> = vec![first];
    let mut cur = first;
    while let Some(&p) = parent_of.get(&cur) {
        if chain.contains(&p) || chain.len() > 64 {
            break;
        }
        chain.push(p);
        cur = p;
    }
    for &h in it {
        let mut anc: Vec<u32> = vec![h];
        let mut cur = h;
        while let Some(&p) = parent_of.get(&cur) {
            if anc.contains(&p) || anc.len() > 64 {
                break;
            }
            anc.push(p);
            cur = p;
        }
        chain.retain(|c| anc.contains(c));
        if chain.is_empty() {
            return None;
        }
    }
    chain.into_iter().find(|c| all_ir.contains_key(c))
}

// A name none of the given functions already binds. The function's own name
// is preferred; a clash takes a numeric suffix.
fn free_name(
    base: String,
    check: &BTreeSet<u32>,
    taken: &BTreeSet<String>,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
) -> String {
    let mut bound: BTreeSet<String> = taken.clone();
    for fid in check {
        if let Some(stmts) = all_ir.get(fid) {
            let mut b = Bound(&mut bound);
            for s in stmts {
                b.visit_statement(s);
            }
        }
    }
    if !bound.contains(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}{n}");
        if !bound.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

// The free names of `fid`: everything it or a function nested in it refers
// to, minus what it binds itself.
fn free_names(
    fid: u32,
    children: &BTreeMap<u32, Vec<u32>>,
    refs_of: &BTreeMap<u32, BTreeSet<String>>,
    bound_of: &BTreeMap<u32, BTreeSet<String>>,
    memo: &mut BTreeMap<u32, BTreeSet<String>>,
    depth: usize,
) -> BTreeSet<String> {
    if let Some(f) = memo.get(&fid) {
        return f.clone();
    }
    let mut free: BTreeSet<String> = refs_of.get(&fid).cloned().unwrap_or_default();
    if depth < 32 {
        if let Some(kids) = children.get(&fid) {
            for &kid in kids {
                free.extend(free_names(
                    kid,
                    children,
                    refs_of,
                    bound_of,
                    memo,
                    depth + 1,
                ));
            }
        }
    }
    if let Some(bound) = bound_of.get(&fid) {
        free.retain(|n| !bound.contains(n));
    }
    memo.insert(fid, free.clone());
    free
}

// Every variable name a body reads or writes.
struct Refs<'a>(&'a mut BTreeSet<String>);

impl<'b> Visitor<'b> for Refs<'_> {
    fn visit_assign_target(&mut self, t: &'b crate::ir::AssignTarget) {
        if let crate::ir::AssignTarget::Binding(Binding::Variable(n)) = t {
            self.0.insert(n.clone());
        }
        self.walk_assign_target(t);
    }
    fn visit_expression(&mut self, e: &'b Expression) {
        if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
            self.0.insert(n.clone());
        }
        self.walk_expression(e);
    }
}

struct Bound<'a>(&'a mut BTreeSet<String>);

impl<'b> Visitor<'b> for Bound<'_> {
    fn visit_binding_def(&mut self, name: &'b str) {
        self.0.insert(name.to_string());
    }
    fn visit_assign_target(&mut self, t: &'b crate::ir::AssignTarget) {
        if let crate::ir::AssignTarget::Binding(Binding::Variable(n)) = t {
            self.0.insert(n.clone());
        }
        self.walk_assign_target(t);
    }
    fn visit_expression(&mut self, e: &'b Expression) {
        // A function id is fine to see again; its body is separate IR.
        self.walk_expression(e);
    }
}

struct Collect<'a> {
    holder: u32,
    sites: &'a mut BTreeMap<u32, Vec<Site>>,
    shapes: &'a mut BTreeMap<u32, Shape>,
}

impl Collect<'_> {
    fn record(&mut self, e: &Expression, iife: bool) {
        if let Expression::Function {
            id,
            name,
            is_arrow,
            is_async,
            is_generator,
        } = e
        {
            self.sites.entry(id.0).or_default().push(Site {
                holder: self.holder,
                iife,
            });
            self.shapes.entry(id.0).or_insert_with(|| Shape {
                name: name.clone(),
                is_arrow: *is_arrow,
                is_async: *is_async,
                is_generator: *is_generator,
            });
        }
    }
}

impl<'b> Visitor<'b> for Collect<'_> {
    // A class body is its own declaration of every method it holds; the
    // constructor seen there is not a site to hoist, and a hoisted copy of it
    // would bind the class name a second time.
    fn visit_statement(&mut self, s: &'b Statement) {
        if !matches!(s, Statement::Class { .. }) {
            self.walk_statement(s);
        }
    }
    fn visit_expression(&mut self, e: &'b Expression) {
        match e {
            Expression::Call { callee, arguments }
                if matches!(callee.as_ref(), Expression::Function { .. }) =>
            {
                self.record(callee, true);
                for a in arguments {
                    self.visit_expression(a);
                }
            }
            Expression::Function { .. } => self.record(e, false),
            _ => self.walk_expression(e),
        }
    }
}

struct Replace<'a> {
    names: &'a BTreeMap<u32, String>,
}

impl MutVisitor for Replace<'_> {
    fn visit_statement(&mut self, s: &mut Statement) {
        if !matches!(s, Statement::Class { .. }) {
            self.walk_statement(s);
        }
    }
    fn visit_expression(&mut self, e: &mut Expression) {
        if let Expression::Function { id, .. } = e {
            if let Some(name) = self.names.get(&id.0) {
                *e = Expression::Value(Value::Binding(Binding::Variable(name.clone())));
                return;
            }
        }
        self.walk_expression(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    fn func(id: u32, name: &str) -> Expression {
        Expression::Function {
            id: FunctionId(id),
            name: Some(name.into()),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        }
    }

    fn iife(id: u32, name: &str) -> Statement {
        Statement::Expr(Expression::Call {
            callee: Box::new(func(id, name)),
            arguments: vec![Expression::constant(Constant::Integer(1))],
        })
    }

    #[test]
    fn sites_in_sibling_functions_declare_once_in_the_parent() {
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![Statement::Return(None)]);
        all_ir.insert(2, vec![iife(9, "getRawTag")]);
        all_ir.insert(3, vec![iife(9, "getRawTag")]);
        let parent_of: BTreeMap<u32, u32> = [(2, 1), (3, 1), (9, 2)].into_iter().collect();
        let n = hoist_repeated_closures(&mut all_ir, &parent_of, &BTreeMap::new());
        assert_eq!(n, 1);
        assert!(matches!(
            &all_ir[&1][0],
            Statement::Let { name, value: Expression::Function { id, .. }, .. }
                if name == "getRawTag" && id.0 == 9
        ));
        for holder in [2, 3] {
            let Statement::Expr(Expression::Call { callee, .. }) = &all_ir[&holder][0] else {
                panic!("call kept");
            };
            assert_eq!(
                **callee,
                Expression::Value(Value::Binding(Binding::Variable("getRawTag".into())))
            );
        }
    }

    #[test]
    fn a_single_anonymous_callback_is_left_alone() {
        let mut all_ir = BTreeMap::new();
        all_ir.insert(
            1,
            vec![Statement::Expr(Expression::Call {
                callee: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                    "map".into(),
                )))),
                arguments: vec![Expression::Function {
                    id: FunctionId(9),
                    name: None,
                    is_arrow: true,
                    is_async: false,
                    is_generator: false,
                }],
            })],
        );
        let before = all_ir.clone();
        let n = hoist_repeated_closures(&mut all_ir, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(n, 0);
        assert_eq!(all_ir, before);
    }

    #[test]
    fn a_named_iife_in_one_function_is_declared_there() {
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![iife(9, "sum")]);
        let n = hoist_repeated_closures(&mut all_ir, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(n, 1);
        assert_eq!(all_ir[&1].len(), 2);
        assert!(matches!(&all_ir[&1][0], Statement::Let { name, .. } if name == "sum"));
    }

    #[test]
    fn a_function_creating_itself_is_declared_outside_and_named_inside() {
        // F9 builds `{ optional: F9 }`; F1 (its parent) also creates F9 once.
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![Statement::Expr(func(9, "optional"))]);
        all_ir.insert(
            9,
            vec![Statement::Return(Some(Expression::Object {
                properties: vec![crate::ir::ObjectProperty {
                    key: crate::ir::PropertyKey::Ident("optional".into()),
                    value: func(9, "optional"),
                }],
            }))],
        );
        let parent_of: BTreeMap<u32, u32> = [(9, 1)].into_iter().collect();
        let n = hoist_repeated_closures(&mut all_ir, &parent_of, &BTreeMap::new());
        assert_eq!(n, 1);
        assert!(matches!(&all_ir[&1][0], Statement::Let { name, .. } if name == "optional"));
        let body = format!("{:?}", all_ir[&9]);
        assert!(
            !body.contains("Function"),
            "self reference became a name: {body}"
        );
    }

    #[test]
    fn a_two_member_cycle_is_declared_in_the_outside_holder() {
        // F1 creates F8; F8 creates F9; F9 creates F8. F8 and F9 form a cycle.
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![Statement::Expr(func(8, "or"))]);
        all_ir.insert(8, vec![Statement::Expr(func(9, "optional"))]);
        all_ir.insert(9, vec![Statement::Expr(func(8, "or"))]);
        let parent_of: BTreeMap<u32, u32> = [(8, 1), (9, 8)].into_iter().collect();
        hoist_repeated_closures(&mut all_ir, &parent_of, &BTreeMap::new());
        let body8 = format!("{:?}", all_ir[&8]);
        let body9 = format!("{:?}", all_ir[&9]);
        assert!(
            !body9.contains("Function"),
            "F9 no longer embeds F8: {body9}"
        );
        // F8 keeps at most the declaration of F9, never a site of itself.
        assert!(
            !body8.contains("FunctionId(8)"),
            "F8 no longer embeds itself: {body8}"
        );
        assert!(matches!(&all_ir[&1][0], Statement::Let { name, .. } if name == "or"));
    }

    #[test]
    fn two_helpers_with_one_name_get_distinct_bindings() {
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![iife(8, "T"), iife(9, "T")]);
        hoist_repeated_closures(&mut all_ir, &BTreeMap::new(), &BTreeMap::new());
        let names: Vec<String> = all_ir[&1]
            .iter()
            .filter_map(|s| match s {
                Statement::Let { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["T".to_string(), "T2".to_string()]);
    }

    #[test]
    fn a_name_the_scope_already_binds_takes_a_suffix() {
        let mut all_ir = BTreeMap::new();
        all_ir.insert(
            1,
            vec![
                Statement::Let {
                    name: "sum".into(),
                    value: Expression::constant(Constant::Integer(0)),
                    kind: VarKind::Let,
                },
                iife(9, "sum"),
            ],
        );
        hoist_repeated_closures(&mut all_ir, &BTreeMap::new(), &BTreeMap::new());
        assert!(matches!(&all_ir[&1][0], Statement::Let { name, .. } if name == "sum2"));
    }
}
