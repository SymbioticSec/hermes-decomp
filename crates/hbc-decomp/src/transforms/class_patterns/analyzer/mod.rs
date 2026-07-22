use crate::ir::{Statement, Expression, Value, Constant, AssignTarget, PropertyKey, MethodKind};
use std::collections::{BTreeMap, HashSet};
use super::builder::ClassBuilder;
use super::utils::{
    extract_name, get_target_name, is_likely_class_name, is_create_class_call,
    is_set_prototype_of_call, is_define_property_call, extract_method_array,
    extract_inheritance, extract_accessor_definition
};

mod emit;

pub struct ClassAnalyzer<'a> {
    file: &'a crate::BytecodeFile,
    format: &'a crate::BytecodeFormat,
    options: &'a crate::DecompileOptionsV2,
    closure_ctx: Option<&'a crate::ClosureContext>,
    // Map from register/variable name to class being built
    pub(super) classes: BTreeMap<String, ClassBuilder>,
    // Track which statements have been consumed into classes
    pub(super) consumed: HashSet<usize>,
    // Statement indices consumed on behalf of each class (key = class map key).
    // The class is emitted at its earliest index, so multiple classes in one
    // function each surface at the right position (and exactly once).
    pub(super) class_indices: BTreeMap<String, Vec<usize>>,
    // Variables/registers aliasing `<Class>.prototype` (e.g. HBC >=97 emits
    // `home = Class.prototype; home["m"] = fn`). Maps alias name → class map key.
    pub(super) proto_aliases: BTreeMap<String, String>,
}

impl<'a> ClassAnalyzer<'a> {
    pub fn new(
        file: &'a crate::BytecodeFile,
        format: &'a crate::BytecodeFormat,
        options: &'a crate::DecompileOptionsV2,
        closure_ctx: Option<&'a crate::ClosureContext>,
    ) -> Self {
        Self {
            file,
            format,
            options,
            closure_ctx,
            classes: BTreeMap::new(),
            consumed: HashSet::new(),
            class_indices: BTreeMap::new(),
            proto_aliases: BTreeMap::new(),
        }
    }

    // Mark statement `idx` as consumed on behalf of class `class_key`.
    pub(super) fn consume(&mut self, class_key: &str, idx: usize) {
        self.consumed.insert(idx);
        self.class_indices
            .entry(class_key.to_string())
            .or_default()
            .push(idx);
    }

    pub fn analyze(&mut self, stmts: Vec<Statement>) -> Vec<Statement> {
        // Pass 1: Identify class candidates from prototype usage
        let candidates = self.find_candidates(&stmts);

        // Pass 1b: Collect `<Class>.prototype` aliases (HBC >=97 `home = C.prototype`).
        self.collect_proto_aliases(&stmts);

        // Pass 2: Scan for class patterns
        for (idx, stmt) in stmts.iter().enumerate() {
            self.analyze_statement(stmt, idx, &candidates);
        }

        // Pass 3: Generate output, replacing consumed statements with classes.
        // Each class is emitted at its earliest consumed index so that multiple
        // classes in the same function keep their relative order and surface once.
        let mut anchor_to_class: BTreeMap<usize, String> = BTreeMap::new();
        for (class_key, indices) in &self.class_indices {
            if let Some(&min_idx) = indices.iter().min() {
                anchor_to_class.insert(min_idx, class_key.clone());
            }
        }

        let mut result = Vec::new();
        let mut emitted_classes: HashSet<String> = HashSet::new();

        for (idx, stmt) in stmts.into_iter().enumerate() {
            if self.consumed.contains(&idx) {
                // Emit the class anchored at this index (if any).
                if let Some(class_name) = anchor_to_class.get(&idx).cloned() {
                    if !emitted_classes.contains(&class_name) {
                        if let Some(builder) = self.classes.get(&class_name) {
                            result.push(self.build_class(builder));
                            emitted_classes.insert(class_name);
                        }
                    }
                }
                continue;
            }

            // Recursively transform nested statements
            result.push(self.transform_recursive(stmt));
        }

        // The constructor closure landed in a register (e.g. `r10000 = function
        // Animal(){}`); that assignment was consumed, but later references to the
        // class (`new r10000(...)`, `r10000.prototype` reads) still point at the
        // register. Rename them to the class display name so we emit `new Animal`.
        let mut reg_to_class: BTreeMap<u32, String> = BTreeMap::new();
        for (key, builder) in &self.classes {
            if let Some(reg) = key.strip_prefix('r').and_then(|n| n.parse::<u32>().ok()) {
                if emit::is_real_class_name(&builder.name) && builder.name != *key {
                    reg_to_class.insert(reg, builder.name.clone());
                }
            }
        }
        if !reg_to_class.is_empty() {
            result = crate::analysis::rename_registers(result, &reg_to_class);
        }

        result
    }

    fn find_candidates(&self, stmts: &[Statement]) -> HashSet<String> {
        let mut candidates = HashSet::new();

        for stmt in stmts {
            // Look for Foo.prototype usage
            if let Statement::Assign { target: AssignTarget::Member { object, property }, .. } = stmt {
                if property == "prototype" {
                    if let Some(name) = extract_name(object) {
                        candidates.insert(name);
                    }
                } else if let Expression::Member { object: inner, property: PropertyKey::Ident(prop), .. } = object {
                    if prop == "prototype" {
                        if let Some(name) = extract_name(inner) {
                            candidates.insert(name);
                        }
                    }
                }
            }

            // Look for _createClass calls
            if let Statement::Expr(Expression::Call { callee, arguments }) = stmt {
                if is_create_class_call(callee) && !arguments.is_empty() {
                    if let Some(name) = extract_name(&arguments[0]) {
                        candidates.insert(name);
                    }
                }
            }
        }

        candidates
    }

    // Record `home = <Class>.prototype` aliases so `home["m"] = fn` can be tied
    // back to the class (HBC >=97 lowers derived-class method definitions this way).
    fn collect_proto_aliases(&mut self, stmts: &[Statement]) {
        for stmt in stmts {
            let (alias, value) = match stmt {
                Statement::Assign {
                    target: target @ (AssignTarget::Register(_) | AssignTarget::Variable(_)),
                    value,
                } => (get_target_name(target), value),
                Statement::Let { name, value, .. } => (Some(name.clone()), value),
                _ => continue,
            };
            if let (Some(alias), Expression::Member { object, property: PropertyKey::Ident(p), .. }) =
                (alias, value)
            {
                if p == "prototype" {
                    if let Some(class_key) = extract_name(object) {
                        self.proto_aliases.insert(alias, class_key);
                    }
                }
            }
        }
    }

    // Resolve the owning class of a method-assignment object, either a direct
    // `<Class>.prototype` member or a tracked prototype alias.
    fn resolve_proto_class(&self, object: &Expression) -> Option<String> {
        if let Expression::Member { object: inner, property: PropertyKey::Ident(p), .. } = object {
            if p == "prototype" {
                return extract_name(inner);
            }
        }
        let name = extract_name(object)?;
        self.proto_aliases.get(&name).cloned()
    }

    fn analyze_statement(&mut self, stmt: &Statement, idx: usize, candidates: &HashSet<String>) {
        match stmt {
            // Pattern: Foo = function() { ... } (Constructor). Restrict to a plain
            // register/variable target so it does not swallow member-target method
            // assignments like `Foo.prototype.m = function() {}` (handled below).
            Statement::Assign { target: target @ (AssignTarget::Register(_) | AssignTarget::Variable(_)), value }
                if matches!(value, Expression::Function { .. }) =>
            {
                if let Some(name) = get_target_name(target) {
                    // A register holding a named constructor closure (`r5 = function
                    // Animal() {}`) is a class even though the register name itself
                    // isn't class-like; accept it when the closure carries a name.
                    let named_ctor = matches!(value, Expression::Function { name: Some(n), .. } if super::utils::is_likely_class_name(n));
                    if candidates.contains(&name) || is_likely_class_name(&name) || named_ctor {
                        self.register_constructor(&name, value.clone(), idx);
                    }
                }
            }

            // Pattern: let Foo = function() { ... }
            Statement::Let { name, value, .. } if matches!(value, Expression::Function { .. }) => {
                if candidates.contains(name) || is_likely_class_name(name) {
                    self.register_constructor(name, value.clone(), idx);
                }
            }

            // Pattern: Foo.prototype.method = function() { ... }
            Statement::Assign { target: AssignTarget::Member { object: Expression::Member { object: proto_obj, property: PropertyKey::Ident(proto_prop), .. }, property }, value }
                if matches!(value, Expression::Function { .. }) =>
            {
                if proto_prop == "prototype" {
                    if let Some(class_name) = extract_name(proto_obj) {
                        self.add_method(&class_name, property.clone(), value.clone(), false, MethodKind::Method, idx);
                    }
                }
            }

            // Pattern: <Class>.prototype["m"] = function() {...}  OR  alias["m"] = fn
            // (HBC >=97 DefineOwnByVal). Pulled INTO the class body ONLY when the
            // method uses `super`, which is a syntax error outside a class method.
            // Other methods stay as external `prototype["m"] = fn` assignments
            // (already valid JS) so existing output is not disturbed.
            Statement::Assign { target: AssignTarget::Index { object, key }, value }
                if matches!(value, Expression::Function { .. }) =>
            {
                if let Expression::Value(Value::Constant(Constant::String(method_name))) = key {
                    if let Some(class_name) = self.resolve_proto_class(object) {
                        if self.body_uses_super(value) {
                            self.add_method(&class_name, method_name.clone(), value.clone(), false, MethodKind::Method, idx);
                        }
                    }
                }
            }

            // Pattern: Foo.staticMethod = function() { ... }
            Statement::Assign { target: AssignTarget::Member { object, property }, value }
                if matches!(value, Expression::Function { .. }) =>
            {
                if let Some(class_name) = extract_name(object) {
                    if candidates.contains(&class_name) && property != "prototype" {
                        self.add_method(&class_name, property.clone(), value.clone(), true, MethodKind::Method, idx);
                    }
                }
            }

            // Pattern: Foo.prototype = { method: function() { ... }, ... }
            Statement::Assign { target: AssignTarget::Member { object, property }, value: Expression::Object { properties } }
                if property == "prototype" =>
            {
                if let Some(class_name) = extract_name(object) {
                    for prop in properties {
                        if let PropertyKey::Ident(method_name) | PropertyKey::String(method_name) = &prop.key {
                            if matches!(&prop.value, Expression::Function { .. }) {
                                self.add_method(&class_name, method_name.clone(), prop.value.clone(), false, MethodKind::Method, idx);
                            }
                        }
                    }
                    self.consume(&class_name, idx);
                }
            }

            // Pattern: _createClass(Foo, protoMethods, staticMethods)
            Statement::Expr(Expression::Call { callee, arguments }) if is_create_class_call(callee) => {
                if arguments.len() >= 2 {
                    if let Some(class_name) = extract_name(&arguments[0]) {
                        // Proto methods (2nd argument)
                        if let Some(methods) = extract_method_array(&arguments[1]) {
                            for (name, value, kind) in methods {
                                self.add_method(&class_name, name, value, false, kind, idx);
                            }
                        }
                        // Static methods (3rd argument if present)
                        if arguments.len() >= 3 {
                            if let Some(methods) = extract_method_array(&arguments[2]) {
                                for (name, value, kind) in methods {
                                    self.add_method(&class_name, name, value, true, kind, idx);
                                }
                            }
                        }
                        self.consume(&class_name, idx);
                    }
                }
            }

            // Pattern: __hermes_class_extends__(Class, Super), the synthetic marker
            // emitted by CreateDerivedClass desugaring (HBC >=97 `class B extends A`).
            Statement::Expr(Expression::Call { callee, arguments })
                if matches!(callee.as_ref(), Expression::Value(Value::Variable(n)) if n == crate::ir::EXTENDS_MARKER) =>
            {
                if let [class_arg, super_arg] = arguments.as_slice() {
                    if let Some(class_name) = extract_name(class_arg) {
                        let builder = self.classes.entry(class_name.clone()).or_insert_with(|| {
                            ClassBuilder { name: class_name.clone(), ..Default::default() }
                        });
                        // Stored as-is (often Register(baseClass)); the final
                        // register→class-name rename turns it into `extends A`.
                        builder.super_class = Some(super_arg.clone());
                        self.consume(&class_name, idx);
                    } else {
                        // Always drop the marker even if unresolved.
                        self.consumed.insert(idx);
                    }
                } else {
                    self.consumed.insert(idx);
                }
            }

            // Pattern: Object.setPrototypeOf(Foo.prototype, Bar.prototype) - inheritance
            Statement::Expr(Expression::Call { callee, arguments }) if is_set_prototype_of_call(callee) => {
                if arguments.len() >= 2 {
                    if let Some((class_name, super_name)) = extract_inheritance(&arguments[0], &arguments[1]) {
                        if let Some(builder) = self.classes.get_mut(&class_name) {
                            builder.super_class = Some(Expression::Value(Value::Variable(super_name)));
                        }
                        self.consume(&class_name, idx);
                    }
                }
            }

            // Pattern: Object.defineProperty(Foo.prototype, "prop", { get: ..., set: ... })
            Statement::Expr(Expression::Call { callee, arguments }) if is_define_property_call(callee) => {
                if arguments.len() >= 3 {
                    if let Some((class_name, prop_name, getter, setter)) = extract_accessor_definition(&arguments[0], &arguments[1], &arguments[2]) {
                        if let Some(getter_fn) = getter {
                            self.add_method(&class_name, prop_name.clone(), getter_fn, false, MethodKind::Getter, idx);
                        }
                        if let Some(setter_fn) = setter {
                            self.add_method(&class_name, prop_name, setter_fn, false, MethodKind::Setter, idx);
                        }
                        self.consume(&class_name, idx);
                    }
                }
            }

            _ => {}
        }
    }
}
