/// Which Metro loader helper performed the call. Kept distinct so
/// `require(N)` and `importDefault(N)` never share one binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum LoaderKind {
    Require,
    ImportDefault,
    ImportAll,
}

impl LoaderKind {
    pub(super) fn from_seed(name: &str) -> Option<Self> {
        match name {
            "require" => Some(Self::Require),
            "importDefault" => Some(Self::ImportDefault),
            "importAll" => Some(Self::ImportAll),
            _ => None,
        }
    }
}

pub(super) type HoistKey = (u32, LoaderKind);

pub(super) const LOADER_NAMES: &[&str] = &["require", "importDefault", "importAll"];

// Reserved so injected bindings never shadow Metro factory roles or loaders.
pub(super) const RESERVED_BINDINGS: &[&str] = &[
    "require",
    "importDefault",
    "importAll",
    "module",
    "exports",
    "global",
    "dependencyMap",
    "default",
    "undefined",
    "arguments",
    "eval",
];
