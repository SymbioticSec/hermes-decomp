// Functions that are referenced by id but never made it into `all_ir` (IR
// generation failed, or the body was dropped) render as `{ ... }`. Rebuild
// each one from bytecode and render it against the bodies we already have.
// A factory is skipped: its source is already the module itself. A body over
// the size cap is skipped too, so one giant object copied into a thousand
// schemas cannot multiply the output.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::opcode::BytecodeFormat;
use crate::pipeline::get_function_params;
use crate::pipeline::{generate_ir, DecompileOptionsV2};

use super::PipelineContext;

const MAX_FILLED_BODY_CHARS: usize = 48_000;
const MAX_FILLED_FUNCTIONS: usize = 4_096;

pub(super) fn ids_referenced_but_missing(
    prepared: &BTreeMap<u32, (Vec<String>, Vec<Statement>)>,
    have: &BTreeMap<u32, String>,
    factories: &BTreeMap<u32, u32>,
) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    for (_, stmts) in prepared.values() {
        let mut refs = Vec::new();
        super::collect_function_refs(stmts, &mut refs);
        for id in refs {
            if !have.contains_key(&id) && !factories.contains_key(&id) && seen.insert(id) {
                ids.push(id);
            }
        }
    }
    if ids.len() > MAX_FILLED_FUNCTIONS {
        log::debug!(
            "[pipeline] inline bodies: {} missing ids, only {} rebuilt",
            ids.len(),
            MAX_FILLED_FUNCTIONS
        );
    }
    ids.truncate(MAX_FILLED_FUNCTIONS);
    ids
}

impl PipelineContext {
    pub(super) fn render_missing_from_bytecode(
        &self,
        file: &BytecodeFile,
        missing: &[u32],
        have: &BTreeMap<u32, String>,
    ) -> BTreeMap<u32, String> {
        let Ok(format) = BytecodeFormat::for_version(file.header.version) else {
            return BTreeMap::new();
        };
        let options = DecompileOptionsV2 {
            resolve_strings: true,
            ..DecompileOptionsV2::default()
        };
        let mut inline = have.clone();
        let mut out = BTreeMap::new();
        // Inner functions are missing too. Each pass renders against what the
        // previous pass just recovered, so a method body exists before the
        // object that stores it. The map is snapshotted once per pass.
        for _ in 0..4 {
            let map = Arc::new(inline.clone());
            let mut gained = Vec::new();
            for &id in missing {
                // Factories are referenced too. A real module body is far past
                // the size cap below and is left as a hole. A small function
                // that was registered as a module still has a body worth inlining.
                if gained.iter().any(|(i, _)| *i == id) {
                    continue;
                }
                let Ok(stmts) = generate_ir(
                    file,
                    &format,
                    id,
                    &options,
                    self.closure_ctx.as_ref(),
                    false,
                ) else {
                    continue;
                };
                let params = get_function_params(file, id);
                let rendered = self.render_one_body(file, id, &params, &stmts, &map);
                if rendered.len() > MAX_FILLED_BODY_CHARS || hole_only(&rendered) {
                    continue;
                }
                let prev = map
                    .get(&id)
                    .map(|s| s.matches(crate::transforms::BODY_HOLE).count())
                    .unwrap_or(usize::MAX);
                if rendered.matches(crate::transforms::BODY_HOLE).count() < prev {
                    gained.push((id, rendered));
                }
            }
            if gained.is_empty() {
                break;
            }
            for (id, rendered) in gained {
                inline.insert(id, rendered.clone());
                out.insert(id, rendered);
            }
        }
        out
    }
}

fn hole_only(rendered: &str) -> bool {
    let holes = rendered.matches(crate::transforms::BODY_HOLE).count();
    holes == 1 && rendered.trim().ends_with("*/ }") && rendered.len() < 80
}
