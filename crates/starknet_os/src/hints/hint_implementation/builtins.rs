use blockifier::state::state_api::StateReader;
use cairo_lang_runner::short_string::as_cairo_short_string;
use cairo_vm::hint_processor::builtin_hint_processor::hint_utils::{
    get_integer_from_var_name,
    get_ptr_from_var_name,
    insert_value_from_var_name,
};
use cairo_vm::hint_processor::hint_processor_utils::felt_to_usize;
use cairo_vm::types::relocatable::MaybeRelocatable;

use crate::hints::error::{OsHintError, OsHintResult};
use crate::hints::types::HintArgs;
use crate::hints::vars::{CairoStruct, Ids};
use crate::vm_utils::get_address_of_nested_fields;

pub(crate) fn selected_builtins<S: StateReader>(HintArgs { .. }: HintArgs<'_, S>) -> OsHintResult {
    todo!()
}

pub(crate) fn select_builtin<S: StateReader>(HintArgs { .. }: HintArgs<'_, S>) -> OsHintResult {
    todo!()
}

/// Update subsets of the pointer at 'builtin_ptrs' with the pointers at 'selected_ptrs' according
/// to the location specified by 'selected_encodings'.
///
/// Assumption: selected builtins encoding is an ordered subset of builtin_params.
pub(crate) fn update_builtin_ptrs<S: StateReader>(
    HintArgs { hint_processor, vm, ids_data, ap_tracking, .. }: HintArgs<'_, S>,
) -> OsHintResult {
    let n_builtins = get_integer_from_var_name(Ids::NBuiltins.into(), vm, ids_data, ap_tracking)?;

    let builtins_encoding_addr = get_address_of_nested_fields(
        ids_data,
        Ids::BuiltinParams,
        CairoStruct::BuiltinParamsPtr,
        vm,
        ap_tracking,
        &["builtin_encodings"],
        &hint_processor.os_program,
    )?;

    let n_selected_builtins =
        get_integer_from_var_name(Ids::NSelectedBuiltins.into(), vm, ids_data, ap_tracking)?;

    let selected_encodings =
        get_ptr_from_var_name(Ids::SelectedEncodings.into(), vm, ids_data, ap_tracking)?;

    let builtin_ptrs = get_ptr_from_var_name(Ids::BuiltinPtrs.into(), vm, ids_data, ap_tracking)?;

    let orig_builtin_ptrs = builtin_ptrs;

    let selected_ptrs = get_ptr_from_var_name(Ids::SelectedPtrs.into(), vm, ids_data, ap_tracking)?;

    let all_builtins =
        vm.get_continuous_range(builtins_encoding_addr, felt_to_usize(&n_builtins)?)?;

    let selected_builtins =
        vm.get_continuous_range(selected_encodings, felt_to_usize(&n_selected_builtins)?)?;

    let mut returned_builtins: Vec<MaybeRelocatable> = Vec::new();
    let mut selected_builtin_offset: usize = 0;

    for (i, builtin) in all_builtins.iter().enumerate() {
        // For debugging purposes, try to decode the builtin name - ignore failures.
        let decoded_builtin: Option<String> = match builtin {
            MaybeRelocatable::Int(builtin_value) => as_cairo_short_string(builtin_value),
            MaybeRelocatable::RelocatableValue(builtin_ptr) => vm
                .get_integer(*builtin_ptr)
                .map(|felt| as_cairo_short_string(&felt))
                .unwrap_or(None),
        };
        if selected_builtins.contains(builtin) {
            // TODO(Dori): consider computing the address of the selected builtin via the
            //   `get_address_of_nested_fields` utility. See `OsLogger::insert_builtins` for an
            //   example for accessing fields of `CairoStruct::BuiltinPointers`.
            returned_builtins.push(
                vm.get_maybe(&(selected_ptrs + selected_builtin_offset)?).ok_or_else(|| {
                    OsHintError::MissingSelectedBuiltinPtr {
                        builtin: builtin.clone(),
                        decoded: decoded_builtin,
                    }
                })?,
            );
            selected_builtin_offset += 1;
        } else {
            // The builtin is unselected, hence its value is the same as before calling the program.
            returned_builtins.push(vm.get_maybe(&(orig_builtin_ptrs + i)?).ok_or_else(|| {
                OsHintError::MissingUnselectedBuiltinPtr {
                    builtin: builtin.clone(),
                    decoded: decoded_builtin,
                }
            })?);
        }
    }

    let return_builtin_ptrs_base = vm.add_memory_segment();
    vm.load_data(return_builtin_ptrs_base, &returned_builtins)?;
    insert_value_from_var_name(
        Ids::ReturnBuiltinPtrs.into(),
        return_builtin_ptrs_base,
        vm,
        ids_data,
        ap_tracking,
    )?;
    Ok(())
}
