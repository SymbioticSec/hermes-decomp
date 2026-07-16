use super::closure_usage::ClosureUsageInfo;

// Infer a name from the collected usage patterns of a closure variable.
pub(super) fn infer_name_from_closure_usage(info: &ClosureUsageInfo, slot_name_hint: Option<&str>) -> Option<String> {
    let all_accessed: Vec<&str> = info.methods.iter()
        .chain(info.properties.iter())
        .map(|s| s.as_str())
        .collect();

    // Prefer a meaningful slot hint from the parent StoreToEnvironment
    // (e.g. factory roles rewritten to require/dependencyMap, or a named var).
    if let Some(hint) = slot_name_hint {
        if is_strong_slot_hint(hint) {
            return Some(hint.to_string());
        }
    }

    // Heavily indexed capture → dependency map / native module table.
    // Even a single index is a strong signal when there are no other accesses.
    if info.indexed_accesses >= 1 && all_accessed.is_empty() {
        if let Some(hint) = slot_name_hint {
            if hint.contains("dep") || hint == "dependencyMap" || hint == "require" {
                return Some(hint.to_string());
            }
        }
        // Called after index: `table[i](…)` is the classic TurboModule / BatchedBridge shape.
        if info.called_as_function || info.indexed_accesses >= 2 {
            return Some("dependencyMap".to_string());
        }
        return Some("table".to_string());
    }

    // Spread of a capture with no other usage → arguments.
    if info.spread && all_accessed.is_empty() && info.indexed_accesses == 0 {
        return Some("args".to_string());
    }

    // Logger: only .log / .warn / .error / .info / .debug
    if !info.methods.is_empty()
        && info.methods.iter().all(|m| {
            matches!(m.as_str(), "log" | "warn" | "error" | "info" | "debug" | "trace")
        })
    {
        return Some("logger".to_string());
    }

    // Heuristic 0: React/JSX patterns, very common in React Native bundles
    let react_hooks = ["useState", "useRef", "useEffect", "useCallback", "useMemo",
                       "useContext", "useReducer", "useLayoutEffect", "useImperativeHandle"];
    let react_methods = ["createElement", "createRef", "createContext", "forwardRef",
                         "memo", "lazy", "Suspense", "Fragment", "Children"];
    let jsx_props = ["jsx", "jsxs", "jsxDEV"];

    let has_react_hook = all_accessed.iter().any(|a| react_hooks.contains(a));
    let has_react_method = all_accessed.iter().any(|a| react_methods.contains(a));
    let has_jsx = all_accessed.iter().any(|a| jsx_props.contains(a));

    if has_react_hook || has_react_method || has_jsx {
        return Some("React".to_string());
    }

    // React Native Animated API
    if all_accessed.iter().any(|a| matches!(*a, "Animated" | "withTiming" | "withSpring" | "withDecay"
        | "useSharedValue" | "useAnimatedStyle" | "runOnUI" | "runOnJS"
        | "interpolate" | "Easing" | "cancelAnimation")) {
        return Some("Animated".to_string());
    }

    // GraphQL pattern
    if all_accessed.iter().any(|a| matches!(*a, "GraphQLError" | "DocumentNode" | "gql" | "useQuery" | "useMutation")) {
        return Some("graphql".to_string());
    }

    // StyleSheet pattern
    if info.methods.iter().any(|m| m == "create") && info.properties.iter().any(|p| p == "hairlineWidth" || p == "flatten" || p == "absoluteFill") {
        return Some("StyleSheet".to_string());
    }

    // Regex pattern: requires definitive regex methods (.exec or .test)
    if all_accessed.iter().any(|a| matches!(*a, "exec" | "test")) {
        // Confirm with absence of non-regex properties
        let has_non_regex = all_accessed.iter().any(|a| matches!(*a,
            "alternate" | "memoizedState" | "child" | "sibling" | "stateNode"
            | "pendingProps" | "memoizedProps" | "updateQueue" | "return"));
        if !has_non_regex {
            return Some("regex".to_string());
        }
    }

    // React ref pattern: .current is the primary access and no fiber properties
    let has_current = info.properties.iter().any(|p| p == "current");
    let has_fiber_props = all_accessed.iter().any(|a| matches!(*a,
        "child" | "sibling" | "tag" | "flags" | "lanes" | "mode" | "type"
        | "stateNode" | "pendingProps" | "memoizedProps" | "memoizedState"
        | "updateQueue" | "return" | "alternate" | "refCleanup"));
    if has_current && !has_fiber_props && info.properties.len() <= 3 {
        return Some("ref".to_string());
    }

    // Redux pattern
    if all_accessed.iter().any(|a| matches!(*a, "useSelector" | "useDispatch" | "useStore" | "connect" | "Provider")) {
        return Some("redux".to_string());
    }

    // React Native core components
    if all_accessed.iter().any(|a| matches!(*a, "View" | "Text" | "Image" | "ScrollView" | "FlatList"
        | "TouchableOpacity" | "TextInput" | "ActivityIndicator" | "Modal" | "SafeAreaView")) {
        return Some("RN".to_string());
    }

    // Platform check
    if all_accessed.iter().any(|a| matches!(*a, "Platform" | "OS" | "select"))
        && all_accessed.iter().any(|a| matches!(*a, "Platform" | "OS")) {
        return Some("Platform".to_string());
    }

    // Validation / assertion utilities
    if all_accessed.iter().any(|a| matches!(*a, "isValid" | "raiseError" | "devAssert" | "invariant")) {
        return Some("assert".to_string());
    }

    // Heuristic 1: Only called as function, no property access → callback
    if info.called_as_function && all_accessed.is_empty() {
        // Use slot hint if available
        if let Some(hint) = slot_name_hint {
            if !hint.starts_with("closure_") && !hint.starts_with("f")
                && !hint.starts_with("arg") && !hint.starts_with("r") {
                return Some(hint.to_string());
            }
        }
        return Some("callback".to_string());
    }

    // Heuristic 2: Method call patterns → type-based naming
    if !info.methods.is_empty() {
        // Store pattern: multiple set*/get* methods
        let setter_getter_count = info.methods.iter()
            .filter(|m| m.starts_with("set") || m.starts_with("get"))
            .count();
        if setter_getter_count >= 2 {
            // Try to extract a common domain from the method names
            if let Some(domain) = extract_store_domain(&info.methods) {
                return Some(format!("{domain}Store"));
            }
            return Some("store".to_string());
        }

        // Navigation pattern, requires at least one navigation-specific method
        if info.methods.iter().any(|m| matches!(m.as_str(), "navigate" | "goBack" | "reset"))
            || (info.methods.iter().any(|m| m == "push" || m == "replace")
                && info.methods.iter().any(|m| matches!(m.as_str(), "navigate" | "goBack" | "reset" | "getParam" | "setParams")))
        {
            return Some("navigation".to_string());
        }

        // Promise pattern
        if info.methods.iter().any(|m| matches!(m.as_str(), "then" | "catch" | "finally")) {
            return Some("promise".to_string());
        }

        // Array pattern
        if info.methods.iter().any(|m| matches!(m.as_str(), "push" | "pop" | "shift" | "unshift" | "splice" | "slice")) {
            return Some("arr".to_string());
        }

        // Map/Set pattern
        if info.methods.iter().any(|m| matches!(m.as_str(), "has" | "delete"))
            && info.methods.iter().any(|m| m == "set" || m == "get")
        {
            return Some("map".to_string());
        }

        // Set pattern (has + add but no set/get)
        if info.methods.iter().any(|m| m == "has" || m == "add" || m == "delete")
            && !info.methods.iter().any(|m| m == "set" || m == "get") {
            return Some("set".to_string());
        }

        // Prototype access → class/constructor
        if info.methods.iter().any(|m| m == "prototype") || info.properties.iter().any(|p| p == "prototype") {
            return Some("ctor".to_string());
        }

        // Single method with set/get prefix → use the domain
        if info.methods.len() == 1 {
            let method = &info.methods[0];
            if let Some(domain) = strip_accessor_prefix(method) {
                if !domain.is_empty() {
                    return Some(to_camel_case(domain));
                }
            }
        }
    }

    // Heuristic 3: Property-only access patterns
    if !info.properties.is_empty() && info.methods.is_empty() && !info.called_as_function {
        // Single .default access → likely a module
        if info.properties.len() == 1 && info.properties[0] == "default" {
            // If we have a slot name hint from ClosureContext, use it
            if let Some(hint) = slot_name_hint {
                if !hint.starts_with("closure_") && !hint.starts_with("f")
                    && !hint.starts_with("arg") && !hint.starts_with("r") {
                    return Some(hint.to_string());
                }
            }
            return Some("mod".to_string());
        }

        // .prototype only → constructor
        if info.properties.iter().any(|p| p == "prototype") {
            return Some("ctor".to_string());
        }

        // ALL_CAPS properties → constants object
        if info.properties.iter().all(|p| p.chars().all(|c| c.is_ascii_uppercase() || c == '_')) {
            return Some("constants".to_string());
        }

        // Single property → use property name as the object name
        if info.properties.len() == 1 {
            let prop = &info.properties[0];
            // Avoid naming the object the same as its property
            return Some(infer_object_name_from_property(prop));
        }

        // Multiple properties → try to find a common theme
        if let Some(name) = infer_name_from_multiple_properties(&info.properties) {
            return Some(name);
        }
    }

    // Heuristic 4: Mixed usage (methods + properties + calls)
    if !all_accessed.is_empty() {
        // If called as function AND has property accesses → likely a module or multi-purpose object
        if info.called_as_function {
            // Use slot hint if meaningful
            if let Some(hint) = slot_name_hint {
                if !hint.starts_with("closure_") && !hint.starts_with("f")
                    && !hint.starts_with("arg") && !hint.starts_with("r") {
                    return Some(hint.to_string());
                }
            }
            return Some("lib".to_string());
        }

        // Fall back to the most descriptive single access
        if all_accessed.len() == 1 {
            let name = all_accessed[0];
            if name == "default" {
                return Some("mod".to_string());
            }
            return Some(infer_object_name_from_property(name));
        }
    }

    // Heuristic 5: Slot name hint as last resort (for closures with no usage patterns)
    if let Some(hint) = slot_name_hint {
        if is_usable_slot_hint(hint) {
            return Some(hint.to_string());
        }
    }

    None
}

// Strong hints from the parent env store, always prefer these.
fn is_strong_slot_hint(hint: &str) -> bool {
    matches!(
        hint,
        "require"
            | "dependencyMap"
            | "exports"
            | "module"
            | "global"
            | "importDefault"
            | "importAll"
            | "args"
            | "logger"
            | "React"
            | "StyleSheet"
            | "Platform"
    ) || (hint.len() > 2
        && !hint.starts_with("closure_")
        && !hint.starts_with('c')
        && !hint.starts_with("arg")
        && !hint.starts_with('r')
        && !hint.starts_with('f')
        && hint.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && hint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$'))
}

fn is_usable_slot_hint(hint: &str) -> bool {
    hint.len() > 1
        && !hint.starts_with("closure_")
        && !hint.starts_with('f')
        && !hint.starts_with("arg")
        && !hint.starts_with('r')
        && !(hint.starts_with('c') && hint[1..].chars().all(|c| c.is_ascii_digit()))
}

// Extract a common domain name from setter/getter methods.
// e.g., ["setToken", "setErrorLogin", "getUser"] → "auth" (common theme)
// e.g., ["setLoading", "setData"] → "state"
fn extract_store_domain(methods: &[String]) -> Option<String> {
    let stripped: Vec<&str> = methods.iter()
        .filter_map(|m| strip_accessor_prefix(m))
        .collect();

    if stripped.is_empty() {
        return None;
    }

    // Check for common React Native / state management patterns
    let lower: Vec<String> = stripped.iter().map(|s| s.to_lowercase()).collect();

    // Auth-related
    if lower.iter().any(|s| s.contains("token") || s.contains("login") || s.contains("auth") || s.contains("user") || s.contains("password")) {
        return Some("auth".to_string());
    }
    // Loading/error state
    if lower.iter().any(|s| s.contains("loading")) && lower.iter().any(|s| s.contains("error") || s.contains("data")) {
        return Some("state".to_string());
    }
    // UI state
    if lower.iter().any(|s| s.contains("visible") || s.contains("modal") || s.contains("show") || s.contains("open")) {
        return Some("ui".to_string());
    }

    None
}

// Strip set/get/is prefix from a method name, returning the remaining domain.
fn strip_accessor_prefix(method: &str) -> Option<&str> {
    if method.len() > 3
        && (method.starts_with("set") || method.starts_with("get"))
        && method.as_bytes()[3].is_ascii_uppercase()
    {
        Some(&method[3..])
    } else if method.len() > 2 && method.starts_with("is") && method.as_bytes()[2].is_ascii_uppercase() {
        Some(&method[2..])
    } else {
        None
    }
}

// Convert PascalCase to camelCase.
fn to_camel_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => {
            let lower: String = c.to_lowercase().collect();
            format!("{}{}", lower, chars.collect::<String>())
        }
        None => String::new(),
    }
}

// Infer an object name from a single property being accessed.
// e.g., "email" → "user", "EVENT_NAME" → "events"
fn infer_object_name_from_property(prop: &str) -> String {
    // ALL_CAPS → constants
    if prop.chars().all(|c| c.is_ascii_uppercase() || c == '_') && prop.len() > 1 {
        return "constants".to_string();
    }

    // Known property → domain mappings
    match prop {
        "email" | "password" | "username" | "token" | "avatar" => "user".to_string(),
        "navigate" | "goBack" | "push" | "replace" | "reset" | "params" | "route" => "navigation".to_string(),
        "dispatch" | "getState" => "store".to_string(),
        "width" | "height" | "flex" | "margin" | "padding" | "fontSize" => "styles".to_string(),
        "current" => "ref".to_string(),
        _ => {
            // Use the property name itself if it's a reasonable identifier
            let sanitized = super::suggestions::sanitize_name(prop);
            if sanitized.len() <= 20 {
                sanitized
            } else {
                "obj".to_string()
            }
        }
    }
}

// Infer a name from multiple property accesses.
fn infer_name_from_multiple_properties(properties: &[String]) -> Option<String> {
    // Check for style-related properties
    let style_props = ["width", "height", "flex", "margin", "padding", "fontSize", "color", "backgroundColor", "borderRadius"];
    if properties.iter().any(|p| style_props.contains(&p.as_str())) {
        return Some("styles".to_string());
    }

    // Check for user-related properties
    let user_props = ["email", "password", "username", "name", "avatar", "id", "token"];
    if properties.iter().filter(|p| user_props.contains(&p.as_str())).count() >= 2 {
        return Some("user".to_string());
    }

    // Check for config-related properties
    let config_props = ["baseURL", "timeout", "headers", "apiKey", "endpoint", "host", "port"];
    if properties.iter().any(|p| config_props.contains(&p.as_str())) {
        return Some("config".to_string());
    }

    // If has "default" plus other props, likely a module
    if properties.iter().any(|p| p == "default") {
        return Some("mod".to_string());
    }

    None
}
