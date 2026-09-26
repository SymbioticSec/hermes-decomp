mod analyze;
mod naming;
mod types;
mod value;

#[cfg(test)]
mod tests;

pub(crate) use naming::name_from_constant_text;
pub use types::{encode_level_slot, ClosureInfo, ClosureSlotValue};
pub use value::{ident_from_property_key, value_from_expr};
