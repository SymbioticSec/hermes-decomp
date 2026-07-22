// Bytecode write path: encode, assemble, patch, create.
//
// See repository `ROADMAP.md`. Does *not* recompile decompiled JavaScript.

pub mod create;
pub mod encode;
pub mod footer;
pub mod hasm;
pub mod header_write;
pub mod patch;
pub mod reloc;
pub mod serialize;

pub use create::{create_minimal, CreateOptions};
pub use encode::{encode_function_body, encode_instruction};
pub use footer::{append_footer, compute_file_hash, rehash_footer, verify_footer};
pub use hasm::{
    assemble_function_hasm, assemble_module, emit_hasm_function, parse_hasm,
    parse_hasm_with_context, HasmFunction, HasmModule,
};
pub use patch::{
    inject_stub, patch_function_body, patch_function_bytes, patch_string_by_id,
    patch_string_replace, InjectStubKind, PatchOptions,
};
pub use reloc::RelocPlan;
pub use serialize::{finalize_raw_image, serialize_file, write_file, SerializeOptions};
