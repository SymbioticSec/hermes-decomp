use crate::ir::{BlockId, Terminator, CFG};
use std::collections::{BTreeMap, BTreeSet, HashSet};

// Information about a detected loop.
#[derive(Debug, Clone)]
pub struct LoopInfo {
    pub header: BlockId,
    pub body: BTreeSet<BlockId>,
    pub exit: Option<BlockId>,
    pub back_edges: Vec<(BlockId, BlockId)>,
    pub is_do_while: bool,
}

// Detect all loops in the CFG using back-edge analysis.
//
// Strategy:
// 1. Compute Dominators for the CFG.
// 2. Find Back-Edges: edges `A -> B` where `B` dominates `A`. `B` is the loop header.
// 3. Construct Loop Body: Start from `A` and walk predecessors backwards until reaching `B`.
// 4. Find Loop Exit: Successors of body nodes that are outside the body.
pub fn detect_loops(cfg: &CFG) -> Vec<LoopInfo> {
    let dominators = compute_dominators(cfg);
    let mut loops = Vec::new();
    let mut back_edges = Vec::new();

    // Find back edges (edge to a dominator)
    for block in cfg.blocks() {
        for succ in block.successors() {
            if dominators
                .get(&block.id)
                .map(|d| d.contains(&succ))
                .unwrap_or(false)
            {
                back_edges.push((block.id, succ));
            }
        }
    }

    let mut headers: BTreeMap<BlockId, Vec<(BlockId, BlockId)>> = BTreeMap::new();
    for edge in back_edges {
        headers.entry(edge.1).or_default().push(edge);
    }

    for (header, edges) in headers {
        let mut body = BTreeSet::new();
        body.insert(header);

        for (from, _) in &edges {
            collect_loop_body(cfg, *from, header, &mut body);
        }

        // Find loop exit (successor of header not in body, or branch target out of body)
        let exit = find_loop_exit(cfg, header, &body);

        // Determine if this is a do-while (condition at end)
        let is_do_while = edges.iter().any(|(from, to)| {
            *to == header
                && cfg
                    .get(*from)
                    .map(|b| matches!(b.terminator, Terminator::Branch { .. }))
                    .unwrap_or(false)
        });

        loops.push(LoopInfo {
            header,
            body,
            exit,
            back_edges: edges,
            is_do_while,
        });
    }

    loops
}

fn collect_loop_body(cfg: &CFG, node: BlockId, header: BlockId, body: &mut BTreeSet<BlockId>) {
    if body.contains(&node) {
        return;
    }
    body.insert(node);
    for pred in cfg.predecessors(node) {
        if pred != header {
            collect_loop_body(cfg, pred, header, body);
        }
    }
}

fn find_loop_exit(cfg: &CFG, header: BlockId, body: &BTreeSet<BlockId>) -> Option<BlockId> {
    if let Some(block) = cfg.get(header) {
        for succ in block.successors() {
            if !body.contains(&succ) {
                return Some(succ);
            }
        }
    }

    for &block_id in body {
        if let Some(block) = cfg.get(block_id) {
            for succ in block.successors() {
                if !body.contains(&succ) {
                    return Some(succ);
                }
            }
        }
    }

    None
}

pub fn compute_dominators(cfg: &CFG) -> BTreeMap<BlockId, HashSet<BlockId>> {
    let all_blocks: HashSet<_> = cfg.block_ids().collect();
    let mut dom: BTreeMap<BlockId, HashSet<BlockId>> = BTreeMap::new();

    for id in cfg.block_ids() {
        if id == cfg.entry {
            let mut s = HashSet::new();
            s.insert(id);
            dom.insert(id, s);
        } else {
            dom.insert(id, all_blocks.clone());
        }
    }

    // Fixed-point iteration
    let rpo = cfg.reverse_postorder();
    let mut changed = true;

    while changed {
        changed = false;
        for &block_id in &rpo {
            if block_id == cfg.entry {
                continue;
            }

            let preds = cfg.predecessors(block_id);
            if preds.is_empty() {
                continue;
            }

            let mut new_dom = dom.get(&preds[0]).cloned().unwrap_or_default();
            for &pred in &preds[1..] {
                if let Some(pred_dom) = dom.get(&pred) {
                    new_dom = new_dom.intersection(pred_dom).copied().collect();
                }
            }
            new_dom.insert(block_id);

            if new_dom != *dom.get(&block_id).unwrap() {
                changed = true;
                dom.insert(block_id, new_dom);
            }
        }
    }

    dom
}
