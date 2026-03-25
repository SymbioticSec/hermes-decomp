use std::collections::HashSet;
use super::registry::MetroRegistry;

#[derive(Debug, Clone)]
pub struct DependencyTree {
    pub module_id: u32,
    pub function_id: Option<u32>,
    pub name: Option<String>,
    pub children: Vec<DependencyTree>,
}

impl DependencyTree {
    pub fn format(&self, indent: usize) -> String {
        let mut output = String::new();
        let prefix = "  ".repeat(indent);

        let func_str = self
            .function_id
            .map(|f| format!(" (F{f})"))
            .unwrap_or_default();
        let name_str = self
            .name
            .as_ref()
            .map(|n| format!(" - {n}"))
            .unwrap_or_default();

        output.push_str(&format!(
            "{}Module {}{}{}\n",
            prefix, self.module_id, func_str, name_str
        ));

        for child in &self.children {
            output.push_str(&child.format(indent + 1));
        }

        output
    }
}

pub struct DependencyGraph;

impl DependencyGraph {
    pub fn get_dependency_tree(
        registry: &MetroRegistry,
        module_id: u32,
        max_depth: usize,
    ) -> DependencyTree {
        Self::build_tree(
            registry,
            module_id,
            0,
            max_depth,
            &mut HashSet::new(),
        )
    }

    fn build_tree(
        registry: &MetroRegistry,
        module_id: u32,
        depth: usize,
        max_depth: usize,
        visited: &mut HashSet<u32>,
    ) -> DependencyTree {
        let mut tree = DependencyTree {
            module_id,
            function_id: registry.modules.get(&module_id).map(|m| m.function_id),
            name: registry
                .modules
                .get(&module_id)
                .and_then(|m| m.name.clone()),
            children: Vec::new(),
        };

        if depth >= max_depth || visited.contains(&module_id) {
            return tree;
        }

        visited.insert(module_id);

        if let Some(module) = registry.modules.get(&module_id) {
            for &dep_id in &module.dependencies {
                tree.children.push(Self::build_tree(
                    registry,
                    dep_id,
                    depth + 1,
                    max_depth,
                    visited,
                ));
            }
        }

        tree
    }
}
