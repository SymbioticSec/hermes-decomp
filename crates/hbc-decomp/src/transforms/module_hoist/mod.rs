// Hoist eager inline module loads into a single module-level binding.
//
// On a modern Metro bundle the loader (`require`, `importDefault`, `importAll`, or
// a register assigned one of them) is called at every use site, so a dependency
// shows up many times as `importDefault(530).foo()`. This pass gives each eagerly
// used (module id, loader kind) pair one factory-top binding (ESM → import) and
// rewrites eager occurrences to that binding.
//
// Conservative rules:
//   * Absolute id unifies `loader(1234)` and `loader(dependencyMap[i])`.
//   * Member indices resolve only for known dep-map objects.
//   * require / importDefault / importAll of the same id stay distinct.
//   * Existing factory-top bindings are reused.
//   * Lazy RN getters are left alone; an id is hoisted only if never used lazily.
//   * The exact loader callee is preserved on inject.

mod detect;
mod hoist;
mod kinds;
mod lazy;
mod names;

#[cfg(test)]
mod tests;

pub use hoist::hoist_module_loaders;
