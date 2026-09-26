use super::candidates::extract;
use super::verify::verify;
use super::{Artifact, BundleFingerprint, Proposal, Rejection, Role, ARTIFACT_VERSION};
use crate::ir::{Binding, Constant, Expression, Statement, Value, VarKind};
use std::collections::{BTreeMap, BTreeSet, HashSet};

fn fingerprint() -> BundleFingerprint {
    BundleFingerprint {
        hbc_version: 96,
        function_count: 10,
        string_count: 3,
        string_digest: 42,
    }
}

fn table<'a>(items: &[&'a str]) -> HashSet<&'a str> {
    items.iter().copied().collect()
}

fn string(s: &str) -> Expression {
    Expression::constant(Constant::String(s.to_string()))
}

fn string_array(items: &[&str]) -> Expression {
    Expression::Array {
        elements: items.iter().map(|s| Some(string(s))).collect(),
    }
}

// `function f(i) { return TABLE[i]; }`, the shape a string obfuscator leaves.
fn decoder_body(items: &[&str]) -> Vec<Statement> {
    vec![
        Statement::Let {
            name: "table".to_string(),
            value: string_array(items),
            kind: VarKind::Const,
        },
        Statement::Return(Some(Expression::Member {
            object: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                "table".to_string(),
            )))),
            property: crate::ir::PropertyKey::Computed(Box::new(Expression::Value(
                Value::Parameter(0),
            ))),
            optional: false,
        })),
    ]
}

fn artifact(proposals: Vec<Proposal>) -> Artifact {
    Artifact {
        version: ARTIFACT_VERSION,
        bundle: fingerprint(),
        proposals,
    }
}

fn ir(entries: Vec<(u32, Vec<Statement>)>) -> BTreeMap<u32, Vec<Statement>> {
    entries.into_iter().collect()
}

fn only_rejection(v: &super::VerifiedArtifact) -> Rejection {
    assert_eq!(v.rejected.len(), 1, "expected exactly one rejection");
    v.rejected[0].1.clone()
}

// The check the whole chain rests on: the strings a claimed decoder yields have to
// be strings this binary actually holds.
#[test]
fn a_decoder_whose_strings_are_all_in_the_table_is_confirmed() {
    let all_ir = ir(vec![(7, decoder_body(&["alpha", "beta"]))]);
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".to_string(),
        role: Role::StringDecoder,
    }]);
    let v = verify(
        &art,
        &fingerprint(),
        &table(&["alpha", "beta", "gamma"]),
        &all_ir,
    );
    assert_eq!(v.names.get(&7).map(String::as_str), Some("decodeName"));
    assert_eq!(v.confirmation_rate(), 1.0);
}

#[test]
fn a_decoder_yielding_a_string_the_binary_does_not_hold_is_refused() {
    let all_ir = ir(vec![(7, decoder_body(&["alpha", "not-in-this-build"]))]);
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".to_string(),
        role: Role::StringDecoder,
    }]);
    let v = verify(&art, &fingerprint(), &table(&["alpha", "beta"]), &all_ir);
    assert!(
        v.names.is_empty(),
        "nothing may be renamed on a refused proposal"
    );
    assert_eq!(
        only_rejection(&v),
        Rejection::StringsAbsentFromTable {
            missing: 1,
            checked: 2
        }
    );
}

#[test]
fn a_function_that_is_not_the_claimed_role_is_refused() {
    let all_ir = ir(vec![(7, vec![Statement::Return(Some(string("x")))])]);
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".to_string(),
        role: Role::StringDecoder,
    }]);
    let v = verify(&art, &fingerprint(), &table(&["x"]), &all_ir);
    assert!(matches!(
        only_rejection(&v),
        Rejection::RoleNotConfirmed { .. }
    ));
}

// Function ids are positions in one build. An artifact made for another bundle
// would rename by coincidence, so none of it is applied.
#[test]
fn an_artifact_made_for_another_bundle_is_refused_whole() {
    let all_ir = ir(vec![(7, decoder_body(&["alpha"]))]);
    let mut art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".to_string(),
        role: Role::StringDecoder,
    }]);
    art.bundle.string_digest = 43;
    let v = verify(&art, &fingerprint(), &table(&["alpha"]), &all_ir);
    assert!(v.names.is_empty());
    assert_eq!(only_rejection(&v), Rejection::WrongBundle);
}

#[test]
fn an_artifact_from_a_future_version_is_refused_whole() {
    let all_ir = ir(vec![(7, decoder_body(&["alpha"]))]);
    let mut art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".to_string(),
        role: Role::StringDecoder,
    }]);
    art.version = ARTIFACT_VERSION + 1;
    let v = verify(&art, &fingerprint(), &table(&["alpha"]), &all_ir);
    assert!(v.names.is_empty());
    assert!(matches!(
        only_rejection(&v),
        Rejection::ArtifactVersion { .. }
    ));
}

#[test]
fn a_proposal_for_a_function_this_bundle_does_not_have_is_refused() {
    let all_ir = ir(vec![(7, decoder_body(&["alpha"]))]);
    let art = artifact(vec![Proposal {
        function_id: 999,
        name: "decodeName".to_string(),
        role: Role::StringDecoder,
    }]);
    let v = verify(&art, &fingerprint(), &table(&["alpha"]), &all_ir);
    assert_eq!(only_rejection(&v), Rejection::UnknownFunction);
}

#[test]
fn a_proposed_name_that_is_not_an_identifier_is_refused() {
    let all_ir = ir(vec![(7, decoder_body(&["alpha"]))]);
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "decode name".to_string(),
        role: Role::StringDecoder,
    }]);
    let v = verify(&art, &fingerprint(), &table(&["alpha"]), &all_ir);
    assert_eq!(only_rejection(&v), Rejection::NameNotAnIdentifier);
}

#[test]
fn a_proposed_name_that_is_a_reserved_word_is_refused() {
    let all_ir = ir(vec![(7, decoder_body(&["alpha"]))]);
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "class".to_string(),
        role: Role::StringDecoder,
    }]);
    let v = verify(&art, &fingerprint(), &table(&["alpha"]), &all_ir);
    assert_eq!(only_rejection(&v), Rejection::NameIsReserved);
}

#[test]
fn the_simple_roles_are_each_confirmed_and_each_refused() {
    let noop = vec![Statement::Return(None)];
    let identity = vec![Statement::Return(Some(Expression::Value(
        Value::Parameter(0),
    )))];
    let constant = vec![Statement::Return(Some(string("k")))];
    let all_ir = ir(vec![(1, noop), (2, identity), (3, constant)]);
    let good = artifact(vec![
        Proposal {
            function_id: 1,
            name: "ignore".into(),
            role: Role::Noop,
        },
        Proposal {
            function_id: 2,
            name: "passThrough".into(),
            role: Role::Identity,
        },
        Proposal {
            function_id: 3,
            name: "marker".into(),
            role: Role::ConstantReturner,
        },
    ]);
    let v = verify(&good, &fingerprint(), &table(&["k"]), &all_ir);
    assert_eq!(v.confirmed(), 3, "rejected: {:?}", v.rejected);

    // The same three functions with the roles rotated: none of them fits.
    let wrong = artifact(vec![
        Proposal {
            function_id: 1,
            name: "ignore".into(),
            role: Role::Identity,
        },
        Proposal {
            function_id: 2,
            name: "passThrough".into(),
            role: Role::ConstantReturner,
        },
        Proposal {
            function_id: 3,
            name: "marker".into(),
            role: Role::Noop,
        },
    ]);
    let v = verify(&wrong, &fingerprint(), &table(&["k"]), &all_ir);
    assert_eq!(v.confirmed(), 0);
    assert_eq!(v.rejected.len(), 3);
}

// A hole makes the array something other than a string table, so the function is
// not the decoder it was claimed to be.
#[test]
fn an_array_with_a_hole_is_not_a_string_table() {
    let body = vec![
        Statement::Let {
            name: "table".to_string(),
            value: Expression::Array {
                elements: vec![Some(string("alpha")), None],
            },
            kind: VarKind::Const,
        },
        Statement::Return(Some(Expression::Member {
            object: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                "table".to_string(),
            )))),
            property: crate::ir::PropertyKey::Computed(Box::new(Expression::Value(
                Value::Parameter(0),
            ))),
            optional: false,
        })),
    ];
    let all_ir = ir(vec![(7, body)]);
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".into(),
        role: Role::StringDecoder,
    }]);
    let v = verify(&art, &fingerprint(), &table(&["alpha"]), &all_ir);
    assert!(matches!(
        only_rejection(&v),
        Rejection::RoleNotConfirmed { .. }
    ));
}

// A fixed property name is a field read, not a decode.
#[test]
fn a_fixed_property_read_is_not_a_decoder() {
    let body = vec![Statement::Return(Some(Expression::Member {
        object: Box::new(string_array(&["alpha"])),
        property: crate::ir::PropertyKey::Ident("length".to_string()),
        optional: false,
    }))];
    let all_ir = ir(vec![(7, body)]);
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".into(),
        role: Role::StringDecoder,
    }]);
    let v = verify(&art, &fingerprint(), &table(&["alpha"]), &all_ir);
    assert!(matches!(
        only_rejection(&v),
        Rejection::RoleNotConfirmed { .. }
    ));
}

#[test]
fn extraction_finds_the_shapes_and_skips_functions_that_already_have_a_name() {
    let all_ir = ir(vec![
        (1, decoder_body(&["alpha", "beta"])),
        (
            2,
            vec![Statement::Return(Some(Expression::Value(
                Value::Parameter(0),
            )))],
        ),
        (3, vec![Statement::Return(Some(string("k")))]),
        // A real body with nothing verification could judge.
        (
            4,
            vec![
                Statement::Expr(string("side effect")),
                Statement::Return(None),
            ],
        ),
    ]);
    let mut names = BTreeMap::new();
    names.insert(3u32, "alreadyRecovered".to_string());
    let found = extract(&all_ir, &names);

    let ids: BTreeSet<u32> = found.iter().map(|c| c.function_id).collect();
    assert!(ids.contains(&1), "the decoder is a candidate");
    assert!(ids.contains(&2), "the identity is a candidate");
    assert!(
        !ids.contains(&3),
        "a function whose name the bytecode already gave is left alone"
    );
    assert!(
        !ids.contains(&4),
        "a body no role describes is not proposed"
    );

    let decoder = found.iter().find(|c| c.function_id == 1).expect("decoder");
    assert_eq!(decoder.shape, Role::StringDecoder);
    assert_eq!(decoder.sample_of, 2);
    assert_eq!(
        decoder.sample,
        vec!["alpha".to_string(), "beta".to_string()]
    );
}

// Renaming happens only through verification, so a rejected proposal leaves the IR
// exactly as it was.
#[test]
fn application_touches_only_confirmed_functions() {
    use crate::ir::FunctionId;
    let mut all_ir = ir(vec![(
        0,
        vec![
            Statement::Expr(Expression::Function {
                id: FunctionId(7),
                name: None,
                is_arrow: false,
                is_async: false,
                is_generator: false,
            }),
            Statement::Expr(Expression::Function {
                id: FunctionId(8),
                name: None,
                is_arrow: false,
                is_async: false,
                is_generator: false,
            }),
        ],
    )]);
    let mut verified = super::VerifiedArtifact::default();
    verified.names.insert(7, "decodeName".to_string());

    let applied = super::apply(&mut all_ir, &verified);
    assert_eq!(applied, 1);

    let body = &all_ir[&0];
    match &body[0] {
        Statement::Expr(Expression::Function { name, .. }) => {
            assert_eq!(name.as_deref(), Some("decodeName"))
        }
        other => panic!("unexpected {other:?}"),
    }
    match &body[1] {
        Statement::Expr(Expression::Function { name, .. }) => {
            assert_eq!(name.as_deref(), None, "no proposal, no rename")
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn an_artifact_survives_a_round_trip_through_json() {
    let art = artifact(vec![Proposal {
        function_id: 7,
        name: "decodeName".to_string(),
        role: Role::StringDecoder,
    }]);
    let text = serde_json::to_string_pretty(&art).expect("serialise");
    let back: Artifact = serde_json::from_str(&text).expect("parse");
    assert_eq!(art, back);
    assert!(
        text.contains("\"string_decoder\""),
        "the role is written in a form a reader can produce: {text}"
    );
}
