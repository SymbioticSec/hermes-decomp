// Patch an existing Hermes bytecode image (hermes_rs issue #10 class).
//
// Split by concern: `strings` (string table edits, same-length and resize),
// `functions` (whole function body replace), `inject` (stub injection).

use crate::write::serialize::SerializeOptions;

pub mod functions;
pub mod inject;
pub mod strings;

pub use functions::{patch_function_body, patch_function_bytes};
pub use inject::{inject_stub, InjectStubKind};
pub use strings::{patch_string_by_id, patch_string_replace};

#[derive(Debug, Clone, Default)]
pub struct PatchOptions {
    pub serialize: SerializeOptions,
}
