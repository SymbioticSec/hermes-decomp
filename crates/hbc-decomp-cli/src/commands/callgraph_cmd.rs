use crate::helpers::function_name;
use hbc_decomp::opcode::BytecodeFormat;
use hbc_decomp::BytecodeFile;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;

pub fn run_callgraph(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function: Option<u32>,
    depth: usize,
    dot: bool,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    if json {
        let analysis = hbc_decomp::analyze_module(file, format)?;
        let doc = callgraph_json(file, &analysis.graph.calls, function, depth);
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    let output = hbc_decomp::render_call_graph(file, format, function, depth, dot)?;
    print!("{output}");
    Ok(())
}

// The edges the text listing prints, as a JSON document. `root` and `depth`
// restrict the graph the same way the text listing does.
pub fn callgraph_json(
    file: &BytecodeFile,
    calls: &BTreeMap<u32, Vec<u32>>,
    root: Option<u32>,
    depth: usize,
) -> Value {
    let allowed = root.map(|r| reachable_within(calls, r, depth));
    let node_allowed = |id: u32| allowed.as_ref().is_none_or(|s| s.contains(&id));

    let mut edges = Vec::new();
    for (&caller, callees) in calls {
        if !node_allowed(caller) {
            continue;
        }
        let unique: BTreeSet<u32> = callees
            .iter()
            .copied()
            .filter(|&c| node_allowed(c))
            .collect();
        if unique.is_empty() {
            continue;
        }
        let callees: Vec<Value> = unique
            .iter()
            .map(|&c| json!({ "id": c, "name": function_name(file, c) }))
            .collect();
        edges.push(json!({
            "caller": caller,
            "name": function_name(file, caller),
            "callees": callees,
        }));
    }
    json!({
        "root": root,
        "depth": root.map(|_| depth),
        "edges": edges,
    })
}

// Nodes reachable from `root` within `depth` hops.
fn reachable_within(calls: &BTreeMap<u32, Vec<u32>>, root: u32, depth: usize) -> BTreeSet<u32> {
    let mut keep = BTreeSet::new();
    keep.insert(root);
    let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
    queue.push_back((root, 0));
    while let Some((node, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        if let Some(callees) = calls.get(&node) {
            for &c in callees {
                if keep.insert(c) {
                    queue.push_back((c, d + 1));
                }
            }
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_file() -> BytecodeFile {
        let bytes = hbc_decomp::create_minimal(&hbc_decomp::CreateOptions {
            version: 96,
            strings: vec!["global".into()],
            ..Default::default()
        })
        .unwrap();
        BytecodeFile::parse_auto(&bytes).unwrap()
    }

    fn graph() -> BTreeMap<u32, Vec<u32>> {
        BTreeMap::from([(0, vec![1, 1]), (1, vec![2]), (2, vec![3])])
    }

    #[test]
    fn callgraph_json_dedups_callees() {
        let doc = callgraph_json(&minimal_file(), &graph(), None, 3);
        let back: Value = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert!(back["root"].is_null());
        assert!(back["depth"].is_null());
        let edges = back["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 3);
        assert_eq!(edges[0]["caller"], 0);
        assert_eq!(edges[0]["callees"].as_array().unwrap().len(), 1);
        assert_eq!(edges[0]["callees"][0]["id"], 1);
    }

    #[test]
    fn callgraph_json_honours_root_and_depth() {
        let doc = callgraph_json(&minimal_file(), &graph(), Some(1), 1);
        let back: Value = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back["root"], 1);
        assert_eq!(back["depth"], 1);
        let edges = back["edges"].as_array().unwrap();
        // Only 1 -> 2 is within one hop of 1; 2 -> 3 is not, and 0 is unreachable.
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["caller"], 1);
        assert_eq!(edges[0]["callees"][0]["id"], 2);
    }
}
