use std::collections::HashMap;
use std::collections::BTreeMap;

// Parameter role assignments for a Metro factory function.
// Metro convention: `function(global, require, module, exports)` or with deps array.
// Instead of hardcoding parameter names like "arg1" or "exports", we store indices.
#[derive(Debug, Clone)]
pub struct FactoryRoles {
    pub param_count: u32,
    // Index of the `global` parameter (typically 0)
    pub global_idx: u32,
    // Index of the `require` parameter (typically 1)
    pub require_idx: u32,
    // Index of the `module` parameter (typically 2)
    pub module_idx: u32,
    // Index of the `exports` parameter (typically 3)
    pub exports_idx: u32,
    // Index of the dependency map parameter, if present (typically 4+)
    pub deps_idx: Option<u32>,
}

impl FactoryRoles {
    // Standard Metro factory: (global, require, module, exports)
    pub fn standard() -> Self {
        Self {
            param_count: 4,
            global_idx: 0,
            require_idx: 1,
            module_idx: 2,
            exports_idx: 3,
            deps_idx: None,
        }
    }

    // Metro factory with explicit dependency map parameter
    pub fn with_deps(deps_idx: u32, param_count: u32) -> Self {
        Self {
            param_count,
            global_idx: 0,
            require_idx: 1,
            module_idx: 2,
            exports_idx: 3,
            deps_idx: Some(deps_idx),
        }
    }

    // Handles "arg1", "p1", "require"
    pub fn is_require_param(&self, name: &str) -> bool {
        self.param_name_matches_idx(name, self.require_idx)
    }

    pub fn is_exports_param(&self, name: &str) -> bool {
        self.param_name_matches_idx(name, self.exports_idx)
    }

    pub fn is_module_param(&self, name: &str) -> bool {
        self.param_name_matches_idx(name, self.module_idx)
    }

    pub fn is_deps_param(&self, name: &str) -> bool {
        if let Some(idx) = self.deps_idx {
            self.param_name_matches_idx(name, idx)
        } else {
            // Heuristic: if param index >= exports_idx + 1, it could be deps
            if let Some(p_idx) = Self::extract_param_index(name) {
                return p_idx > self.exports_idx;
            }
            false
        }
    }

    // Handles: "arg0", "arg1", "p0", "p1", "require", "exports", "module", "global"
    fn param_name_matches_idx(&self, name: &str, expected_idx: u32) -> bool {
        match expected_idx {
            idx if idx == self.require_idx => {
                if name == "require" { return true; }
            }
            idx if idx == self.exports_idx => {
                if name == "exports" { return true; }
            }
            idx if idx == self.module_idx => {
                if name == "module" { return true; }
            }
            idx if idx == self.global_idx => {
                if name == "global" { return true; }
            }
            _ => {}
        }
        // Numeric parameter name matching (arg0, arg1, p0, p1, etc.)
        if let Some(p_idx) = Self::extract_param_index(name) {
            return p_idx == expected_idx;
        }
        false
    }

    // Extract parameter index from names like "arg0", "arg1", "p0", "p1"
    pub fn extract_param_index(name: &str) -> Option<u32> {
        name.strip_prefix("arg")
            .or_else(|| name.strip_prefix('p'))
            .and_then(|s| s.parse::<u32>().ok())
    }
}

impl Default for FactoryRoles {
    fn default() -> Self {
        Self::standard()
    }
}

// Information about a Metro module.
#[derive(Debug, Clone)]
pub struct MetroModule {
    // The module ID (used in require calls).
    // In Metro, this is typically an integer index (0, 1, 2...)
    // but can sometimes be mapped from complex requires.
    pub module_id: u32,
    // The function ID that implements this module
    pub function_id: u32,
    // Optional module name/path
    pub name: Option<String>,
    // Dependencies (module IDs this module requires)
    pub dependencies: Vec<u32>,
    // Exported functions (property name -> function ID)
    pub exports: HashMap<String, u32>,
    // Factory parameter roles (inferred from parameter count)
    pub roles: FactoryRoles,
}

// Registry of all Metro modules in a bundle.
//
// Helps traversing the dependency graph.
// Essential for resolving imports/requires across files.
//
// Example:
// A Require call `require(5)` inside function `f10` needs this registry to know that
// module 5 maps to function `f20`, so we can analyze `f20`'s exports.
#[derive(Debug, Clone, Default)]
pub struct MetroRegistry {
    // Module ID -> Module info
    pub modules: BTreeMap<u32, MetroModule>,
    // Function ID -> Module ID (reverse lookup)
    // Function ID -> Module ID (reverse lookup)
    pub function_to_module: BTreeMap<u32, u32>,
    // Function ID -> Module info (Factory specific definition)
    pub factories: BTreeMap<u32, MetroModule>,
}

impl MetroRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // Get a module by its ID.
    pub fn get_module(&self, module_id: u32) -> Option<&MetroModule> {
        self.modules.get(&module_id)
    }

    // Get the module that a function implements.
    pub fn get_module_for_function(&self, function_id: u32) -> Option<&MetroModule> {
        // Prefer factory definition if available
        self.factories.get(&function_id).or_else(|| {
            self.function_to_module
                .get(&function_id)
                .and_then(|mod_id| self.modules.get(mod_id))
        })
    }

    // Graph related helpers that just access the struct (not traversing) can stay here,
    // but deeper traversal (like trees) should move to graph.rs.
    // For now we expose the data directly.

    // Get all modules that depend on a given module.
    pub fn get_dependents(&self, module_id: u32) -> Vec<u32> {
        self.modules
            .values()
            .filter(|m| m.dependencies.contains(&module_id))
            .map(|m| m.module_id)
            .collect()
    }
}
