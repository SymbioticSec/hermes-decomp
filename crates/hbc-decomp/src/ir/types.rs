use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub u32);

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "B{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FunctionId(pub u32);

impl fmt::Display for FunctionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "F{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constant {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Integer(i32),
    String(String),
    BigInt(String),
}

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::Undefined => write!(f, "undefined"),
            Constant::Null => write!(f, "null"),
            Constant::Bool(b) => write!(f, "{b}"),
            Constant::Number(n) if n.is_nan() => write!(f, "NaN"),
            Constant::Number(n) if n.is_infinite() => {
                write!(f, "{}Infinity", if n.is_sign_negative() { "-" } else { "" })
            }
            Constant::Number(n) => write!(f, "{n}"),
            Constant::Integer(i) => write!(f, "{i}"),
            Constant::String(s) => write!(f, "\"{}\"", crate::util::escape_js_string_bare(s)),
            Constant::BigInt(s) => write!(f, "{s}n"),
        }
    }
}

// The identity of a storage location.
//
// A name in the IR plays one of two roles: it designates a place that can be
// written, or it designates a value that is read. Those roles lived in two
// unrelated enums that each repeated the same three identities, so nothing
// stopped a pass from writing to something that was never a place. `Binding` is
// the identity itself, shared by both roles, which is what lets a pass ask what
// a name refers to instead of guessing from its spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Binding {
    Register(u32),
    Variable(String),
    ClosureVar { level: u32, slot: u32 },
}

impl Binding {
    /// The name this binding renders as, which is also how two bindings are
    /// told apart once registers have been named.
    pub fn name(&self) -> String {
        match self {
            Binding::Register(r) => format!("r{r}"),
            Binding::Variable(n) => n.clone(),
            Binding::ClosureVar { level, slot } => Value::closure_var_name(*level, *slot),
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Binding::Register(r) => write!(f, "r{r}"),
            // Matches `Value::Variable`, which sanitises on the way out.
            Binding::Variable(name) => write!(f, "{}", crate::util::sanitize_identifier(name)),
            Binding::ClosureVar { level, slot } => {
                write!(f, "{}", Value::closure_var_name(*level, *slot))
            }
        }
    }
}

impl From<Binding> for Value {
    fn from(b: Binding) -> Self {
        match b {
            Binding::Register(r) => Value::Register(r),
            Binding::Variable(n) => Value::Variable(n),
            Binding::ClosureVar { level, slot } => Value::ClosureVar { level, slot },
        }
    }
}

impl Value {
    /// The binding this value reads, when it reads one at all. A constant, `this`
    /// or `arguments` designates no storage location and yields `None`.
    pub fn as_binding(&self) -> Option<Binding> {
        match self {
            Value::Register(r) => Some(Binding::Register(*r)),
            Value::Variable(n) => Some(Binding::Variable(n.clone())),
            Value::ClosureVar { level, slot } => Some(Binding::ClosureVar {
                level: *level,
                slot: *slot,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Register(u32),
    Variable(String),
    Constant(Constant),
    This,
    Global,
    Parameter(u32),
    ClosureVar { level: u32, slot: u32 },
    Arguments,
    NewTarget,
    // The `super` keyword (ES6 class). Only valid inside a class method body;
    // produced when reconstructing `super.method()` from Hermes
    // GetByIdWithReceiver opcodes (emitted exclusively for super property access).
    Super,
}

impl Value {
    /// Stable JS identifier for an unresolved env-slot capture.
    ///
    /// Level 0 (current function env) → `closure_{slot}`.
    /// Parent envs (level ≥ 1) → `closure_{level}_{slot}` so nested captures stay
    /// in the same family as local ones (replaces the old `outerN_M` form, which
    /// looked like real source identifiers but never was).
    pub fn closure_var_name(level: u32, slot: u32) -> String {
        if level == 0 {
            format!("closure_{slot}")
        } else {
            format!("closure_{level}_{slot}")
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Register(r) => write!(f, "r{r}"),
            Value::Variable(name) => {
                // Sanitize identifiers (handles @@symbols, invalid chars, etc.)
                let sanitized = crate::util::sanitize_identifier(name);
                write!(f, "{sanitized}")
            }
            Value::Constant(c) => write!(f, "{c}"),
            Value::This => write!(f, "this"),
            Value::Global => write!(f, "globalThis"),
            Value::Parameter(i) => write!(f, "arg{i}"),
            Value::ClosureVar { level, slot } => {
                write!(f, "{}", Self::closure_var_name(*level, *slot))
            }
            Value::Arguments => write!(f, "arguments"),
            Value::NewTarget => write!(f, "new.target"),
            Value::Super => write!(f, "super"),
        }
    }
}

#[cfg(test)]
mod binding_tests {
    use super::{Binding, Value};

    #[test]
    fn a_value_that_reads_a_location_yields_its_binding() {
        assert_eq!(Value::Register(5).as_binding(), Some(Binding::Register(5)));
        assert_eq!(
            Value::Variable("env".into()).as_binding(),
            Some(Binding::Variable("env".into()))
        );
        assert_eq!(
            Value::ClosureVar { level: 1, slot: 2 }.as_binding(),
            Some(Binding::ClosureVar { level: 1, slot: 2 })
        );
    }

    #[test]
    fn a_value_that_designates_no_location_yields_none() {
        // These read no storage, so treating them as a place to write is the very
        // confusion the type is there to prevent.
        for v in [
            Value::This,
            Value::Global,
            Value::Arguments,
            Value::NewTarget,
            Value::Super,
            Value::Parameter(0),
            Value::Constant(super::Constant::Integer(1)),
        ] {
            assert_eq!(v.as_binding(), None, "{v:?} designates no location");
        }
    }

    #[test]
    fn the_round_trip_through_value_keeps_the_identity() {
        for b in [
            Binding::Register(3),
            Binding::Variable("obj".into()),
            Binding::ClosureVar { level: 0, slot: 7 },
        ] {
            let v: Value = b.clone().into();
            assert_eq!(v.as_binding(), Some(b));
        }
    }

    #[test]
    fn a_binding_renders_exactly_as_the_value_it_converts_to() {
        // The refactor must not move a single character of output, so the two
        // renderings have to agree.
        for b in [
            Binding::Register(3),
            Binding::Variable("obj".into()),
            Binding::Variable("get Foo".into()),
            Binding::ClosureVar { level: 0, slot: 7 },
            Binding::ClosureVar { level: 2, slot: 1 },
        ] {
            let v: Value = b.clone().into();
            assert_eq!(format!("{b}"), format!("{v}"));
        }
    }
}
