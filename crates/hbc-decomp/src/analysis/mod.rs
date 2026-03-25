pub mod closure;
pub mod ipa;
pub mod liveness;
pub mod loops;
pub mod metro;
pub mod naming;
pub mod reaching;
pub mod structure;
pub mod xref;

pub use closure::{resolve_closures, ClosureContext, ClosureInfo, ClosureSlotValue};
pub use ipa::{run_ipa, FunctionNameIndex, GlobalAnalysis};
pub use metro::{DependencyTree, MetroModule, MetroRegistry};
pub use naming::{analyze_registers, generate_name, rename_registers, RegisterInfo, RegisterRole};
pub use structure::{Structure, StructureAnalysis};
pub use xref::{find_function_refs, find_string_xrefs, XrefResult};
