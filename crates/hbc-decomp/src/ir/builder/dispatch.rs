use super::opcodes_arith::*;
use super::opcodes_call::*;
use super::env_state::EnvRegMap;
use super::opcodes_environment::{
    handle_create_environment, handle_get_closure_environment, handle_get_environment,
    handle_load_from_environment, handle_store_np_to_environment, handle_store_to_environment,
};
use super::opcodes_flow::{
    handle_catch, handle_debugger, handle_get_next_pname, handle_ignored_guard, handle_jmp,
    handle_jmp_builtin_is, handle_jmp_comparison, handle_jmp_cond, handle_jmp_typeof_is,
    handle_jmp_undefined, handle_ret, handle_select_object, handle_throw,
    handle_throw_if_undefined, FlowResult,
};
use super::opcodes_generator::{
    handle_complete_generator, handle_create_generator, handle_resume_generator,
    handle_save_generator, handle_start_generator,
};
use super::opcodes_load::*;
use super::opcodes_obj::*;
use super::opcodes_prop::*;
use super::opcodes_switch::{handle_string_switch_imm, handle_switch_imm};
use crate::ir::Statement;
use crate::{BytecodeFile, BytecodeFormat, Instruction};

pub fn dispatch_instruction(
    inst: &Instruction,
    file: &BytecodeFile,
    format: &BytecodeFormat,
    resolve_strings: bool,
    func_bytecode_offset: u32,
    frame_size: u32,
    env_map: &mut EnvRegMap,
) -> FlowResult {
    let def = match format.definitions.get(inst.opcode as usize) {
        Some(d) => d,
        None => return unknown_opcode(inst),
    };

    let name = def.name.as_str();

    // Environment first so Create/Get update env_map before any later use.
    if let Some(result) = try_env_handlers(name, inst, env_map) {
        return result;
    }
    // Try each handler category
    if let Some(result) = try_load_handlers(name, inst, file, resolve_strings, env_map) {
        return result;
    }
    if let Some(result) = try_arith_handlers(name, inst) {
        return result;
    }
    if let Some(result) = try_prop_handlers(name, inst, file, resolve_strings) {
        return result;
    }
    if let Some(result) = try_call_handlers(name, inst, file, resolve_strings, frame_size, format.version) {
        return result;
    }
    if let Some(result) = try_obj_handlers(name, inst, file, resolve_strings) {
        return result;
    }
    if let Some(result) = try_flow_handlers(name, inst, format, file, func_bytecode_offset) {
        return result;
    }

    FlowResult::Statement(Statement::Comment(format!(
        "{} (0x{:02x})",
        name, inst.opcode
    )))
}

fn try_env_handlers(
    name: &str,
    inst: &Instruction,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    match name {
        "CreateEnvironment"
        | "CreateFunctionEnvironment"
        | "CreateTopLevelEnvironment"
        | "CreateInnerEnvironment" => handle_create_environment(name, inst, env_map),
        "GetEnvironment" | "GetParentEnvironment" => handle_get_environment(inst, env_map),
        "GetClosureEnvironment" => handle_get_closure_environment(inst, env_map),
        "LoadFromEnvironment" | "LoadFromEnvironmentL" => {
            handle_load_from_environment(inst, env_map)
        }
        "StoreToEnvironment" | "StoreToEnvironmentL" => {
            handle_store_to_environment(inst, env_map)
        }
        "StoreNPToEnvironment" | "StoreNPToEnvironmentL" => {
            handle_store_np_to_environment(inst, env_map)
        }
        // Legacy aliases treated as GetEnvironment.
        "LoadParentNoTraps" | "TypedLoadParent" => handle_get_environment(inst, env_map),
        _ => None,
    }
}

fn try_load_handlers(
    name: &str,
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    // Propagate env-level on register copies so Load/Store after Mov still resolve.
    if matches!(name, "Mov" | "MovLong") {
        if let (Some(dst), Some(src)) = (
            super::opcodes_load::get_reg(&inst.operands, 0),
            super::opcodes_load::get_reg(&inst.operands, 1),
        ) {
            env_map.copy_reg(dst, src);
        }
    }

    match name {
        "LoadConstUndefined"
        | "LoadConstNull"
        | "LoadConstTrue"
        | "LoadConstFalse"
        | "LoadConstZero"
        | "LoadConstEmpty"
        | "LoadConstUInt8"
        | "LoadConstInt"
        | "LoadConstDouble"
        | "LoadConstString"
        | "LoadConstStringLongIndex"
        | "LoadConstBigInt"
        | "LoadConstBigIntLongIndex" =>
            handle_load_const(name, inst, file, resolve_strings).map(FlowResult::Statement),
        "Mov" | "MovLong" => handle_mov(inst).map(FlowResult::Statement),
        "LoadParam" | "LoadParamLong" => handle_load_param(inst).map(FlowResult::Statement),
        "GetGlobalObject" => handle_get_global(inst).map(FlowResult::Statement),
        "LoadThisNS" => handle_load_this(inst).map(FlowResult::Statement),
        "DeclareGlobalVar" => {
            handle_declare_global(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        _ => None,
    }
}

fn try_arith_handlers(name: &str, inst: &Instruction) -> Option<FlowResult> {
    match name {
        "Add" | "AddN" | "AddS" | "Sub" | "SubN" | "Mul" | "MulN" | "Div" | "DivN" | "Mod"
        | "BitAnd" | "BitOr" | "BitXor" | "LShift" | "Shl" | "RShift" | "Shr" | "URshift"
        | "UShr" => handle_binary_op(name, inst).map(FlowResult::Statement),
        "Eq" | "StrictEq" | "Neq" | "StrictNeq" | "Less" | "LessEq" | "Greater"
        | "GreaterEq" => handle_comparison(name, inst).map(FlowResult::Statement),
        "Negate" | "Not" | "BitNot" | "TypeOf" => {
            handle_unary_op(name, inst).map(FlowResult::Statement)
        }
        "Inc" | "Dec" => handle_inc_dec(name, inst).map(FlowResult::Statement),
        "ToNumber" | "ToNumeric" | "ToInt32" | "ToUint32" | "AddEmptyString"
        | "CoerceThisNS" => handle_coercion(name, inst).map(FlowResult::Statement),
        "InstanceOf" | "IsIn" => handle_instance_in(name, inst).map(FlowResult::Statement),
        _ => None,
    }
}

fn try_prop_handlers(
    name: &str,
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<FlowResult> {
    match name {
        "GetById" | "GetByIdLong" | "GetByIdShort" => {
            handle_get_by_id(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "GetByIdWithReceiver" | "GetByIdWithReceiverLong" => {
            handle_get_by_id_with_receiver(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "TryGetById" | "TryGetByIdLong" => {
            handle_try_get_by_id(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "PutById" | "PutByIdLong" | "PutByIdLoose" | "PutByIdStrict" | "PutByIdLooseLong"
        | "PutByIdStrictLong" | "PutNewOwnById" | "PutNewOwnByIdLong" | "PutNewOwnByIdShort"
        | "TryPutById" | "TryPutByIdLong" | "TryPutByIdLoose" | "TryPutByIdStrict"
        | "TryPutByIdLooseLong" | "TryPutByIdStrictLong" => {
            handle_put_by_id(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "DefineOwnById" | "DefineOwnByIdLong" => {
            handle_put_by_id(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "GetByVal" => handle_get_by_val(inst).map(FlowResult::Statement),
        "ToPropertyKey" => handle_to_property_key(inst).map(FlowResult::Statement),
        "CreatePrivateName" => {
            handle_create_private_name(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "GetOwnPrivateBySym" => {
            handle_get_own_private_by_sym(inst).map(FlowResult::Statement)
        }
        "PutOwnPrivateBySym" => {
            handle_put_own_private_by_sym(inst).map(FlowResult::Statement)
        }
        "AddOwnPrivateBySym" => {
            handle_add_own_private_by_sym(inst).map(FlowResult::Statement)
        }
        "PrivateIsIn" => handle_private_is_in(inst).map(FlowResult::Statement),
        "GetByValWithReceiver" => {
            handle_get_by_val_with_receiver(inst).map(FlowResult::Statement)
        }
        "PutByValWithReceiver" => {
            handle_put_by_val_with_receiver(inst).map(FlowResult::Statement)
        }
        "PutByVal" | "PutByValLoose" | "PutByValStrict" => {
            handle_put_by_val(inst).map(FlowResult::Statement)
        }
        "DelByVal" | "DelByValLoose" | "DelByValStrict" => {
            handle_del_by_val(inst).map(FlowResult::Statement)
        }
        "DelById" => handle_del_by_id(inst, file, resolve_strings).map(FlowResult::Statement),
        "TypeOfIs" => {
            handle_typeof_is(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "TypeOfIsNot" => {
            // The inverse of TypeOfIs. The decoded condition may be any boolean
            // shape (a single compare or a disjunction), so negate the whole
            // expression rather than flipping one operator.
            handle_typeof_is(inst, file, resolve_strings).map(|stmt| {
                if let Statement::Assign { target, value } = stmt {
                    FlowResult::Statement(Statement::Assign {
                        target,
                        value: crate::ir::Expression::unary(crate::ir::UnaryOp::Not, value),
                    })
                } else {
                    FlowResult::Statement(stmt)
                }
            })
        }
        _ => None,
    }
}

fn try_call_handlers(
    name: &str,
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
    frame_size: u32,
    version: u32,
) -> Option<FlowResult> {
    match name {
        "Call1" | "Call2" | "Call3" | "Call4" => {
            handle_call_fixed(name, inst).map(FlowResult::Statement)
        }
        "Call" | "CallLong" => handle_call(inst, frame_size, version).map(FlowResult::Statement),
        "Construct" | "ConstructLong" | "CallWithNewTarget" | "CallWithNewTargetLong" => {
            handle_construct(inst, frame_size, version).map(FlowResult::Statement)
        }
        "CreateClosure" | "CreateClosureLongIndex" => {
            handle_create_closure(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "CreateAsyncClosure" => {
            handle_create_async_closure(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "CreateGeneratorClosure" => {
            handle_create_generator_closure(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "CallBuiltin" | "CallBuiltinLong" => {
            handle_call_builtin(inst, frame_size, version).map(noreturn_builtin_result)
        }
        "GetBuiltinClosure" => handle_get_builtin_closure(inst, version).map(FlowResult::Statement),
        "DirectEval" => handle_direct_eval(inst).map(FlowResult::Statement),
        "CallRequire" => handle_call_require(inst).map(FlowResult::Statement),
        _ => None,
    }
}

fn try_obj_handlers(
    name: &str,
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<FlowResult> {
    match name {
        "NewObject" | "CacheNewObject" => handle_new_object(inst).map(FlowResult::Statement),
        "CreateBaseClass" | "CreateBaseClassLongIndex" => {
            handle_create_class(inst, file, resolve_strings, false).map(FlowResult::Statement)
        }
        "CreateDerivedClass" | "CreateDerivedClassLongIndex" => {
            handle_create_class(inst, file, resolve_strings, true).map(FlowResult::Statement)
        }
        "NewObjectWithParent" | "NewObjectWithBufferAndParent" => {
            handle_new_object_with_parent(inst, file).map(FlowResult::Statement)
        }
        "NewObjectWithBuffer" | "NewObjectWithBufferLong" => {
            handle_new_object_with_buffer(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "NewArray" | "NewFastArray" => handle_new_array(inst).map(FlowResult::Statement),
        "NewArrayWithBuffer" | "NewArrayWithBufferLong" => {
            handle_new_array_with_buffer(inst, file).map(FlowResult::Statement)
        }
        "PutOwnByIndex" | "PutOwnByIndexL" | "DefineOwnByIndex" | "DefineOwnByIndexL"
        | "DefineOwnInDenseArray" | "DefineOwnInDenseArrayL" => {
            handle_put_own_by_index(inst).map(FlowResult::Statement)
        }
        // PutOwnBySlotIdx: store value into object slot (same semantics as PutOwnByIndex)
        "PutOwnBySlotIdx" | "PutOwnBySlotIdxLong" => {
            handle_put_own_by_index(inst).map(FlowResult::Statement)
        }
        // GetOwnBySlotIdx: load from object slot
        "GetOwnBySlotIdx" | "GetOwnBySlotIdxLong" => {
            handle_get_own_by_slot(inst).map(FlowResult::Statement)
        }
        "GetByIndex" => handle_get_by_index(inst).map(FlowResult::Statement),
        "PutOwnByVal" | "DefineOwnByVal" => {
            handle_put_own_by_val(inst).map(FlowResult::Statement)
        }
        "FastArrayLoad" => handle_fast_array_load(inst).map(FlowResult::Statement),
        "FastArrayStore" | "FastArrayStoreLoose" => {
            handle_fast_array_store(inst).map(FlowResult::Statement)
        }
        "FastArrayPush" => handle_fast_array_push(inst).map(FlowResult::Statement),
        "FastArrayAppend" => handle_fast_array_append(inst).map(FlowResult::Statement),
        "FastArrayLength" => handle_fast_array_length(inst).map(FlowResult::Statement),
        "CreateRegExp" => {
            handle_create_regexp(inst, file, resolve_strings).map(FlowResult::Statement)
        }
        "GetArgumentsLength" => handle_get_arguments_length(inst).map(FlowResult::Statement),
        "GetArgumentsPropByVal" | "GetArgumentsPropByValLoose"
        | "GetArgumentsPropByValStrict" => {
            handle_get_arguments_prop_by_val(inst).map(FlowResult::Statement)
        }
        "ReifyArguments" | "ReifyArgumentsLoose" | "ReifyArgumentsStrict" => {
            handle_reify_arguments(inst).map(FlowResult::Statement)
        }
        // CreateThisForSuper (HBC >=97): allocates the derived `this` from the
        // parent class. Operand 0 is the destination; the super-constructor result
        // overwrites it and cleanup drops the placeholder. Treat like CreateThis
        // so it no longer leaves an unhandled-opcode comment + dangling temps.
        "CreateThis" | "CreateThisForNew" | "CreateThisForSuper" => {
            handle_create_this(inst).map(FlowResult::Statement)
        }
        "GetNewTarget" => handle_get_new_target(inst).map(FlowResult::Statement),
        "IteratorBegin" => handle_iterator_begin(inst).map(FlowResult::Statement),
        "IteratorNext" => handle_iterator_next(inst).map(FlowResult::Statement),
        "IteratorClose" => handle_iterator_close(inst).map(FlowResult::Statement),
        "GetPNameList" => handle_get_pname_list(inst).map(FlowResult::Statement),
        "PutOwnGetterSetterByVal" | "DefineOwnGetterSetterByVal" => {
            handle_put_own_getter_setter_by_val(inst).map(FlowResult::Statement)
        }
        _ => None,
    }
}

fn try_flow_handlers(
    name: &str,
    inst: &Instruction,
    format: &BytecodeFormat,
    file: &BytecodeFile,
    func_bytecode_offset: u32,
) -> Option<FlowResult> {
    match name {
        "Jmp" | "JmpLong" => handle_jmp(inst, format),
        "JmpTrue" | "JmpTrueLong" | "JmpFalse" | "JmpFalseLong" => {
            handle_jmp_cond(name, inst, format)
        }
        "JEqual"
        | "JNotEqual"
        | "JStrictEqual"
        | "JStrictNotEqual"
        | "JEqualLong"
        | "JNotEqualLong"
        | "JStrictEqualLong"
        | "JStrictNotEqualLong"
        | "JLess"
        | "JLessEqual"
        | "JGreater"
        | "JGreaterEqual"
        | "JLessLong"
        | "JLessEqualLong"
        | "JGreaterLong"
        | "JGreaterEqualLong"
        | "JLessN"
        | "JLessEqualN"
        | "JGreaterN"
        | "JGreaterEqualN"
        | "JLessNLong"
        | "JLessEqualNLong"
        | "JGreaterNLong"
        | "JGreaterEqualNLong"
        | "JNotLess"
        | "JNotLessEqual"
        | "JNotGreater"
        | "JNotGreaterEqual"
        | "JNotLessLong"
        | "JNotLessEqualLong"
        | "JNotGreaterLong"
        | "JNotGreaterEqualLong"
        | "JNotLessN"
        | "JNotLessEqualN"
        | "JNotGreaterN"
        | "JNotGreaterEqualN"
        | "JNotLessNLong"
        | "JNotLessEqualNLong"
        | "JNotGreaterNLong"
        | "JNotGreaterEqualNLong" => handle_jmp_comparison(name, inst, format),
        "JmpUndefined" | "JmpUndefinedLong" => handle_jmp_undefined(name, inst, format),
        // JmpTypeOfIs: branch if typeof(reg) === typeString
        "JmpTypeOfIs" => handle_jmp_typeof_is(inst, format, file),
        // JmpBuiltinIs/IsNot: branch if typeof(reg) === typeString (builtin type check)
        "JmpBuiltinIs" | "JmpBuiltinIsLong" | "JmpBuiltinIsNot" | "JmpBuiltinIsNotLong" => {
            handle_jmp_builtin_is(name, inst, format)
        }
        "Ret" => handle_ret(inst),
        "Throw" => handle_throw(inst),
        "ThrowIfThisInitialized" | "ProfilePoint" | "Unreachable" => handle_ignored_guard(),
        // Both are dead-zone guards of the shape (destination, checked value):
        // they raise only when the value is still empty or undefined, and
        // otherwise move it. Routing them to the unconditional Throw handler
        // threw the destination register and cut the block short on a path that
        // does not actually raise.
        "ThrowIfUndefined" | "ThrowIfEmpty" => handle_throw_if_undefined(inst),
        // Environment opcodes handled in try_env_handlers (need EnvRegMap).
        "SelectObject" => handle_select_object(inst),
        "Debugger" | "AsyncBreakCheck" => handle_debugger(),
        "Catch" => handle_catch(inst),
        "StartGenerator" => handle_start_generator(),
        "ResumeGenerator" => handle_resume_generator(inst),
        "CreateGenerator" | "CreateGeneratorLongIndex" => handle_create_generator(inst),
        "CompleteGenerator" => handle_complete_generator(inst),
        "SaveGenerator" | "SaveGeneratorLong" => handle_save_generator(inst, format),
        "GetNextPName" => handle_get_next_pname(inst),
        "SwitchImm" | "UIntSwitchImm" => {
            handle_switch_imm(inst, format, file, func_bytecode_offset)
        }
        "StringSwitchImm" => {
            handle_string_switch_imm(inst, format, file, func_bytecode_offset)
        }
        _ => None,
    }
}

// `HermesBuiltin.throwTypeError` and `throwReferenceError` never return: they
// raise. Emitted as an ordinary call, the block kept its fall through and every
// statement after them stayed reachable, which produced dead branches that read
// as real control flow. They become a throw of the error they raise, which both
// prints as source and terminates the block.
fn noreturn_builtin_result(stmt: Statement) -> FlowResult {
    use crate::ir::{Expression, PropertyKey, Value};

    if let Statement::Assign {
        value: Expression::Call { callee, arguments },
        ..
    } = &stmt
    {
        let ctor = match callee.as_ref() {
            Expression::Member {
                property: PropertyKey::Ident(p) | PropertyKey::String(p),
                ..
            } => match p.as_str() {
                "throwTypeError" => Some("TypeError"),
                "throwReferenceError" => Some("ReferenceError"),
                _ => None,
            },
            _ => None,
        };
        if let Some(ctor) = ctor {
            return FlowResult::Throw(Expression::New {
                callee: Box::new(Expression::member(Expression::Value(Value::Global), ctor)),
                arguments: arguments.clone(),
            });
        }
    }
    FlowResult::Statement(stmt)
}

fn unknown_opcode(inst: &Instruction) -> FlowResult {
    FlowResult::Statement(Statement::Comment(format!(
        "unknown opcode 0x{:02x}",
        inst.opcode
    )))
}
