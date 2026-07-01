use std::collections::HashMap;
use std::collections::BTreeMap;

// Parameter role assignments for a Metro factory function.
//
// Metro emits factories with one of a few well-known arities. The parameter
// order is fixed by Metro's `require` runtime, which invokes the factory as
// `factory(global, require, importDefault, importAll, module, exports, deps)`
// (modern) or the older `factory(global, require, module, exports[, deps])`.
// The *number of declared parameters* therefore determines the layout:
//
//   4 params: (global, require, module, exports)
//   5 params: (global, require, module, exports, dependencyMap)
//   6 params: (global, require, importDefault, importAll, module, exports)
//   7 params: (global, require, importDefault, importAll, module, exports, dependencyMap)
//
// All index fields below are in the *declared-parameter* space (i.e. the
// `Value::Parameter(idx)` / `argN` space, where `this` is excluded and the
// first declared parameter `global` is index 0).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FactoryRoles {
    // Number of declared parameters (this-excluded).
    pub param_count: u32,
    // Index of the `global` parameter (typically 0)
    pub global_idx: u32,
    // Index of the `require` parameter (typically 1)
    pub require_idx: u32,
    // Index of the `importDefault` helper, if present (modern factories, typically 2)
    pub import_default_idx: Option<u32>,
    // Index of the `importAll` helper, if present (modern factories, typically 3)
    pub import_all_idx: Option<u32>,
    // Index of the `module` parameter (2 classic, 4 modern)
    pub module_idx: u32,
    // Index of the `exports` parameter (3 classic, 5 modern)
    pub exports_idx: u32,
    // Index of the dependency map parameter, if present
    pub deps_idx: Option<u32>,
}

impl FactoryRoles {
    // Standard classic Metro factory: (global, require, module, exports)
    pub fn standard() -> Self {
        Self {
            param_count: 4,
            global_idx: 0,
            require_idx: 1,
            import_default_idx: None,
            import_all_idx: None,
            module_idx: 2,
            exports_idx: 3,
            deps_idx: None,
        }
    }

    // Derive the role layout from the number of *declared* parameters
    // (this-excluded). This is the reliable, version-independent signal:
    // Metro's factory arity directly encodes which convention is in use.
    pub fn from_param_count(declared: u32) -> Self {
        // Modern factories (>= 6 params) carry the importDefault/importAll
        // interop helpers between `require` and `module`.
        let has_helpers = declared >= 6;
        let (import_default_idx, import_all_idx, module_idx) = if has_helpers {
            (Some(2), Some(3), 4)
        } else {
            (None, None, 2)
        };
        let exports_idx = module_idx + 1;
        // A trailing dependency-map parameter is present when there is at least
        // one declared parameter past `exports`.
        let deps_idx = if declared > exports_idx + 1 {
            Some(exports_idx + 1)
        } else {
            None
        };
        Self {
            param_count: declared,
            global_idx: 0,
            require_idx: 1,
            import_default_idx,
            import_all_idx,
            module_idx,
            exports_idx,
            deps_idx,
        }
    }

    // Metro factory with explicit dependency map parameter (classic layout).
    pub fn with_deps(deps_idx: u32, param_count: u32) -> Self {
        Self {
            param_count,
            global_idx: 0,
            require_idx: 1,
            import_default_idx: None,
            import_all_idx: None,
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

    pub fn is_import_default_param(&self, name: &str) -> bool {
        self.import_default_idx
            .is_some_and(|idx| self.param_name_matches_idx(name, idx))
    }

    pub fn is_import_all_param(&self, name: &str) -> bool {
        self.import_all_idx
            .is_some_and(|idx| self.param_name_matches_idx(name, idx))
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

    // Handles canonical role names ("require", "exports", "module", "global",
    // "importDefault", "importAll", "dependencyMap") and numeric names
    // ("arg0", "p1", ...). The literal-name match takes precedence so that once
    // a factory parameter has been renamed to its semantic name, downstream
    // checks are correct regardless of any index assumptions.
    fn param_name_matches_idx(&self, name: &str, expected_idx: u32) -> bool {
        let literal = match expected_idx {
            idx if idx == self.require_idx => Some("require"),
            idx if idx == self.exports_idx => Some("exports"),
            idx if idx == self.module_idx => Some("module"),
            idx if idx == self.global_idx => Some("global"),
            idx if self.import_default_idx == Some(idx) => Some("importDefault"),
            idx if self.import_all_idx == Some(idx) => Some("importAll"),
            idx if self.deps_idx == Some(idx) => Some("dependencyMap"),
            _ => None,
        };
        if let Some(lit) = literal {
            if name == lit {
                return true;
            }
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_4_param_layout() {
        let r = FactoryRoles::from_param_count(4);
        assert_eq!((r.global_idx, r.require_idx, r.module_idx, r.exports_idx), (0, 1, 2, 3));
        assert_eq!(r.import_default_idx, None);
        assert_eq!(r.import_all_idx, None);
        assert_eq!(r.deps_idx, None);
        assert!(r.is_module_param("arg2"));
        assert!(r.is_exports_param("arg3"));
        assert!(!r.is_module_param("arg4"));
    }

    #[test]
    fn classic_5_param_has_deps() {
        let r = FactoryRoles::from_param_count(5);
        assert_eq!((r.module_idx, r.exports_idx), (2, 3));
        assert_eq!(r.deps_idx, Some(4));
        assert!(r.is_deps_param("arg4"));
    }

    #[test]
    fn modern_7_param_layout() {
        let r = FactoryRoles::from_param_count(7);
        assert_eq!(r.require_idx, 1);
        assert_eq!(r.import_default_idx, Some(2));
        assert_eq!(r.import_all_idx, Some(3));
        assert_eq!(r.module_idx, 4);
        assert_eq!(r.exports_idx, 5);
        assert_eq!(r.deps_idx, Some(6));
        // arg4/arg5 are module/exports in the modern layout, NOT arg2/arg3.
        assert!(r.is_module_param("arg4"));
        assert!(r.is_exports_param("arg5"));
        assert!(!r.is_module_param("arg2"));
        assert!(r.is_import_default_param("arg2"));
        assert!(r.is_import_all_param("arg3"));
        assert!(r.is_deps_param("arg6"));
    }

    #[test]
    fn modern_6_param_no_deps() {
        let r = FactoryRoles::from_param_count(6);
        assert_eq!((r.import_default_idx, r.import_all_idx), (Some(2), Some(3)));
        assert_eq!((r.module_idx, r.exports_idx), (4, 5));
        assert_eq!(r.deps_idx, None);
    }

    #[test]
    fn literal_role_names_match() {
        let r = FactoryRoles::from_param_count(7);
        assert!(r.is_module_param("module"));
        assert!(r.is_exports_param("exports"));
        assert!(r.is_require_param("require"));
        // a literal name only matches its own role
        assert!(!r.is_module_param("exports"));
    }
}
