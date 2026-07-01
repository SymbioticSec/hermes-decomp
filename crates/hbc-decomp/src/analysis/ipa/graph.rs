use std::collections::HashSet;
use std::collections::BTreeMap;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CallGraph {
    pub calls: BTreeMap<u32, Vec<u32>>,
    pub callers: BTreeMap<u32, Vec<u32>>,
}

impl Default for CallGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl CallGraph {
    pub fn new() -> Self {
        Self {
            calls: BTreeMap::new(),
            callers: BTreeMap::new(),
        }
    }

    pub fn add_call(&mut self, caller: u32, callee: u32) {
        self.calls.entry(caller).or_default().push(callee);
        self.callers.entry(callee).or_default().push(caller);
    }

    // Compute a post-order traversal of the call graph (Bottom-Up).
    // Leaf functions (no callees) appear first.
    // This handles cycles by visiting nodes only once.
    pub fn compute_post_order(&self) -> Vec<u32> {
        let mut visited = HashSet::new();
        let mut post_order = Vec::new();

        // Find all functions involved in calls (check both keys and values to safe)
        let mut all_funcs = HashSet::new();
        for (&caller, callees) in &self.calls {
            all_funcs.insert(caller);
            for &callee in callees {
                all_funcs.insert(callee);
            }
        }

        // Ensure deterministic order for stable results
        let mut sorted_funcs: Vec<u32> = all_funcs.into_iter().collect();
        sorted_funcs.sort();

        for func_id in sorted_funcs {
            if !visited.contains(&func_id) {
                self.dfs_post_order(func_id, &mut visited, &mut post_order);
            }
        }

        post_order
    }

    fn dfs_post_order(
        &self,
        u: u32,
        visited: &mut HashSet<u32>,
        post_order: &mut Vec<u32>,
    ) {
        visited.insert(u);
        if let Some(callees) = self.calls.get(&u) {
            for &v in callees {
                if !visited.contains(&v) {
                    self.dfs_post_order(v, visited, post_order);
                }
            }
        }
        post_order.push(u);
    }

    // Identify functions reachable from a set of root functions.
    pub fn get_reachable_functions(&self, roots: &[u32]) -> HashSet<u32> {
        let mut reachable = HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        for &root in roots {
            reachable.insert(root);
            queue.push_back(root);
        }

        while let Some(u) = queue.pop_front() {
            if let Some(callees) = self.calls.get(&u) {
                for &v in callees {
                    if !reachable.contains(&v) {
                        reachable.insert(v);
                        queue.push_back(v);
                    }
                }
            }
        }

        reachable
    }
}
