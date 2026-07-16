// On-disk cache for the (expensive) whole-program analysis.
//
// `PipelineContext::build_with_options` takes several seconds on a large bundle
// because it runs the full analysis pipeline over every function. That work only
// depends on the bytecode itself *and the decompiler binary*, so we serialize
// the resulting context to disk keyed by:
//   - SHA-256 of the `.hbc` bytes
//   - SHA-256 of the running `hermes-decomp` executable (auto-invalidates on
//     any rebuild, no manual CACHE_VERSION bump required for output changes)
//   - a manual CACHE_VERSION (schema / wire-format safety net)
//   - output-affecting options
// A later invocation on the same file with the same binary deserializes the
// context (sub-second); any mismatch rebuilds transparently.

use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::analysis::{ClosureContext, GlobalAnalysis, MetroRegistry};
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::opcode::BytecodeFormat;

use super::context::PipelineContext;
use super::DecompileOptionsV2;

// Bump when the *on-disk schema* changes (header fields, snapshot layout) in a
// way that old entries must not be read. Pipeline *output* changes are covered
// by `binary_fingerprint()`, no need to bump for every decompiler fix.
pub const CACHE_VERSION: u32 = 3;
const MAGIC: [u8; 4] = *b"HDC1";

// Standard cache path for an input file: `<input>.hdcache` next to it.
pub fn default_cache_path(input: &Path) -> PathBuf {
    let mut name = input.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".hdcache");
    input.with_file_name(name)
}

// SHA-256 of the bytecode. A cryptographic digest means the key changes if any
// byte changes and distinct files never collide in practice (256-bit), so a
// cache is only ever reused for the exact same file.
fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// Identity of the running decompiler binary (SHA-256 of `current_exe()`).
///
/// Computed once per process. Any recompilation produces a different binary
/// image → different fingerprint → automatic cache miss. A wrong key is only a
/// miss (rebuild), never a wrong result.
///
/// If the executable cannot be read, returns a sentinel that will not match a
/// real digest, so we never reuse a foreign cache entry.
pub fn binary_fingerprint() -> [u8; 32] {
    static FP: OnceLock<[u8; 32]> = OnceLock::new();
    *FP.get_or_init(|| match std::env::current_exe().and_then(std::fs::read) {
        Ok(bytes) => hash_bytes(&bytes),
        Err(_) => {
            // 0xFF… is not a SHA-256 of any real file (SHA-256 is uniform; we
            // only need "won't collide with a previous real fingerprint").
            [0xFF; 32]
        }
    })
}

// Only these options actually change the built context (see build_with_options).
fn options_key(options: &DecompileOptionsV2) -> u32 {
    (options.assembly_mode as u32) | ((options.include_offsets as u32) << 1)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheHeader {
    magic: [u8; 4],
    cache_version: u32,
    file_hash: [u8; 32],
    /// SHA-256 of the decompiler binary that produced this entry.
    binary_hash: [u8; 32],
    options_key: u32,
}

impl CacheHeader {
    // A cached entry is usable only if every field matches: same magic, same
    // cache format version, same bytecode, same decompiler binary, and same
    // output-affecting options.
    fn matches(&self, want: &CacheHeader) -> bool {
        self.magic == want.magic
            && self.cache_version == want.cache_version
            && self.file_hash == want.file_hash
            && self.binary_hash == want.binary_hash
            && self.options_key == want.options_key
    }
}

// Serializable mirror of PipelineContext. `inline_bodies` is stored as a plain
// map (the Arc is reconstructed on load); worklet_sources is copied verbatim.
#[derive(serde::Serialize, serde::Deserialize)]
struct PipelineSnapshot {
    all_ir: BTreeMap<u32, Vec<Statement>>,
    registry: MetroRegistry,
    closure_ctx: Option<ClosureContext>,
    global_analysis: GlobalAnalysis,
    inline_bodies: BTreeMap<u32, String>,
    worklet_sources: BTreeMap<String, String>,
}

impl PipelineContext {
    // Build the pipeline, using an on-disk cache at `cache_path` keyed by the
    // bytecode `bytes`. Any cache read/write failure (missing, corrupt, stale,
    // permission) silently falls back to a normal build, the cache is an
    // optimization, never a correctness dependency.
    pub fn build_cached(
        file: &BytecodeFile,
        format: &BytecodeFormat,
        options: &DecompileOptionsV2,
        bytes: &[u8],
        cache_path: &Path,
    ) -> Result<Self> {
        let header = CacheHeader {
            magic: MAGIC,
            cache_version: CACHE_VERSION,
            file_hash: hash_bytes(bytes),
            binary_hash: binary_fingerprint(),
            options_key: options_key(options),
        };

        if let Some(ctx) = try_load(cache_path, &header) {
            log::debug!("[cache] hit: {}", cache_path.display());
            return Ok(ctx);
        }

        let t = std::time::Instant::now();
        let ctx = Self::build_with_options(file, format, options)?;
        log::debug!("[cache] miss: built in {:.2?}", t.elapsed());

        if let Err(e) = try_save(cache_path, &header, &ctx) {
            log::debug!("[cache] save failed ({}): {e}", cache_path.display());
        } else {
            log::debug!("[cache] wrote: {}", cache_path.display());
        }

        Ok(ctx)
    }

    fn from_snapshot(snap: PipelineSnapshot) -> Self {
        PipelineContext {
            all_ir: snap.all_ir,
            registry: snap.registry,
            closure_ctx: snap.closure_ctx,
            global_analysis: snap.global_analysis,
            inline_bodies: Arc::new(snap.inline_bodies),
            worklet_sources: snap.worklet_sources,
        }
    }

    fn to_snapshot(&self) -> PipelineSnapshot {
        PipelineSnapshot {
            all_ir: self.all_ir.clone(),
            registry: self.registry.clone(),
            closure_ctx: self.closure_ctx.clone(),
            global_analysis: self.global_analysis.clone(),
            inline_bodies: (*self.inline_bodies).clone(),
            worklet_sources: self.worklet_sources.clone(),
        }
    }
}

fn try_load(path: &Path, want: &CacheHeader) -> Option<PipelineContext> {
    let f = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(f);
    let got: CacheHeader = rmp_serde::decode::from_read(&mut reader).ok()?;
    if !got.matches(want) {
        return None;
    }
    let snap: PipelineSnapshot = rmp_serde::decode::from_read(&mut reader).ok()?;
    Some(PipelineContext::from_snapshot(snap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::metro::registry::{FactoryRoles, MetroModule};

    #[test]
    fn cache_path_appends_extension() {
        let p = default_cache_path(Path::new("/tmp/app.android.bundle"));
        assert_eq!(p, PathBuf::from("/tmp/app.android.bundle.hdcache"));
    }

    #[test]
    fn snapshot_roundtrip_preserves_registry_and_ir() {
        // Minimal but non-empty context.
        let mut registry = MetroRegistry::new();
        registry.modules.insert(
            7,
            MetroModule {
                module_id: 7,
                function_id: 42,
                name: Some("login".into()),
                dependencies: vec![1, 2],
                exports: std::collections::HashMap::from([("default".to_string(), 42)]),
                roles: FactoryRoles::from_param_count(7),
            },
        );
        let mut all_ir = BTreeMap::new();
        all_ir.insert(42u32, vec![crate::ir::Statement::Return(None)]);

        let ctx = PipelineContext {
            all_ir,
            registry,
            closure_ctx: None,
            global_analysis: GlobalAnalysis::new(),
            inline_bodies: Arc::new(BTreeMap::from([(42u32, "body".to_string())])),
            worklet_sources: BTreeMap::new(),
        };

        // Serialize the snapshot and read it back.
        let bytes = rmp_serde::to_vec(&ctx.to_snapshot()).expect("serialize");
        let snap: PipelineSnapshot = rmp_serde::from_slice(&bytes).expect("deserialize");
        let restored = PipelineContext::from_snapshot(snap);

        let m = restored.registry.modules.get(&7).expect("module preserved");
        assert_eq!(m.name.as_deref(), Some("login"));
        assert_eq!(m.dependencies, vec![1, 2]);
        assert_eq!(m.roles.module_idx, 4); // modern 7-param layout survived
        assert_eq!(m.exports.get("default"), Some(&42));
        assert!(restored.all_ir.contains_key(&42));
        assert_eq!(restored.inline_bodies.get(&42).map(String::as_str), Some("body"));
    }

    #[test]
    fn stale_header_is_rejected() {
        let want = CacheHeader {
            magic: MAGIC,
            cache_version: CACHE_VERSION,
            file_hash: [1u8; 32],
            binary_hash: [2u8; 32],
            options_key: 0,
        };
        // Same file must match itself.
        assert!(want.matches(&want));
        // A different bytecode hash must be rejected (stale, not a cache hit).
        let other_file = CacheHeader {
            file_hash: [9u8; 32],
            ..want
        };
        assert!(!want.matches(&other_file));
        // A different decompiler binary must be rejected (pipeline output change).
        let other_binary = CacheHeader {
            binary_hash: [3u8; 32],
            ..want
        };
        assert!(!want.matches(&other_binary));
        // A different cache-format version must be rejected.
        let other_version = CacheHeader {
            cache_version: CACHE_VERSION + 1,
            ..want
        };
        assert!(!want.matches(&other_version));
        // Different output-affecting options must be rejected.
        let other_opts = CacheHeader {
            options_key: 1,
            ..want
        };
        assert!(!want.matches(&other_opts));
    }

    #[test]
    fn binary_fingerprint_is_stable_within_process() {
        let a = binary_fingerprint();
        let b = binary_fingerprint();
        assert_eq!(a, b);
        // Real exe hash is almost certainly not the all-0xFF sentinel.
        // (In exotic environments where current_exe fails, both are the sentinel.)
        assert_eq!(a.len(), 32);
    }
}

fn try_save(path: &Path, header: &CacheHeader, ctx: &PipelineContext) -> std::io::Result<()> {
    // Write to a temp file then rename, so a concurrent reader never sees a
    // half-written cache.
    let tmp = path.with_extension("hdcache.tmp");
    {
        let f = std::fs::File::create(&tmp)?;
        let mut writer = BufWriter::new(f);
        let map_err = |e: rmp_serde::encode::Error| std::io::Error::other(e.to_string());
        rmp_serde::encode::write(&mut writer, header).map_err(map_err)?;
        rmp_serde::encode::write(&mut writer, &ctx.to_snapshot()).map_err(map_err)?;
        use std::io::Write;
        writer.flush()?;
    }
    std::fs::rename(&tmp, path)
}
