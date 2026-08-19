use crate::ir::{Expression, PropertyKey, Statement, Value};

mod legacy;
mod modern;

pub use legacy::detect_legacy_for_of;
pub use modern::detect_for_of_loops;

// `obj[Symbol.iterator]()` -> Some(obj)
pub(super) fn is_iterator_call(expr: &Expression) -> Option<Expression> {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.is_empty() {
            if let Expression::Member { object, property: PropertyKey::Computed(computed), .. } = callee.as_ref() {
                if let Expression::Member { object: sym, property: PropertyKey::Ident(p), .. } = computed.as_ref() {
                    if let Expression::Value(Value::Variable(n)) = sym.as_ref() {
                        if n == "Symbol" && p == "iterator" {
                            return Some((**object).clone());
                        }
                    }
                }
            }
        }
    }
    None
}

// Remove the `try { body } catch { iter.return(); throw }` wrapper and the
// trailing `// continue` marker that the iterator lowering leaves behind.
pub(super) fn unwrap_iterator_body(body: &[Statement], _iter_reg: u32) -> Vec<Statement> {
    let inner: Vec<Statement> = if body.len() == 1 {
        match &body[0] {
            Statement::TryCatch { try_body, .. } => try_body.clone(),
            _ => body.to_vec(),
        }
    } else {
        body.to_vec()
    };
    inner
        .into_iter()
        .filter(|s| !matches!(s, Statement::Comment(c) if c == "continue"))
        .collect()
}
