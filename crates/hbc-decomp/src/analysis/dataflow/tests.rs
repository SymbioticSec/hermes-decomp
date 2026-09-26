use super::reaching_bindings::{Defs, Extract, ReachingDefinitions};
use super::{solve, solve_observed};
use crate::ir::{Constant, Expression, Statement, VarKind};

// A definition is summarised by the integer constant it is assigned, so a test
// can state exactly which value reaches a point. Anything else is a write the
// extractor cannot summarise, which the lattice must treat as unknown.
fn extractor() -> Box<Extract<i32>> {
    Box::new(|stmt: &Statement, _: &Defs<i32>| match stmt {
        Statement::Let { name, value, .. }
        | Statement::Assign {
            target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(name)),
            value,
        } => match value {
            Expression::Value(crate::ir::Value::Constant(Constant::Integer(i))) => {
                Some((name.clone(), Some(*i)))
            }
            _ => Some((name.clone(), None)),
        },
        _ => None,
    })
}

fn run(body: &[Statement]) -> Defs<i32> {
    let extract = extractor();
    let analysis = ReachingDefinitions::new(extract.as_ref());
    solve(&analysis, body, Defs::new())
}

fn let_int(name: &str, value: i32) -> Statement {
    Statement::Let {
        name: name.to_string(),
        value: Expression::constant(Constant::Integer(value)),
        kind: VarKind::Let,
    }
}

fn assign_int(name: &str, value: i32) -> Statement {
    Statement::Assign {
        target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(name.to_string())),
        value: Expression::constant(Constant::Integer(value)),
    }
}

fn truthy() -> Expression {
    Expression::constant(Constant::Bool(true))
}

#[test]
fn a_straight_line_keeps_the_last_definition() {
    let out = run(&[let_int("x", 1), assign_int("x", 2)]);
    assert_eq!(out.get("x"), Some(&2));
}

#[test]
fn the_two_arms_of_an_if_disagreeing_make_the_name_unknown() {
    let out = run(&[Statement::If {
        condition: truthy(),
        then_body: vec![let_int("x", 1)],
        else_body: vec![let_int("x", 2)],
    }]);
    assert_eq!(
        out.get("x"),
        None,
        "two values reach, neither is the answer"
    );
    assert!(out.is_ambiguous("x"));
}

#[test]
fn the_two_arms_of_an_if_agreeing_keep_the_definition() {
    let out = run(&[Statement::If {
        condition: truthy(),
        then_body: vec![let_int("x", 7)],
        else_body: vec![let_int("x", 7)],
    }]);
    assert_eq!(out.get("x"), Some(&7));
}

// A path that defines nothing supplies no competing definition, so the one made on
// the other arm is still the only one that can reach here. Reading this as unknown
// would cost the call graph every binding introduced inside a branch.
#[test]
fn a_definition_on_one_arm_only_still_reaches() {
    let out = run(&[Statement::If {
        condition: truthy(),
        then_body: vec![let_int("x", 1)],
        else_body: vec![],
    }]);
    assert_eq!(out.get("x"), Some(&1));
}

#[test]
fn two_arms_defining_a_name_differently_is_what_makes_it_unknown() {
    let out = run(&[Statement::If {
        condition: truthy(),
        then_body: vec![let_int("x", 1)],
        else_body: vec![let_int("x", 2)],
    }]);
    assert!(out.is_ambiguous("x"));
}

// The tree walk this replaces never entered a loop body, so a definition made
// there was invisible and one made before it survived untouched.
#[test]
fn a_write_inside_a_loop_invalidates_the_definition_before_it() {
    let out = run(&[
        let_int("x", 1),
        Statement::While {
            condition: truthy(),
            body: vec![assign_int("x", 2)],
        },
    ]);
    assert_eq!(out.get("x"), None, "the loop may or may not have run");
    assert!(out.is_ambiguous("x"));
}

#[test]
fn a_loop_that_always_rewrites_the_same_value_stays_known() {
    let out = run(&[
        let_int("x", 5),
        Statement::While {
            condition: truthy(),
            body: vec![assign_int("x", 5)],
        },
    ]);
    assert_eq!(out.get("x"), Some(&5));
}

#[test]
fn a_do_body_always_runs_so_its_definition_holds_after_it() {
    let out = run(&[
        let_int("x", 1),
        Statement::DoWhile {
            body: vec![assign_int("x", 2)],
            condition: truthy(),
        },
    ]);
    assert_eq!(out.get("x"), Some(&2));
}

#[test]
fn a_definition_inside_a_switch_case_reaches_the_exit() {
    let out = run(&[Statement::Switch {
        discriminant: truthy(),
        cases: vec![(
            Expression::constant(Constant::Integer(0)),
            vec![let_int("x", 9)],
        )],
        default: Some(vec![let_int("x", 9)]),
    }]);
    assert_eq!(out.get("x"), Some(&9));
}

#[test]
fn a_switch_without_a_default_may_match_nothing() {
    let out = run(&[Statement::Switch {
        discriminant: truthy(),
        cases: vec![(
            Expression::constant(Constant::Integer(0)),
            vec![let_int("x", 9)],
        )],
        default: None,
    }]);
    assert_eq!(out.get("x"), Some(&9), "the only definition that can reach");
}

#[test]
fn a_catch_param_is_bound_but_carries_no_known_value() {
    let out = run(&[Statement::TryCatch {
        try_body: vec![let_int("x", 1)],
        catch_param: Some("err".to_string()),
        catch_body: vec![],
        finally_body: vec![],
    }]);
    assert!(
        out.is_ambiguous("err"),
        "err exists, its value is not known"
    );
    assert_eq!(
        out.get("x"),
        Some(&1),
        "the try body holds the only definition"
    );
}

#[test]
fn a_for_of_head_binds_its_variable() {
    let out = run(&[Statement::ForOf {
        variable: "item".to_string(),
        iterable: truthy(),
        body: vec![],
    }]);
    assert!(out.is_ambiguous("item"));
}

// A path that returns cannot be observed afterwards, so what it defined must not
// be joined back into the fact that follows.
#[test]
fn a_returning_arm_does_not_contribute_to_what_follows() {
    let out = run(&[Statement::If {
        condition: truthy(),
        then_body: vec![let_int("x", 1), Statement::Return(None)],
        else_body: vec![let_int("x", 2)],
    }]);
    assert_eq!(out.get("x"), Some(&2), "only the else arm reaches here");
}

#[test]
fn both_arms_returning_leaves_nothing_reachable() {
    let out = run(&[
        let_int("y", 3),
        Statement::If {
            condition: truthy(),
            then_body: vec![Statement::Return(None)],
            else_body: vec![Statement::Throw(truthy())],
        },
        assign_int("y", 4),
    ]);
    assert_eq!(out.get("y"), Some(&3), "the trailing write is unreachable");
}

#[test]
fn a_write_the_extractor_cannot_summarise_invalidates_the_name() {
    let out = run(&[
        let_int("x", 1),
        Statement::Assign {
            target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable("x".into())),
            value: Expression::constant(Constant::Undefined),
        },
    ]);
    assert_eq!(out.get("x"), None);
    assert!(out.is_ambiguous("x"));
}

// The observed walk reports the fact on entry to each statement, once, with the
// loop already settled so the reported fact holds on every iteration.
#[test]
fn observation_reports_the_settled_fact_inside_a_loop() {
    let body = vec![
        let_int("x", 1),
        Statement::While {
            condition: truthy(),
            body: vec![assign_int("x", 2), Statement::Expr(truthy())],
        },
    ];
    let extract = extractor();
    let analysis = ReachingDefinitions::new(extract.as_ref());
    let mut seen: Vec<Option<i32>> = Vec::new();
    solve_observed(&analysis, &body, Defs::new(), &mut |stmt, fact| {
        if matches!(stmt, Statement::Expr(_)) {
            seen.push(fact.get("x").copied());
        }
    });
    assert_eq!(
        seen,
        vec![Some(2)],
        "the statement after the loop write sees 2, reported exactly once"
    );
}

#[test]
fn observation_visits_a_statement_in_each_arm_of_an_if() {
    let body = vec![Statement::If {
        condition: truthy(),
        then_body: vec![Statement::Comment("a".into())],
        else_body: vec![Statement::Comment("b".into())],
    }];
    let extract = extractor();
    let analysis = ReachingDefinitions::new(extract.as_ref());
    let mut seen = Vec::new();
    solve_observed(&analysis, &body, Defs::new(), &mut |stmt, _| {
        if let Statement::Comment(c) = stmt {
            seen.push(c.clone());
        }
    });
    assert_eq!(seen, vec!["a".to_string(), "b".to_string()]);
}

// Structure recovery emits statements that cannot run, typically after an if whose
// arms both jump. A call sitting there still tells a naming client what the binary
// passes to that function, so the walk has to report it even though no fact it
// produces may be believed.
#[test]
fn a_statement_in_unreachable_code_is_still_reported() {
    let body = vec![
        let_int("x", 1),
        Statement::If {
            condition: truthy(),
            then_body: vec![Statement::Break(None)],
            else_body: vec![Statement::Continue(None)],
        },
        Statement::Comment("dead".into()),
        assign_int("x", 2),
        Statement::Comment("also dead".into()),
    ];
    let extract = extractor();
    let analysis = ReachingDefinitions::new(extract.as_ref());
    let mut seen = Vec::new();
    let out = solve_observed(&analysis, &body, Defs::new(), &mut |stmt, fact| {
        if let Statement::Comment(c) = stmt {
            seen.push((c.clone(), fact.get("x").copied()));
        }
    });
    assert_eq!(
        seen,
        vec![
            ("dead".to_string(), Some(1)),
            ("also dead".to_string(), Some(1)),
        ],
        "both unreachable statements are reported, with the last fact that held"
    );
    assert_eq!(
        out.get("x"),
        Some(&1),
        "the unreachable write must not reach the end of the body"
    );
}
