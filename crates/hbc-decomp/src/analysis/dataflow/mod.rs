// A dataflow engine over the structured IR.
//
// Structure recovery turns the CFG back into if/while/for/try statements and the
// CFG is dropped, yet every analysis that wants a flow sensitive answer (which
// definition reaches this call, is this name bound at this point) runs after that
// happens. So the engine walks the structured tree and rebuilds the joins the
// control flow implies: the two arms of an `if` meet after it, a loop body is
// iterated to a fixed point, a catch clause is reachable from anywhere inside its
// try, and a path that returns contributes nothing to what follows.
//
// An analysis supplies a lattice and a transfer function. `solve` answers with the
// fact that holds after a body, `solve_observed` also reports the fact holding on
// entry to each statement, which is the form a client querying a program point
// needs.

use crate::ir::Statement;

// Bound on how many times a loop body is re-entered before the engine accepts the
// current fact. Every lattice used here has finite height so the fixed point is
// reached well inside this, but a transfer function is client code and the engine
// must terminate regardless.
pub const MAX_LOOP_ITERATIONS: usize = 16;

// A join semilattice. `join` merges `other` into `self` and reports whether `self`
// changed, which is what drives every fixed point in this module. An analysis
// whose natural combination is intersection implements `join` as intersection: the
// engine only ever asks for "combine these two paths".
pub trait Fact: Clone {
    fn join(&mut self, other: &Self) -> bool;
}

pub trait Analysis {
    type Fact: Fact;

    // Control has passed one statement that is not itself a control structure.
    // Control structures are decomposed by the engine, which calls this only for
    // their leaves, so an implementation never has to recurse.
    fn transfer(&self, stmt: &Statement, fact: &mut Self::Fact);

    // A control structure introduces a name for the extent of its body: the
    // variable of a `for (x of ...)` or `for (x in ...)`, a catch parameter.
    fn declare(&self, _name: &str, _fact: &mut Self::Fact) {}
}

// A fact plus whether the path carrying it is still reachable. A path that has
// returned, thrown, broken or continued stops contributing to what follows, so
// joining it back in would claim things hold that cannot be observed.
#[derive(Clone)]
struct State<F> {
    fact: F,
    live: bool,
}

impl<F: Fact> State<F> {
    fn live(fact: F) -> Self {
        State { fact, live: true }
    }

    // Merge another path into this one. A dead path carries nothing, so it never
    // widens a live one, and two dead paths stay dead.
    fn join(&mut self, other: &Self) -> bool {
        match (self.live, other.live) {
            (_, false) => false,
            (false, true) => {
                self.fact = other.fact.clone();
                self.live = true;
                true
            }
            (true, true) => self.fact.join(&other.fact),
        }
    }
}

type Observer<'o, F> = dyn FnMut(&Statement, &F) + 'o;

// The fact holding after `body` has run, starting from `entry`.
pub fn solve<A: Analysis>(analysis: &A, body: &[Statement], entry: A::Fact) -> A::Fact {
    let mut state = State::live(entry);
    run_body(analysis, body, &mut state, &mut None);
    state.fact
}

// As `solve`, and additionally report the fact holding on entry to each statement,
// nested statements included. Loops are settled to their fixed point before the
// body is walked for observation, so each statement is reported once, with the
// fact that holds on every iteration rather than only the first.
pub fn solve_observed<A: Analysis>(
    analysis: &A,
    body: &[Statement],
    entry: A::Fact,
    observe: &mut dyn FnMut(&Statement, &A::Fact),
) -> A::Fact {
    let mut state = State::live(entry);
    let mut obs: Option<&mut Observer<'_, A::Fact>> = Some(observe);
    run_body(analysis, body, &mut state, &mut obs);
    state.fact
}

fn run_body<A: Analysis>(
    analysis: &A,
    body: &[Statement],
    state: &mut State<A::Fact>,
    observe: &mut Option<&mut Observer<'_, A::Fact>>,
) {
    for stmt in body {
        if let Some(f) = observe.as_mut() {
            f(stmt, &state.fact);
        }
        if state.live {
            run_stmt(analysis, stmt, state, observe);
        } else {
            // The walk carries on so the statement and everything nested in it are
            // still reported, and the fact is put back so nothing unreachable can
            // claim to hold. Reachability decides what a fact may say, not what the
            // binary contains: a call sitting in dead code still names its
            // arguments, and a client harvesting evidence has to be shown it.
            let frozen = state.fact.clone();
            run_stmt(analysis, stmt, state, observe);
            state.fact = frozen;
            state.live = false;
        }
    }
}

// Settle a loop body: the fact on entering the body is the entry fact joined with
// everything the body itself produces, iterated until it stops growing.
fn settle_loop<A: Analysis>(
    analysis: &A,
    parts: &[&[Statement]],
    entry: &State<A::Fact>,
) -> State<A::Fact> {
    let mut head = entry.clone();
    for _ in 0..MAX_LOOP_ITERATIONS {
        let mut walked = head.clone();
        for part in parts {
            run_body(analysis, part, &mut walked, &mut None);
        }
        if !head.join(&walked) {
            break;
        }
    }
    head
}

fn run_stmt<A: Analysis>(
    analysis: &A,
    stmt: &Statement,
    state: &mut State<A::Fact>,
    observe: &mut Option<&mut Observer<'_, A::Fact>>,
) {
    match stmt {
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            let mut then_state = state.clone();
            run_body(analysis, then_body, &mut then_state, observe);
            let mut else_state = state.clone();
            run_body(analysis, else_body, &mut else_state, observe);
            then_state.join(&else_state);
            *state = then_state;
        }

        Statement::Block(inner) => {
            run_body(analysis, inner, state, observe);
        }

        // A `while` may run its body zero times, so what holds after it is the
        // settled head: the entry fact joined with every iteration's output.
        Statement::While { body, .. } => {
            let settled = settle_loop(analysis, &[body], state);
            let mut walked = settled.clone();
            run_body(analysis, body, &mut walked, observe);
            *state = settled;
        }

        // A `do` body always runs, so what holds after it is the body's output.
        Statement::DoWhile { body, .. } => {
            let settled = settle_loop(analysis, &[body], state);
            let mut walked = settled;
            run_body(analysis, body, &mut walked, observe);
            walked.live = walked.live || state.live;
            *state = walked;
        }

        Statement::For {
            init, update, body, ..
        } => {
            if let Some(init) = init {
                if let Some(f) = observe.as_mut() {
                    f(init, &state.fact);
                }
                run_stmt(analysis, init, state, observe);
            }
            let update_slice: Vec<Statement> = update
                .as_ref()
                .map(|u| vec![(**u).clone()])
                .unwrap_or_default();
            let settled = settle_loop(analysis, &[body, &update_slice], state);
            let mut walked = settled.clone();
            run_body(analysis, body, &mut walked, observe);
            if let Some(update) = update {
                if walked.live {
                    if let Some(f) = observe.as_mut() {
                        f(update, &walked.fact);
                    }
                    run_stmt(analysis, update, &mut walked, observe);
                }
            }
            *state = settled;
        }

        Statement::ForOf { variable, body, .. } | Statement::ForIn { variable, body, .. } => {
            let mut head = state.clone();
            analysis.declare(variable, &mut head.fact);
            let settled = settle_loop(analysis, &[body], &head);
            let mut walked = settled.clone();
            run_body(analysis, body, &mut walked, observe);
            // The iterable may be empty, so the loop variable is not guaranteed to
            // be observable after the loop, but it is bound by the head either way.
            *state = settled;
        }

        Statement::Switch { cases, default, .. } => {
            // A case is entered either by matching the discriminant, from the fact
            // before the switch, or by falling out of the case above it.
            let before = state.clone();
            let mut fallthrough = State {
                fact: before.fact.clone(),
                live: false,
            };
            let mut exits: Option<State<A::Fact>> = None;
            for (_, case_body) in cases {
                let mut case_state = before.clone();
                case_state.join(&fallthrough);
                run_body(analysis, case_body, &mut case_state, observe);
                match exits.as_mut() {
                    Some(acc) => {
                        acc.join(&case_state);
                    }
                    None => exits = Some(case_state.clone()),
                }
                fallthrough = case_state;
            }
            if let Some(default_body) = default {
                let mut default_state = before.clone();
                default_state.join(&fallthrough);
                run_body(analysis, default_body, &mut default_state, observe);
                match exits.as_mut() {
                    Some(acc) => {
                        acc.join(&default_state);
                    }
                    None => exits = Some(default_state),
                }
            } else {
                // With no default the discriminant may match nothing at all.
                match exits.as_mut() {
                    Some(acc) => {
                        acc.join(&before);
                    }
                    None => exits = Some(before.clone()),
                }
            }
            if let Some(exit) = exits {
                *state = exit;
            }
        }

        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => {
            let before = state.clone();
            let mut try_state = before.clone();
            run_body(analysis, try_body, &mut try_state, observe);

            // The throw can happen anywhere in the try, so the catch is reachable
            // from every point inside it. Joining the state before the try with the
            // one after covers both ends without pretending to know where it threw.
            let mut catch_state = before.clone();
            catch_state.join(&try_state);
            if let Some(name) = catch_param {
                analysis.declare(name, &mut catch_state.fact);
            }
            run_body(analysis, catch_body, &mut catch_state, observe);

            let mut after = try_state;
            after.join(&catch_state);
            run_body(analysis, finally_body, &mut after, observe);
            *state = after;
        }

        // A class body is a nested scope that does not run here, so its methods do
        // not carry this function's control flow. The statement itself still binds
        // the class name, which is what `transfer` sees.
        Statement::Class { .. } => {
            analysis.transfer(stmt, &mut state.fact);
        }

        Statement::Return(_) | Statement::Throw(_) => {
            analysis.transfer(stmt, &mut state.fact);
            state.live = false;
        }

        Statement::Break(_) | Statement::Continue(_) => {
            state.live = false;
        }

        _ => analysis.transfer(stmt, &mut state.fact),
    }
}

pub mod reaching_bindings;

#[cfg(test)]
mod tests;
