// Propagate async flags along generator wrapper chains.
use super::ClosureContext;

const MAX_ASYNC_PROPAGATION_ITERATIONS: usize = 20;

impl ClosureContext {
    pub fn propagate_async_to_generators(&mut self) {
        // Iterate until no more changes (handles multi-level chains)
        for _ in 0..MAX_ASYNC_PROPAGATION_ITERATIONS {
            let async_generators: Vec<u32> = self
                .generator_functions
                .iter()
                .filter(|&&func_id| {
                    if self.async_functions.contains(&func_id) {
                        return false; // already marked
                    }
                    // Inner body of an async wrapper: parent is async.
                    matches!(self.parent_function.get(&func_id), Some(&parent) if self.async_functions.contains(&parent))
                })
                .copied()
                .collect();

            if async_generators.is_empty() {
                break;
            }
            for func_id in async_generators {
                self.async_functions.insert(func_id);
            }
        }
    }
}
