use std::collections::HashSet;
use std::collections::BTreeMap;

// A link between two function parameters, used for cross-function name propagation.
// When function A passes its parameter `src_param` to function B's parameter `dst_param`,
// the name inferred for one can propagate to the other.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct ParamLink {
    pub src_func: u32,
    pub src_param: u32,
    pub dst_func: u32,
    pub dst_param: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GlobalAnalysis {
    pub param_names: BTreeMap<u32, Vec<Option<String>>>, // FunctionID -> [Param Names]
    pub param_links: Vec<ParamLink>,
    pub graph: crate::analysis::ipa::graph::CallGraph,
    pub dead_code: HashSet<u32>,
}

impl Default for GlobalAnalysis {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalAnalysis {
    pub fn new() -> Self {
        Self {
            param_names: BTreeMap::new(),
            param_links: Vec::new(),
            graph: crate::analysis::ipa::graph::CallGraph::new(),
            dead_code: HashSet::new(),
        }
    }
}
