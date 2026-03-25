mod builder;
mod cfg;
pub mod expr;
mod stmt;
mod types;
pub mod utils;
mod visitor;

pub use builder::*;
pub use cfg::*;
pub use expr::*;
pub use stmt::{AssignTarget, ClassMethod, MethodKind, Statement, Terminator, VarKind};
pub use types::*;
pub use utils::{
    expr_uses_register, exprs_equal, extract_function_id, get_value_name,
    is_nan_check, is_simple_value, is_undefined_expr, map_nested_bodies,
    map_nested_bodies_mut, property_key_uses_register, property_keys_equal,
    stmt_has_side_effects, stmt_uses_register, target_to_key,
};
pub use visitor::*;
