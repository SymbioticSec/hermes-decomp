// Reaching definitions over the structured IR, keyed by binding.
//
// The question a caller asks is "at this point, what is this name defined as".
// The answer is only usable when exactly one definition reaches, so the lattice
// carries that distinction directly: a name reached by two different definitions
// becomes `Many` and the caller is told it does not know, rather than being handed
// whichever one a tree walk happened to visit last.
//
// A write whose value cannot be summarised also lands on `Many`. That is the point
// of doing this over the flow: `x = require(4)` inside a loop that later does
// `x = something_else` must not leave `x` looking like module 4.

use super::{Analysis, Fact};
use crate::ir::Statement;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach<P> {
    // Exactly one definition reaches this point.
    One(P),
    // Two or more definitions reach, or one whose value could not be summarised.
    Many,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defs<P>(pub BTreeMap<String, Reach<P>>);

impl<P> Default for Defs<P> {
    fn default() -> Self {
        Defs(BTreeMap::new())
    }
}

impl<P: Clone + PartialEq> Defs<P> {
    pub fn new() -> Self {
        Self::default()
    }

    // The single definition of `key` reaching here, or `None` when the name is
    // undefined at this point or carries more than one definition.
    pub fn get(&self, key: &str) -> Option<&P> {
        match self.0.get(key) {
            Some(Reach::One(p)) => Some(p),
            _ => None,
        }
    }

    pub fn is_ambiguous(&self, key: &str) -> bool {
        matches!(self.0.get(key), Some(Reach::Many))
    }

    fn define(&mut self, key: String, payload: Option<P>) {
        let entry = match payload {
            Some(p) => Reach::One(p),
            None => Reach::Many,
        };
        self.0.insert(key, entry);
    }
}

impl<P: Clone + PartialEq> Fact for Defs<P> {
    // The question this domain answers is "of the definitions that can reach this
    // point, is there exactly one". A path that defines nothing contributes no
    // definition, so it neither supplies nor destroys one: an entry present on one
    // side and absent on the other survives. Only two different definitions
    // actually reaching the same point make the answer `Many`.
    //
    // That is deliberately a may-reach domain rather than a must-be-defined one.
    // Asking whether a name is definitely bound is a different question, and
    // answering it here would report every branch local definition as unknown and
    // cost the call graph most of its edges.
    fn join(&mut self, other: &Self) -> bool {
        let mut changed = false;
        for (key, theirs) in &other.0 {
            match self.0.get(key) {
                Some(ours) if ours == theirs => {}
                Some(Reach::Many) => {}
                Some(Reach::One(_)) => {
                    self.0.insert(key.clone(), Reach::Many);
                    changed = true;
                }
                None => {
                    self.0.insert(key.clone(), theirs.clone());
                    changed = true;
                }
            }
        }
        changed
    }
}

// What a statement does to the set of definitions. `Some((key, Some(payload)))`
// records a definition the caller could summarise, `Some((key, None))` a write it
// could not, and `None` a statement that defines nothing.
//
// The extractor is handed the definitions in force at that point, because a
// summary usually depends on them: `y = x.default` only means a module when `x`
// is already known to be one.
pub type Extract<P> = dyn Fn(&Statement, &Defs<P>) -> Option<(String, Option<P>)>;

pub struct ReachingDefinitions<'e, P> {
    extract: &'e Extract<P>,
}

impl<'e, P> ReachingDefinitions<'e, P> {
    pub fn new(extract: &'e Extract<P>) -> Self {
        ReachingDefinitions { extract }
    }
}

impl<P: Clone + PartialEq> Analysis for ReachingDefinitions<'_, P> {
    type Fact = Defs<P>;

    fn transfer(&self, stmt: &Statement, fact: &mut Self::Fact) {
        if let Some((key, payload)) = (self.extract)(stmt, fact) {
            fact.define(key, payload);
        }
    }

    // A loop or catch head binds its name to a value this analysis cannot see, so
    // it is known to exist and known not to be summarisable.
    fn declare(&self, name: &str, fact: &mut Self::Fact) {
        fact.define(name.to_string(), None);
    }
}
