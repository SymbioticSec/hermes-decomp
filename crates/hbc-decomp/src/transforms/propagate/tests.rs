use super::*;
use crate::ir::{CFGBuilder, Constant};

#[test]
fn test_constant_propagation() {
    let mut builder = CFGBuilder::new();
    builder.emit(Statement::assign_reg(
        0,
        Expression::constant(Constant::Integer(42)),
    ));
    builder.emit(Statement::assign_reg(
        1,
        Expression::Value(Value::Register(0)),
    ));
    builder.emit_return(Some(Expression::Value(Value::Register(1))));

    let mut cfg = builder.finish();
    propagate(&mut cfg, &PropagationConfig::new());

    let block = cfg.entry_block();
    // After propagation, r1 should be assigned 42, not r0
    if let Statement::Assign { value, .. } = &block.statements[1] {
        assert_eq!(*value, Expression::constant(Constant::Integer(42)));
    }
}

// A register-to-register copy whose source and use live in different blocks
// (the loop-latch shape `Mov r0, i; ...; Inc i, r0`) must be propagated.
#[test]
fn test_cross_block_copy_propagation() {
    // b0:  r5 = 0
    //      r0 = r5          ; copy (single def of r0)
    //      -> b1
    // b1:  r5 = r0 + 1      ; use of r0 -> should read r5
    //      return r5
    let mut builder = CFGBuilder::new();
    let b1 = builder.create_block();
    builder.emit(Statement::assign_reg(
        5,
        Expression::constant(Constant::Integer(0)),
    ));
    builder.emit(Statement::assign_reg(
        0,
        Expression::Value(Value::Register(5)),
    ));
    builder.emit_jump(b1);
    builder.set_current_block(b1);
    builder.emit(Statement::assign_reg(
        5,
        Expression::binary(
            crate::ir::BinaryOp::Add,
            Expression::register(0),
            Expression::constant(Constant::Integer(1)),
        ),
    ));
    builder.emit_return(Some(Expression::Value(Value::Register(5))));

    let mut cfg = builder.finish();
    propagate_copies(&mut cfg);

    let block = cfg.get(b1).unwrap();
    if let Statement::Assign {
        value: Expression::Binary { left, .. },
        ..
    } = &block.statements[0]
    {
        assert_eq!(
            **left,
            Expression::Value(Value::Register(5)),
            "cross-block copy r0=r5 should propagate into the increment"
        );
    } else {
        panic!("expected binary assignment");
    }
}

// Correctness guard: if the copy source is reassigned between the copy and
// the use, the copy must NOT be propagated (the values differ).
#[test]
fn test_copy_not_propagated_when_source_reassigned() {
    // b0:  r5 = 0
    //      r0 = r5          ; copy
    //      r5 = 99          ; source reassigned before the use
    //      -> b1
    // b1:  r7 = r0 + 1      ; use of r0 -> must stay r0 (r0 == 0, not 99)
    //      return r7
    let mut builder = CFGBuilder::new();
    let b1 = builder.create_block();
    builder.emit(Statement::assign_reg(
        5,
        Expression::constant(Constant::Integer(0)),
    ));
    builder.emit(Statement::assign_reg(
        0,
        Expression::Value(Value::Register(5)),
    ));
    builder.emit(Statement::assign_reg(
        5,
        Expression::constant(Constant::Integer(99)),
    ));
    builder.emit_jump(b1);
    builder.set_current_block(b1);
    builder.emit(Statement::assign_reg(
        7,
        Expression::binary(
            crate::ir::BinaryOp::Add,
            Expression::register(0),
            Expression::constant(Constant::Integer(1)),
        ),
    ));
    builder.emit_return(Some(Expression::Value(Value::Register(7))));

    let mut cfg = builder.finish();
    propagate_copies(&mut cfg);

    let block = cfg.get(b1).unwrap();
    if let Statement::Assign {
        value: Expression::Binary { left, .. },
        ..
    } = &block.statements[0]
    {
        assert_eq!(
            **left,
            Expression::Value(Value::Register(0)),
            "copy must not be propagated when the source was reassigned in between"
        );
    } else {
        panic!("expected binary assignment");
    }
}

// A parameter staged through a copy chain (`r1 = r0; r0 = Parameter`) whose use
// is in a later block must resolve to the parameter, not stay a dangling copy.
// Every register is defined once here (as SSA guarantees), so the chain is safe.
#[test]
fn test_param_copy_chain_resolves_across_blocks() {
    // b0:  r0 = Parameter(0)
    //      r1 = r0            ; copy of the param
    //      r2 = r1            ; second copy staged for a later use
    //      -> b1
    // b1:  return r2          ; must resolve to Parameter(0)
    let mut builder = CFGBuilder::new();
    let b1 = builder.create_block();
    builder.emit(Statement::assign_reg(0, Expression::Value(Value::Parameter(0))));
    builder.emit(Statement::assign_reg(1, Expression::Value(Value::Register(0))));
    builder.emit(Statement::assign_reg(2, Expression::Value(Value::Register(1))));
    builder.emit_jump(b1);
    builder.set_current_block(b1);
    builder.emit_return(Some(Expression::Value(Value::Register(2))));

    let mut cfg = builder.finish();
    propagate(&mut cfg, &PropagationConfig::new());

    let block = cfg.get(b1).unwrap();
    assert_eq!(
        block.terminator,
        crate::ir::Terminator::Return(Some(Expression::Value(Value::Parameter(0)))),
        "a param copy chain must resolve to the parameter in a later block"
    );
}
