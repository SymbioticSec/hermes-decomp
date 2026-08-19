use super::*;

#[test]
fn test_callback_param_hints_map() {
    let hints = callback_param_hints("map").unwrap();
    assert_eq!(hints[0], Some("item".to_string()));
    assert_eq!(hints[1], Some("index".to_string()));
}

#[test]
fn test_callback_param_hints_reduce() {
    let hints = callback_param_hints("reduce").unwrap();
    assert_eq!(hints.len(), 3);
    assert_eq!(hints[0], Some("acc".to_string()));
    assert_eq!(hints[1], Some("item".to_string()));
}

#[test]
fn test_callback_param_hints_then_catch() {
    let then = callback_param_hints("then").unwrap();
    assert_eq!(then[0], Some("result".to_string()));

    let catch = callback_param_hints("catch").unwrap();
    assert_eq!(catch[0], Some("error".to_string()));
}

#[test]
fn test_callback_param_hints_sort() {
    let hints = callback_param_hints("sort").unwrap();
    assert_eq!(hints[0], Some("a".to_string()));
    assert_eq!(hints[1], Some("b".to_string()));
}

#[test]
fn test_callback_param_hints_unknown() {
    assert!(callback_param_hints("unknownMethod").is_none());
}

#[test]
fn test_callback_param_hints_event_listener() {
    let hints = callback_param_hints("addEventListener").unwrap();
    assert_eq!(hints[0], Some("event".to_string()));
}
