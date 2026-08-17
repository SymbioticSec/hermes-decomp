use super::*;

#[test]
fn metro_roles_not_applied_in_get_slot_name() {
    let mut info = ClosureInfo::new();
    info.slots
        .insert(1, ClosureSlotValue::Variable("arg1".into()));
    // Without apply_metro_param_roles, arg1 stays generic (not "require").
    assert_eq!(info.get_slot_name(1), "closure_1");
}

#[test]
fn metro_roles_applied_only_via_explicit_call() {
    let mut info = ClosureInfo::new();
    info.slots
        .insert(1, ClosureSlotValue::Variable("arg1".into()));
    info.slots
        .insert(4, ClosureSlotValue::Variable("arg4".into()));
    info.apply_metro_param_roles();
    assert_eq!(info.get_slot_name(1), "require");
    assert_eq!(info.get_slot_name(4), "dependencyMap");
}

#[test]
fn re_n_only_for_exclusive_regexp_slots() {
    let mut info = ClosureInfo::new();
    info.store_slot(3, ClosureSlotValue::RegExp);
    assert_eq!(info.get_slot_name(3), "re3");

    // Non-regex store overwrites RegExp → no reN.
    info.store_slot(3, ClosureSlotValue::Variable("parseInt".into()));
    assert_eq!(info.get_slot_name(3), "parseInt");
}

#[test]
fn re_n_dropped_when_regex_follows_non_regex() {
    let mut info = ClosureInfo::new();
    info.store_slot(5, ClosureSlotValue::Variable("tmp".into()));
    info.store_slot(5, ClosureSlotValue::RegExp);
    // Mixed reuse → Unknown → closure_N, not re5.
    assert_eq!(info.get_slot_name(5), "closure_5");
}

#[test]
fn string_constant_slash_is_not_ren() {
    let mut info = ClosureInfo::new();
    info.store_slot(0, ClosureSlotValue::Constant("\"/api/v1\"".into()));
    assert_eq!(info.get_slot_name(0), "c0");
}

#[test]
fn mutable_counter_keeps_init_name_not_sum() {
    // env[0] = 0; env[0] = sum  → still named c0, not sum
    let mut info = ClosureInfo::new();
    info.store_slot(0, ClosureSlotValue::Constant("0".into()));
    info.store_slot(0, ClosureSlotValue::Variable("sum".into()));
    assert_eq!(info.get_slot_name(0), "c0");
}
