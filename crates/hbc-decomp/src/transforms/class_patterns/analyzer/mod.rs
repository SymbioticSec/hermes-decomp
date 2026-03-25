use crate::ir::{Statement, Expression, Value, AssignTarget, PropertyKey, MethodKind};
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
        }
    }

    pub fn analyze(&mut self, stmts: Vec<Statement>) -> Vec<Statement> {
        // Pass 1: Identify class candidates from prototype usage
        let candidates = self.find_candidates(&stmts);

        // Pass 2: Scan for class patterns
        for (idx, stmt) in stmts.iter().enumerate() {
            self.analyze_statement(stmt, idx, &candidates);
        }

        // Pass 3: Generate output, replacing consumed statements with classes
        let mut result = Vec::new();
        let mut emitted_classes: HashSet<String> = HashSet::new();

        for (idx, stmt) in stmts.into_iter().enumerate() {
            if self.consumed.contains(&idx) {
                // Check if we should emit a class here
                if let Some(class_name) = self.get_class_for_index(idx) {
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

    fn analyze_statement(&mut self, stmt: &Statement, idx: usize, candidates: &HashSet<String>) {
        match stmt {
            // Pattern: Foo = function() { ... } (Constructor)
            Statement::Assign { target, value } if matches!(value, Expression::Function { .. }) => {
                if let Some(name) = get_target_name(target) {
                    if candidates.contains(&name) || is_likely_class_name(&name) {
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
                    self.consumed.insert(idx);
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
                        self.consumed.insert(idx);
                    }
                }
            }

            // Pattern: Object.setPrototypeOf(Foo.prototype, Bar.prototype) - inheritance
            Statement::Expr(Expression::Call { callee, arguments }) if is_set_prototype_of_call(callee) => {
                if arguments.len() >= 2 {
                    if let Some((class_name, super_name)) = extract_inheritance(&arguments[0], &arguments[1]) {
                        if let Some(builder) = self.classes.get_mut(&class_name) {
                            builder.super_class = Some(Expression::Value(Value::Variable(super_name)));
                        }
                        self.consumed.insert(idx);
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
                        self.consumed.insert(idx);
                    }
                }
            }

            _ => {}
        }
    }
}
