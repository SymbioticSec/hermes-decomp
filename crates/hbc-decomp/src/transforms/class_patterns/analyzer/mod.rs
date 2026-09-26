use super::builder::ClassBuilder;
use super::utils::{
    extract_accessor_definition, extract_inheritance, extract_method_array, extract_name,
    get_target_name, is_create_class_call, is_define_property_call, is_likely_class_name,
    is_set_prototype_of_call,
};
use crate::ir::{
    AssignTarget, Binding, Constant, Expression, MethodKind, PropertyKey, Statement, Value,
};
use std::collections::{BTreeMap, HashSet};

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

        // One class can be collected under two keys: the register its
        // constructor landed in and the variable it was later copied to. Left
        // apart they come out as two `class X` declarations of the same name,
        // and a module with two bindings of one name does not parse. Fold the
        // register-keyed builder into the named one first.
        self.merge_same_named_builders();

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

    fn merge_same_named_builders(&mut self) {
        let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, builder) in &self.classes {
            if emit::is_real_class_name(&builder.name) {
                by_name
                    .entry(builder.name.clone())
                    .or_default()
                    .push(key.clone());
            }
        }
        for (name, keys) in by_name {
            if keys.len() < 2 {
                continue;
            }
            // The key that is the class name itself survives, else the first.
            let keep = keys
                .iter()
                .find(|k| **k == name)
                .cloned()
                .unwrap_or_else(|| keys[0].clone());
            for key in keys.into_iter().filter(|k| *k != keep) {
                let Some(other) = self.classes.remove(&key) else {
                    continue;
                };
                let indices = self.class_indices.remove(&key).unwrap_or_default();
                let Some(target) = self.classes.get_mut(&keep) else {
                    continue;
                };
                if target.constructor.is_none() {
                    target.constructor = other.constructor;
                    target.constructor_body = other.constructor_body;
                    target.constructor_params = other.constructor_params;
                }
                if target.super_class.is_none() {
                    target.super_class = other.super_class;
                }
                for method in other.methods {
                    let dup = target
                        .methods
                        .iter()
                        .any(|m| m.key == method.key && m.is_static == method.is_static);
                    if !dup {
                        target.methods.push(method);
                    }
                }
                self.class_indices
                    .entry(keep.clone())
                    .or_default()
                    .extend(indices);
                for alias in self.proto_aliases.values_mut() {
                    if *alias == key {
                        *alias = keep.clone();
                    }
                }
            }
        }
    }

    fn find_candidates(&self, stmts: &[Statement]) -> HashSet<String> {
        let mut candidates = HashSet::new();

        for stmt in stmts {
            // Look for Foo.prototype usage
            if let Statement::Assign {
                target: AssignTarget::Member { object, property },
                ..
            } = stmt
            {
                if property == "prototype" {
                    if let Some(name) = extract_name(object) {
                        candidates.insert(name);
                    }
                } else if let Expression::Member {
                    object: inner,
                    property: PropertyKey::Ident(prop),
                    ..
                } = object
                {
                    if prop == "prototype" {
                        if let Some(name) = extract_name(inner) {
                            candidates.insert(name);
                        }
                    }
                }
            }

            // `home = X.prototype` (HBC >=97 method definitions go through the
            // home object) and the `extends` marker of a derived class both
            // name a class whose anonymous constructor closure would otherwise
            // stay a plain `const x = function(...)` next to an empty class.
            let alias_value = match stmt {
                Statement::Assign {
                    target:
                        AssignTarget::Binding(Binding::Register(_))
                        | AssignTarget::Binding(Binding::Variable(_)),
                    value,
                } => Some(value),
                Statement::Let { value, .. } => Some(value),
                _ => None,
            };
            if let Some(Expression::Member {
                object,
                property: PropertyKey::Ident(p),
                ..
            }) = alias_value
            {
                if p == "prototype" {
                    if let Some(name) = extract_name(object) {
                        candidates.insert(name);
                    }
                }
            }

            if let Statement::Expr(Expression::Call { callee, arguments }) = stmt {
                // Look for _createClass calls
                if is_create_class_call(callee) && !arguments.is_empty() {
                    if let Some(name) = extract_name(&arguments[0]) {
                        candidates.insert(name);
                    }
                }
                let is_marker = matches!(callee.as_ref(),
                    Expression::Value(Value::Binding(Binding::Variable(n))) if n == crate::ir::EXTENDS_MARKER);
                if is_marker {
                    if let Some(name) = arguments.first().and_then(extract_name) {
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
                    target:
                        target @ (AssignTarget::Binding(Binding::Register(_))
                        | AssignTarget::Binding(Binding::Variable(_))),
                    value,
                } => (get_target_name(target), value),
                Statement::Let { name, value, .. } => (Some(name.clone()), value),
                _ => continue,
            };
            if let (
                Some(alias),
                Expression::Member {
                    object,
                    property: PropertyKey::Ident(p),
                    ..
                },
            ) = (alias, value)
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
        if let Expression::Member {
            object: inner,
            property: PropertyKey::Ident(p),
            ..
        } = object
        {
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
            Statement::Assign {
                target:
                    target @ (AssignTarget::Binding(Binding::Register(_))
                    | AssignTarget::Binding(Binding::Variable(_))),
                value,
            } if matches!(value, Expression::Function { .. }) => {
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
            Statement::Assign {
                target:
                    AssignTarget::Member {
                        object:
                            Expression::Member {
                                object: proto_obj,
                                property: PropertyKey::Ident(proto_prop),
                                ..
                            },
                        property,
                    },
                value,
            } if matches!(value, Expression::Function { .. }) => {
                if proto_prop == "prototype" {
                    if let Some(class_name) = extract_name(proto_obj) {
                        self.add_method(
                            &class_name,
                            property.clone(),
                            value.clone(),
                            false,
                            MethodKind::Method,
                            idx,
                        );
                    }
                }
            }

            // Pattern: <Class>.prototype["m"] = function() {...}  OR  alias["m"] = fn
            // (HBC >=97 DefineOwnByVal): this is how hermesc lowers a method
            // written in a class body, so it goes back into the class body. A
            // `Foo.prototype.m = fn` written in the source is a PutById and
            // stays a member assignment (handled above). `Class["m"] = function`
            // is the static form of the same lowering.
            Statement::Assign {
                target: AssignTarget::Index { object, key },
                value,
            } if matches!(value, Expression::Function { .. }) => {
                if let Expression::Value(Value::Constant(Constant::String(method_name))) = key {
                    let (class_name, is_static) = match self.resolve_proto_class(object) {
                        Some(c) => (Some(c), false),
                        None => (
                            extract_name(object)
                                .filter(|n| candidates.contains(n) || self.classes.contains_key(n)),
                            true,
                        ),
                    };
                    if let Some(class_name) = class_name {
                        self.add_method(
                            &class_name,
                            method_name.clone(),
                            value.clone(),
                            is_static,
                            MethodKind::Method,
                            idx,
                        );
                    }
                }
            }

            // Pattern: Foo.staticMethod = function() { ... }
            Statement::Assign {
                target: AssignTarget::Member { object, property },
                value,
            } if matches!(value, Expression::Function { .. }) => {
                if let Some(class_name) = extract_name(object) {
                    if candidates.contains(&class_name) && property != "prototype" {
                        self.add_method(
                            &class_name,
                            property.clone(),
                            value.clone(),
                            true,
                            MethodKind::Method,
                            idx,
                        );
                    }
                }
            }

            // Pattern: Foo.prototype = { method: function() { ... }, ... }
            Statement::Assign {
                target: AssignTarget::Member { object, property },
                value: Expression::Object { properties },
            } if property == "prototype" => {
                if let Some(class_name) = extract_name(object) {
                    for prop in properties {
                        if let PropertyKey::Ident(method_name) | PropertyKey::String(method_name) =
                            &prop.key
                        {
                            if matches!(&prop.value, Expression::Function { .. }) {
                                self.add_method(
                                    &class_name,
                                    method_name.clone(),
                                    prop.value.clone(),
                                    false,
                                    MethodKind::Method,
                                    idx,
                                );
                            }
                        }
                    }
                    self.consume(&class_name, idx);
                }
            }

            // Pattern: _createClass(Foo, protoMethods, staticMethods)
            Statement::Expr(Expression::Call { callee, arguments })
                if is_create_class_call(callee) =>
            {
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
            Statement::Expr(Expression::Call { callee, arguments }) if matches!(callee.as_ref(), Expression::Value(Value::Binding(Binding::Variable(n))) if n == crate::ir::EXTENDS_MARKER) =>
            {
                if let [class_arg, super_arg] = arguments.as_slice() {
                    if let Some(class_name) = extract_name(class_arg) {
                        let builder = self.classes.entry(class_name.clone()).or_insert_with(|| {
                            ClassBuilder {
                                name: class_name.clone(),
                                ..Default::default()
                            }
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
            Statement::Expr(Expression::Call { callee, arguments })
                if is_set_prototype_of_call(callee) =>
            {
                if arguments.len() >= 2 {
                    if let Some((class_name, super_name)) =
                        extract_inheritance(&arguments[0], &arguments[1])
                    {
                        if let Some(builder) = self.classes.get_mut(&class_name) {
                            builder.super_class = Some(Expression::Value(Value::Binding(
                                Binding::Variable(super_name),
                            )));
                        }
                        self.consume(&class_name, idx);
                    }
                }
            }

            // Pattern: Object.defineProperty(Foo.prototype, "prop", { get: ..., set: ... })
            Statement::Expr(Expression::Call { callee, arguments })
                if is_define_property_call(callee) =>
            {
                if arguments.len() >= 3 {
                    if let Some((class_name, prop_name, getter, setter)) =
                        extract_accessor_definition(&arguments[0], &arguments[1], &arguments[2])
                    {
                        if let Some(getter_fn) = getter {
                            self.add_method(
                                &class_name,
                                prop_name.clone(),
                                getter_fn,
                                false,
                                MethodKind::Getter,
                                idx,
                            );
                        }
                        if let Some(setter_fn) = setter {
                            self.add_method(
                                &class_name,
                                prop_name,
                                setter_fn,
                                false,
                                MethodKind::Setter,
                                idx,
                            );
                        }
                        self.consume(&class_name, idx);
                    }
                }
            }

            _ => {}
        }
    }
}

#[cfg(test)]
mod candidate_tests {
    use super::*;

    fn reg(n: u32) -> Expression {
        Expression::Value(Value::Binding(Binding::Register(n)))
    }

    #[test]
    fn a_string_keyed_prototype_method_joins_the_class_body() {
        let create = crate::write::CreateOptions {
            version: 98,
            ..Default::default()
        };
        let bytes = crate::write::create_minimal(&create).expect("minimal file");
        let file = crate::BytecodeFile::parse_auto(&bytes).expect("parse");
        let format = crate::BytecodeFormat::for_version(file.header.version).expect("format");
        let options = crate::DecompileOptionsV2::default();
        let mut analyzer = ClassAnalyzer::new(&file, &format, &options, None);
        let func = |id: u32, name: &str| Expression::Function {
            id: crate::ir::FunctionId(id),
            name: Some(name.to_string()),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        // r1 = function Widget(){}; r2 = r1.prototype; r2["render"] = function render(){}
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(1)),
                value: func(900, "Widget"),
            },
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(2)),
                value: Expression::member(reg(1), "prototype"),
            },
            Statement::Assign {
                target: AssignTarget::Index {
                    object: reg(2),
                    key: Expression::Value(Value::Constant(Constant::String("render".into()))),
                },
                value: func(901, "render"),
            },
        ];
        let out = analyzer.analyze(stmts);
        let class = out
            .iter()
            .find_map(|s| match s {
                Statement::Class { name, methods, .. } => Some((name.clone(), methods)),
                _ => None,
            })
            .expect("a class is emitted");
        assert_eq!(class.0, "Widget");
        assert!(
            class.1.iter().any(|m| m.key == "render" && !m.is_static),
            "render is a method of the class, not a prototype assignment"
        );
        assert!(
            !out.iter().any(|s| matches!(
                s,
                Statement::Assign {
                    target: AssignTarget::Index { .. },
                    ..
                }
            )),
            "the prototype assignment was consumed"
        );
    }

    #[test]
    fn a_prototype_alias_and_an_extends_marker_name_the_class() {
        let create = crate::write::CreateOptions {
            version: 98,
            ..Default::default()
        };
        let bytes = crate::write::create_minimal(&create).expect("minimal file");
        let file = crate::BytecodeFile::parse_auto(&bytes).expect("parse");
        let format = crate::BytecodeFormat::for_version(file.header.version).expect("format");
        let options = crate::DecompileOptionsV2::default();
        let analyzer = ClassAnalyzer::new(&file, &format, &options, None);
        // r1 = function(){}; r2 = r1.prototype; __hermes_class_extends__(r1, r3)
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(2)),
                value: Expression::member(reg(1), "prototype"),
            },
            Statement::Expr(Expression::Call {
                callee: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                    crate::ir::EXTENDS_MARKER.to_string(),
                )))),
                arguments: vec![reg(4), reg(3)],
            }),
        ];
        let candidates = analyzer.find_candidates(&stmts);
        assert!(candidates.contains("r1"), "home = r1.prototype names r1");
        assert!(candidates.contains("r4"), "the extends marker names r4");
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use crate::ir::{ClassMethod, MethodKind};
    use crate::transforms::class_patterns::builder::ClassBuilder;

    #[test]
    fn a_register_keyed_builder_folds_into_the_named_one() {
        let create = crate::write::CreateOptions {
            version: 98,
            ..Default::default()
        };
        let bytes = crate::write::create_minimal(&create).expect("minimal file");
        let file = crate::BytecodeFile::parse_auto(&bytes).expect("parse");
        let format = crate::BytecodeFormat::for_version(98).expect("format");
        let options = crate::DecompileOptionsV2::default();
        let mut analyzer = ClassAnalyzer::new(&file, &format, &options, None);
        analyzer.classes.insert(
            "Performance".into(),
            ClassBuilder {
                name: "Performance".into(),
                ..Default::default()
            },
        );
        let mut by_register = ClassBuilder {
            name: "Performance".into(),
            ..Default::default()
        };
        by_register.methods.push(ClassMethod {
            key: "now".into(),
            value: Expression::Value(Value::Constant(crate::ir::Constant::Undefined)),
            body: None,
            is_static: false,
            kind: MethodKind::Method,
            params: vec![],
        });
        analyzer.classes.insert("r10023".into(), by_register);
        analyzer.class_indices.insert("Performance".into(), vec![3]);
        analyzer.class_indices.insert("r10023".into(), vec![9]);

        analyzer.merge_same_named_builders();

        assert_eq!(analyzer.classes.len(), 1);
        let kept = &analyzer.classes["Performance"];
        assert_eq!(kept.methods.len(), 1);
        assert_eq!(analyzer.class_indices["Performance"], vec![3, 9]);
    }
}
