use super::CFG;
use crate::ir::Terminator;

pub fn generate_dot(cfg: &CFG, function_name: &str) -> String {
    let mut out = String::new();
    out.push_str("digraph CFG {\n");
    out.push_str("  node [shape=box, fontname=\"Courier\", style=filled, fillcolor=white];\n");
    out.push_str(&format!("  label=\"{function_name}\";\n"));
    out.push_str("  labelloc=\"t\";\n");

    for (id, block) in cfg.blocks_with_ids() {
        let mut label = String::new();
        label.push_str(&format!("B{}\\l", id.0));
        label.push_str("----------------\\l");

        for stmt in &block.statements {
            let s = format!("{stmt}").replace("\"", "\\\"");
            label.push_str(&format!("{s}\\l"));
        }

        let term_str = format!("{}", block.terminator).replace("\"", "\\\"");
        label.push_str(&format!("{term_str}\\l"));

        out.push_str(&format!("  {} [label=\"{}\"];\n", id.0, label));
    }

    for (id, block) in cfg.blocks_with_ids() {
        match &block.terminator {
            Terminator::Jump(target) => {
                out.push_str(&format!("  {} -> {};\n", id.0, target.0));
            }
            Terminator::Branch {
                true_target,
                false_target,
                ..
            } => {
                out.push_str(&format!(
                    "  {} -> {} [label=\"true\", color=green];\n",
                    id.0, true_target.0
                ));
                out.push_str(&format!(
                    "  {} -> {} [label=\"false\", color=red];\n",
                    id.0, false_target.0
                ));
            }
            Terminator::Switch { cases, default, .. } => {
                for (val, target) in cases {
                    out.push_str(&format!(
                        "  {} -> {} [label=\"{}\"];\n",
                        id.0, target.0, val
                    ));
                }
                out.push_str(&format!(
                    "  {} -> {} [label=\"default\"];\n",
                    id.0, default.0
                ));
            }
            Terminator::Return(_) | Terminator::Throw(_) => {}
            Terminator::None => {}
        }
    }

    out.push_str("}\n");
    out
}
