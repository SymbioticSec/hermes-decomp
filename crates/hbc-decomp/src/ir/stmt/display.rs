use super::{AssignTarget, Statement, Terminator};
use std::fmt;

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Statement::Expr(e) => write!(f, "{e};"),
            Statement::Let { name, value, kind } => write!(f, "{kind} {name} = {value};"),
            Statement::Assign { target, value } => write!(f, "{target} = {value};"),
            Statement::Delete { target, result } => {
                if let Some(r) = result {
                    write!(f, "r{r} = delete {target};")
                } else {
                    write!(f, "delete {target};")
                }
            }
            Statement::Return(Some(e)) => write!(f, "return {e};"),
            Statement::Return(None) => write!(f, "return;"),
            Statement::Throw(e) => write!(f, "throw {e};"),
            Statement::Debugger => write!(f, "debugger;"),
            Statement::Comment(s) => write!(f, "// {s}"),
            Statement::Break(label) => {
                if let Some(l) = label {
                    write!(f, "break {l};")
                } else {
                    write!(f, "break;")
                }
            }
            Statement::Continue(label) => {
                if let Some(l) = label {
                    write!(f, "continue {l};")
                } else {
                    write!(f, "continue;")
                }
            }
            Statement::Goto(t) => write!(f, "goto {t};"),
            Statement::CondGoto {
                condition,
                target,
                fallthrough,
            } => {
                write!(f, "if ({condition}) goto {target} else goto {fallthrough};")
            }
            Statement::If {
                condition,
                then_body: _,
                else_body,
            } => {
                write!(f, "if ({condition}) {{ ... }}")?;
                if !else_body.is_empty() {
                    write!(f, " else {{ ... }}")?;
                }
                Ok(())
            }
            Statement::While { condition, .. } => {
                write!(f, "while ({condition}) {{ ... }}")
            }
            Statement::DoWhile { condition, .. } => {
                write!(f, "do {{ ... }} while ({condition})")
            }
            Statement::For { condition, .. } => {
                let cond = condition
                    .as_ref()
                    .map(|c| format!("{c}"))
                    .unwrap_or_default();
                write!(f, "for (; {cond}; ) {{ ... }}")
            }
            Statement::ForOf {
                variable, iterable, ..
            } => {
                write!(f, "for (const {variable} of {iterable}) {{ ... }}")
            }
            Statement::ForIn {
                variable, object, ..
            } => {
                write!(f, "for (const {variable} in {object}) {{ ... }}")
            }
            Statement::Switch {
                discriminant,
                cases,
                default,
            } => {
                writeln!(f, "switch ({discriminant}) {{")?;
                for (val, body) in cases {
                    writeln!(f, "  case {val}:")?;
                    writeln!(f, "    ... {} statements", body.len())?;
                }
                if let Some(d) = default {
                    writeln!(f, "  default:")?;
                    writeln!(f, "    ... {} statements", d.len())?;
                }
                write!(f, "}}")
            }
            Statement::TryCatch {
                catch_param,
                finally_body,
                ..
            } => {
                write!(f, "try {{ ... }}")?;
                if let Some(param) = catch_param {
                    write!(f, " catch ({param}) {{ ... }}")?;
                }
                if !finally_body.is_empty() {
                    write!(f, " finally {{ ... }}")?;
                }
                Ok(())
            }
            Statement::Block(_) => write!(f, "{{ ... }}"),
            Statement::Class {
                name,
                super_class,
                methods,
                ..
            } => {
                write!(f, "class {name}")?;
                if let Some(super_cls) = super_class {
                    write!(f, " extends {super_cls}")?;
                }
                write!(f, " {{")?;

                writeln!(f)?;
                for method in methods {
                    if method.is_static {
                        write!(f, "  static ")?;
                    }
                    if method.body.is_some() {
                        writeln!(f, "  {}() {{ /* inlined body */ }}", method.key)?;
                    } else {
                        writeln!(f, "  {}() {{ ... }}", method.key)?;
                    }
                }
                write!(f, "}}")
            }
        }
    }
}

impl fmt::Display for AssignTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssignTarget::Variable(name) => {
                let sanitized = crate::util::sanitize_identifier(name);
                write!(f, "{sanitized}")
            }
            AssignTarget::Register(r) => write!(f, "r{r}"),
            AssignTarget::Member { object, property } => {
                let obj = format!("{object}");
                let s = crate::ir::expr::display::format_member_access_with(
                    &obj,
                    "",
                    &crate::ir::PropertyKey::Ident(property.clone()),
                    |e| format!("{e}"),
                );
                write!(f, "{s}")
            }
            AssignTarget::Index { object, key } => write!(f, "{object}[{key}]"),
            AssignTarget::ClosureVar { level, slot } => {
                if *level == 0 {
                    write!(f, "closure_{slot}")
                } else {
                    write!(f, "outer{level}_{slot}")
                }
            }
            AssignTarget::DestructuringArray(targets) => {
                write!(f, "[")?;
                for (i, target) in targets.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if let Some((t, def)) = target {
                        write!(f, "{t}")?;
                        if let Some(d) = def {
                            write!(f, " = {d}")?;
                        }
                    }
                }
                write!(f, "]")
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                write!(f, "[")?;
                for (i, target) in elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if let Some((t, def)) = target {
                        write!(f, "{t}")?;
                        if let Some(d) = def {
                            write!(f, " = {d}")?;
                        }
                    }
                }
                if !elements.is_empty() {
                    write!(f, ", ")?;
                }
                write!(f, "...{rest}]")
            }
            AssignTarget::DestructuringObject(props) => {
                write!(f, "{{")?;
                for (i, (key, target, default_val)) in props.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    let is_shorthand = if let AssignTarget::Variable(v) = target {
                        v == key
                    } else {
                        false
                    };

                    if is_shorthand {
                        write!(f, "{key}")?;
                    } else {
                        write!(f, "{key}: {target}")?;
                    }

                    if let Some(d) = default_val {
                        write!(f, " = {d}")?;
                    }
                }
                write!(f, "}}")
            }
            AssignTarget::DestructuringObjectRest { properties, rest } => {
                write!(f, "{{")?;
                for (i, (key, target, default_val)) in properties.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    let is_shorthand = if let AssignTarget::Variable(v) = target {
                        v == key
                    } else {
                        false
                    };

                    if is_shorthand {
                        write!(f, "{key}")?;
                    } else {
                        write!(f, "{key}: {target}")?;
                    }

                    if let Some(d) = default_val {
                        write!(f, " = {d}")?;
                    }
                }
                if !properties.is_empty() {
                    write!(f, ", ")?;
                }
                write!(f, "...{rest}}}")
            }
            AssignTarget::Rest(inner) => {
                write!(f, "...{inner}")
            }
        }
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Jump(target) => write!(f, "goto {target}"),
            Terminator::Branch {
                condition,
                true_target,
                false_target,
            } => {
                write!(
                    f,
                    "if ({condition}) goto {true_target} else goto {false_target}"
                )
            }
            Terminator::Return(Some(e)) => write!(f, "return {e}"),
            Terminator::Return(None) => write!(f, "return"),
            Terminator::Throw(e) => write!(f, "throw {e}"),
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                write!(f, "switch ({value}) ")?;
                for (val, target) in cases {
                    write!(f, "case {val}: {target} ")?;
                }
                write!(f, "default: {default}")
            }
            Terminator::None => write!(f, "<no terminator>"),
        }
    }
}
