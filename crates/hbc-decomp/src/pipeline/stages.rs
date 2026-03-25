// Pipeline Stage Documentation
//
// This module documents the ordering and dependencies of all pipeline stages.
// It is not executable code — only documentation to prevent silent reordering bugs.
//
// ============================================================================
// WHOLE-PROGRAM STAGES (PipelineContext::build_with_options)
// ============================================================================
//
// STAGE W1: Closure Context Build
//   - Builds parent/child function relationships from raw IR.
//   - Marks async/generator functions from CreateAsyncClosure/CreateGeneratorClosure.
//   - REQUIRES: nothing (first stage)
//   - OUTPUT: ClosureContext
//
// STAGE W2: Metro Detection
//   - Scans global function for __d() calls to populate MetroRegistry.
//   - Uses raw (un-optimized) IR to avoid pattern destruction.
//   - REQUIRES: nothing (independent of W1)
//   - OUTPUT: MetroRegistry
//
// STAGE W3: Optimized IR Generation (parallel)
//   - Builds per-function IR with all transforms (SSA, propagation, structure, etc.).
//   - Applies register naming and semantic variable naming.
//   - REQUIRES: W1 (closure context for cross-function resolution)
//   - OUTPUT: BTreeMap<u32, Vec<Statement>> (all_ir)
//
// STAGE W4: Closure Analyze + Insert
//   - Analyzes optimized IR to update closure context with new definitions.
//   - Propagates async flags to generators.
//   - REQUIRES: W3 (optimized IR)
//   - OUTPUT: updated ClosureContext
//
// STAGE W5: Module Name Propagation
//   - Sub-phases:
//     W5a: Reverse require naming (varName = require(depId) -> name dep module)
//     W5b: Infer names for anonymous modules from exports/body analysis
//     W5c: Re-export propagation (thin wrappers inherit dep name)
//     W5d: Dependency-chain naming (single-dep wrappers)
//     W5e: Propagate module names to closure slots
//     W5f: Propagate module names to require variables
//   - REQUIRES: W3 (all_ir), W2 (MetroRegistry), W4 (ClosureContext)
//   - OUTPUT: MetroRegistry with module names, renamed variables in all_ir
//
// STAGE W6: Closure Resolution (first pass)
//   - Replaces ClosureVar{slot} with named variables using closure context.
//   - REQUIRES: W5 (propagated names in closure context)
//   - OUTPUT: updated all_ir with resolved closure variables
//
// STAGE W7: Metro Export Analysis
//   - Populates MetroModule.exports maps by analyzing factory bodies.
//   - Detects: direct assignment, bulk assignment, Object.defineProperty patterns.
//   - REQUIRES: W6 (resolved closure variables for better analysis)
//   - OUTPUT: MetroRegistry with export maps
//
// STAGE W8: Inter-Procedural Analysis (IPA)
//   - 6-phase parameter name inference across function boundaries.
//   - Uses MetroRegistry exports for callee resolution.
//   - REQUIRES: W7 (export maps for resolution), W3 (all_ir for call site collection)
//   - OUTPUT: GlobalAnalysis (param_names, param_links, call graph, dead code)
//
// STAGE W9: IPA Closure Re-resolve
//   - Updates closure slots with IPA-inferred parameter names.
//   - Re-runs closure resolution with updated names.
//   - REQUIRES: W8 (IPA results)
//   - OUTPUT: updated ClosureContext and all_ir
//
// STAGE W10: Closure Property Naming
//   - Renames closure_N variables based on cross-function property access patterns.
//   - REQUIRES: W9 (re-resolved closures)
//   - OUTPUT: updated all_ir with renamed closure variables
//
// STAGE W11: Closure Definition Naming
//   - Renames closures from their definition sites (function assigned to variable).
//   - REQUIRES: W10 (after property naming to avoid conflicts)
//   - OUTPUT: updated all_ir
//
// STAGE W12: Strip Hermes This
//   - Removes meaningless `this` arguments from Call expressions.
//   - REQUIRES: W11 (all naming complete)
//   - OUTPUT: updated all_ir
//
// STAGE W13: Variable Inlining
//   - Eliminates dead/single-use temporaries (tmp*, closure_*, rN).
//   - REQUIRES: W12 (all naming and cleanup complete)
//   - OUTPUT: cleaner all_ir
//
// STAGE W14: Async Detection + Yield-to-Await
//   - Detects Babel async-to-generator patterns.
//   - Converts Yield expressions to Await in async function bodies.
//   - REQUIRES: W13 (inlined IR for cleaner pattern detection)
//   - OUTPUT: updated all_ir, updated ClosureContext async flags
//
// STAGE W15: Async Wrapper Unwrap
//   - Inlines Babel _asyncToGenerator wrapper bodies into the outer function.
//   - REQUIRES: W14 (async detection complete)
//   - OUTPUT: simplified all_ir
//
// STAGE W16: Post-IPA Transforms
//   - Reserved word renaming, object/array literal folding, arguments simplification.
//   - REQUIRES: W15 (all async transforms done)
//   - OUTPUT: final all_ir
//
// STAGE W17: Inline Body Rendering
//   - Multi-pass pre-rendering of nested function bodies for codegen.
//   - Renders leaves first, then parents that reference them.
//   - REQUIRES: W16 (final all_ir)
//   - OUTPUT: inline_bodies map
//
// ============================================================================
// PER-FUNCTION STAGES (generate_ir in ir_gen.rs)
// ============================================================================
//
// STAGE F1:  IR Build (bytecode -> CFG)
// STAGE F2:  SSA / Live Range Splitting
// STAGE F3:  Copy/Constant Propagation
// STAGE F4:  Expression Simplification
// STAGE F5:  Structure Recovery (CFG -> if/while/for/switch/try)
// STAGE F6:  Statement Optimization (if inversion, ternary detection, dead assign)
// STAGE F7:  Expression Inlining (single-use register elimination)
// STAGE F8:  Logic Transformation
// STAGE F9:  Concatenation Propagation
// STAGE F10: Pattern Detection (string concat, nullish, optional chaining, short-circuit)
// STAGE F11: Class Pattern Detection (ES6 class reconstruction)
// STAGE F12: Object/Array Literal Reconstruction
// STAGE F13: Default Parameter Detection
// STAGE F14: Spread/Rest Operators
// STAGE F15: Destructuring Detection
// STAGE F16: Generator/Async Pattern Detection
// STAGE F17: Yield-to-Await Conversion (async functions)
// STAGE F18: Cleanup (basic + advanced)
// STAGE F19: Chain Access Optimization
// STAGE F20: Ternary Return Optimization
// STAGE F21: Logic Simplification (advanced)
// STAGE F22: CommonJS Export Inference + Name Inference
// STAGE F23: Register Naming (analyze + debug info merge + rename)
// STAGE F24: Semantic Variable Naming
// STAGE F25: Final Simplification
// STAGE F26: Closure Resolution (if context provided)
