mod for_of;
mod for_in;
mod for_loop;
mod guarded_dowhile;
mod while_true;

pub use for_of::{detect_for_of_loops, detect_legacy_for_of};
pub use for_in::detect_for_in_loops;
pub use for_loop::detect_for_loops;
pub use guarded_dowhile::fold_guarded_loops;
pub use while_true::convert_while_true_loops;
