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

// Whether a corpus fixture is present, for the tests that need real bytecode.
//
// The corpus is rebuilt from the repo by scripts/build/fetch_hermesc.sh then
// build_corpus.sh, and its compiled artefacts are deliberately not tracked. A
// missing fixture used to make these tests return quietly, which turned them
// into permanent no-ops everywhere the corpus had not been built, CI included.
// Now the default is to fail and say how to fix it, and a run that knowingly
// has no corpus opts out through HBC_CORPUS_OPTIONAL.
#[cfg(test)]
pub(crate) fn corpus_fixture_present(path: &str) -> bool {
    if std::path::Path::new(path).exists() {
        return true;
    }
    if std::env::var_os("HBC_CORPUS_OPTIONAL").is_some() {
        eprintln!("skip: corpus fixture missing ({path})");
        return false;
    }
    panic!(
        "corpus fixture missing:\n  {path}\n\
         Build it with scripts/build/fetch_hermesc.sh then \
         scripts/build/build_corpus.sh,\n\
         or set HBC_CORPUS_OPTIONAL=1 to skip the tests that need one."
    );
}
