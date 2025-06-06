use std::sync::Arc;

use cairo_vm::types::relocatable::{MaybeRelocatable, Relocatable};
use cairo_vm::vm::vm_core::VirtualMachine;
use num_traits::ToPrimitive;
use starknet_api::block::{BlockHash, BlockNumber};
use starknet_api::contract_class::EntryPointType;
use starknet_api::core::{ClassHash, ContractAddress, EntryPointSelector, EthAddress, Nonce};
use starknet_api::state::StorageKey;
use starknet_api::transaction::fields::{Calldata, ContractAddressSalt, Fee, TransactionSignature};
use starknet_api::transaction::{
    signed_tx_version,
    EventContent,
    EventData,
    EventKey,
    InvokeTransactionV0,
    L2ToL1Payload,
    TransactionHasher,
    TransactionOptions,
    TransactionVersion,
};
use starknet_types_core::felt::Felt;

use self::hint_processor::{
    create_retdata_segment,
    execute_inner_call,
    felt_to_bool,
    read_call_params,
    read_calldata,
    read_felt_array,
    write_segment,
    EmitEventError,
    SyscallExecutionError,
    SyscallHintProcessor,
};
use crate::blockifier_versioned_constants::{EventLimits, VersionedConstants};
use crate::context::TransactionContext;
use crate::execution::call_info::MessageToL1;
use crate::execution::deprecated_syscalls::DeprecatedSyscallSelector;
use crate::execution::entry_point::{CallEntryPoint, CallType};
use crate::execution::execution_utils::{
    felt_from_ptr,
    write_felt,
    write_maybe_relocatable,
    ReadOnlySegment,
};
use crate::execution::syscalls::syscall_base::SyscallResult;
use crate::transaction::objects::{
    CommonAccountFields,
    DeprecatedTransactionInfo,
    TransactionInfo,
};

pub mod hint_processor;
mod secp;
pub mod syscall_base;

#[cfg(test)]
pub mod syscall_tests;

pub type WriteResponseResult = SyscallResult<()>;

pub type SyscallSelector = DeprecatedSyscallSelector;

pub trait SyscallRequest: Sized {
    fn read(_vm: &VirtualMachine, _ptr: &mut Relocatable) -> SyscallResult<Self>;

    /// Returns the linear factor's length for the syscall.
    /// If no factor exists, it returns 0.
    fn get_linear_factor_length(&self) -> usize {
        0
    }
}

pub trait SyscallResponse {
    fn write(self, _vm: &mut VirtualMachine, _ptr: &mut Relocatable) -> WriteResponseResult;
}

// Syscall header structs.
pub struct SyscallRequestWrapper<T: SyscallRequest> {
    pub gas_counter: u64,
    pub request: T,
}
impl<T: SyscallRequest> SyscallRequest for SyscallRequestWrapper<T> {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<Self> {
        let gas_counter = felt_from_ptr(vm, ptr)?;
        let gas_counter =
            gas_counter.to_u64().ok_or_else(|| SyscallExecutionError::InvalidSyscallInput {
                input: gas_counter,
                info: String::from("Unexpected gas."),
            })?;
        Ok(Self { gas_counter, request: T::read(vm, ptr)? })
    }
}

pub enum SyscallResponseWrapper<T: SyscallResponse> {
    Success { gas_counter: u64, response: T },
    Failure { gas_counter: u64, error_data: Vec<Felt> },
}
impl<T: SyscallResponse> SyscallResponse for SyscallResponseWrapper<T> {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        match self {
            Self::Success { gas_counter, response } => {
                write_felt(vm, ptr, Felt::from(gas_counter))?;
                // 0 to indicate success.
                write_felt(vm, ptr, Felt::ZERO)?;
                response.write(vm, ptr)
            }
            Self::Failure { gas_counter, error_data } => {
                write_felt(vm, ptr, Felt::from(gas_counter))?;
                // 1 to indicate failure.
                write_felt(vm, ptr, Felt::ONE)?;

                // Write the error data to a new memory segment.
                let revert_reason_start = vm.add_memory_segment();
                let revert_reason_end = vm.load_data(
                    revert_reason_start,
                    &error_data.into_iter().map(Into::into).collect::<Vec<MaybeRelocatable>>(),
                )?;

                // Write the start and end pointers of the error data.
                write_maybe_relocatable(vm, ptr, revert_reason_start)?;
                write_maybe_relocatable(vm, ptr, revert_reason_end)?;
                Ok(())
            }
        }
    }
}

// Common structs.

#[derive(Debug, Eq, PartialEq)]
pub struct EmptyRequest;

impl SyscallRequest for EmptyRequest {
    fn read(_vm: &VirtualMachine, _ptr: &mut Relocatable) -> SyscallResult<EmptyRequest> {
        Ok(EmptyRequest)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct EmptyResponse;

impl SyscallResponse for EmptyResponse {
    fn write(self, _vm: &mut VirtualMachine, _ptr: &mut Relocatable) -> WriteResponseResult {
        Ok(())
    }
}

#[derive(Debug)]
pub struct SingleSegmentResponse {
    segment: ReadOnlySegment,
}

impl SyscallResponse for SingleSegmentResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_segment(vm, ptr, self.segment)
    }
}

// CallContract syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct CallContractRequest {
    pub contract_address: ContractAddress,
    pub function_selector: EntryPointSelector,
    pub calldata: Calldata,
}

impl SyscallRequest for CallContractRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<CallContractRequest> {
        let contract_address = ContractAddress::try_from(felt_from_ptr(vm, ptr)?)?;
        let (function_selector, calldata) = read_call_params(vm, ptr)?;

        Ok(CallContractRequest { contract_address, function_selector, calldata })
    }
}

pub type CallContractResponse = SingleSegmentResponse;

pub fn call_contract(
    request: CallContractRequest,
    vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    remaining_gas: &mut u64,
) -> SyscallResult<CallContractResponse> {
    let storage_address = request.contract_address;
    let class_hash = syscall_handler.base.state.get_class_hash_at(storage_address)?;
    let selector = request.function_selector;
    if syscall_handler.is_validate_mode() && syscall_handler.storage_address() != storage_address {
        return Err(SyscallExecutionError::InvalidSyscallInExecutionMode {
            syscall_name: "call_contract".to_string(),
            execution_mode: syscall_handler.execution_mode(),
        });
    }
    let entry_point = CallEntryPoint {
        class_hash: None,
        code_address: Some(storage_address),
        entry_point_type: EntryPointType::External,
        entry_point_selector: selector,
        calldata: request.calldata,
        storage_address,
        caller_address: syscall_handler.storage_address(),
        call_type: CallType::Call,
        // NOTE: this value might be overridden later on.
        initial_gas: *remaining_gas,
    };

    let retdata_segment = execute_inner_call(entry_point, vm, syscall_handler, remaining_gas)
        .map_err(|error| match error {
            SyscallExecutionError::Revert { .. } => error,
            _ => error.as_call_contract_execution_error(class_hash, storage_address, selector),
        })?;

    Ok(CallContractResponse { segment: retdata_segment })
}

// Deploy syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct DeployRequest {
    pub class_hash: ClassHash,
    pub contract_address_salt: ContractAddressSalt,
    pub constructor_calldata: Calldata,
    pub deploy_from_zero: bool,
}

impl SyscallRequest for DeployRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<DeployRequest> {
        let class_hash = ClassHash(felt_from_ptr(vm, ptr)?);
        let contract_address_salt = ContractAddressSalt(felt_from_ptr(vm, ptr)?);
        let constructor_calldata = read_calldata(vm, ptr)?;
        let deploy_from_zero = felt_from_ptr(vm, ptr)?;

        Ok(DeployRequest {
            class_hash,
            contract_address_salt,
            constructor_calldata,
            deploy_from_zero: felt_to_bool(
                deploy_from_zero,
                "The deploy_from_zero field in the deploy system call must be 0 or 1.",
            )?,
        })
    }

    fn get_linear_factor_length(&self) -> usize {
        self.constructor_calldata.0.len()
    }
}

#[derive(Debug)]
pub struct DeployResponse {
    pub contract_address: ContractAddress,
    pub constructor_retdata: ReadOnlySegment,
}

impl SyscallResponse for DeployResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_felt(vm, ptr, *self.contract_address.0.key())?;
        write_segment(vm, ptr, self.constructor_retdata)
    }
}

pub fn deploy(
    request: DeployRequest,
    vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    remaining_gas: &mut u64,
) -> SyscallResult<DeployResponse> {
    // Increment the Deploy syscall's linear cost counter by the number of elements in the
    // constructor calldata.
    syscall_handler
        .increment_linear_factor_by(&SyscallSelector::Deploy, request.constructor_calldata.0.len());

    let (deployed_contract_address, call_info) = syscall_handler.base.deploy(
        request.class_hash,
        request.contract_address_salt,
        request.constructor_calldata,
        request.deploy_from_zero,
        remaining_gas,
    )?;
    let constructor_retdata =
        create_retdata_segment(vm, syscall_handler, &call_info.execution.retdata.0)?;
    syscall_handler.base.inner_calls.push(call_info);

    Ok(DeployResponse { contract_address: deployed_contract_address, constructor_retdata })
}

// EmitEvent syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct EmitEventRequest {
    pub content: EventContent,
}

impl SyscallRequest for EmitEventRequest {
    // The Cairo struct contains: `keys_len`, `keys`, `data_len`, `data`·
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<EmitEventRequest> {
        let keys =
            read_felt_array::<SyscallExecutionError>(vm, ptr)?.into_iter().map(EventKey).collect();
        let data = EventData(read_felt_array::<SyscallExecutionError>(vm, ptr)?);

        Ok(EmitEventRequest { content: EventContent { keys, data } })
    }
}

type EmitEventResponse = EmptyResponse;

pub fn exceeds_event_size_limit(
    versioned_constants: &VersionedConstants,
    n_emitted_events: usize,
    event: &EventContent,
) -> Result<(), EmitEventError> {
    let EventLimits { max_data_length, max_keys_length, max_n_emitted_events } =
        versioned_constants.tx_event_limits;
    if n_emitted_events > max_n_emitted_events {
        return Err(EmitEventError::ExceedsMaxNumberOfEmittedEvents {
            n_emitted_events,
            max_n_emitted_events,
        });
    }
    let keys_length = event.keys.len();
    if keys_length > max_keys_length {
        return Err(EmitEventError::ExceedsMaxKeysLength { keys_length, max_keys_length });
    }
    let data_length = event.data.0.len();
    if data_length > max_data_length {
        return Err(EmitEventError::ExceedsMaxDataLength { data_length, max_data_length });
    }

    Ok(())
}

pub fn emit_event(
    request: EmitEventRequest,
    _vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<EmitEventResponse> {
    syscall_handler.base.emit_event(request.content)?;
    Ok(EmitEventResponse {})
}

// GetBlockHash syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct GetBlockHashRequest {
    pub block_number: BlockNumber,
}

impl SyscallRequest for GetBlockHashRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<GetBlockHashRequest> {
        let felt = felt_from_ptr(vm, ptr)?;
        let block_number = BlockNumber(felt.to_u64().ok_or_else(|| {
            SyscallExecutionError::InvalidSyscallInput {
                input: felt,
                info: String::from("Block number must fit within 64 bits."),
            }
        })?);

        Ok(GetBlockHashRequest { block_number })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct GetBlockHashResponse {
    pub block_hash: BlockHash,
}

impl SyscallResponse for GetBlockHashResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_felt(vm, ptr, self.block_hash.0)?;
        Ok(())
    }
}

/// Returns the block hash of a given block_number.
/// Returns the expected block hash if the given block was created at least
/// [crate::abi::constants::STORED_BLOCK_HASH_BUFFER] blocks before the current block. Otherwise,
/// returns an error.
pub fn get_block_hash(
    request: GetBlockHashRequest,
    _vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<GetBlockHashResponse> {
    let block_hash = BlockHash(syscall_handler.base.get_block_hash(request.block_number.0)?);
    Ok(GetBlockHashResponse { block_hash })
}

// GetExecutionInfo syscall.

type GetExecutionInfoRequest = EmptyRequest;

#[derive(Debug, Eq, PartialEq)]
pub struct GetExecutionInfoResponse {
    pub execution_info_ptr: Relocatable,
}

impl SyscallResponse for GetExecutionInfoResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_maybe_relocatable(vm, ptr, self.execution_info_ptr)?;
        Ok(())
    }
}
pub fn get_execution_info(
    _request: GetExecutionInfoRequest,
    vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<GetExecutionInfoResponse> {
    let execution_info_ptr = syscall_handler.get_or_allocate_execution_info_segment(vm)?;

    Ok(GetExecutionInfoResponse { execution_info_ptr })
}

// LibraryCall syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct LibraryCallRequest {
    pub class_hash: ClassHash,
    pub function_selector: EntryPointSelector,
    pub calldata: Calldata,
}

impl SyscallRequest for LibraryCallRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<LibraryCallRequest> {
        let class_hash = ClassHash(felt_from_ptr(vm, ptr)?);
        let (function_selector, calldata) = read_call_params(vm, ptr)?;

        Ok(LibraryCallRequest { class_hash, function_selector, calldata })
    }
}

type LibraryCallResponse = CallContractResponse;

pub fn library_call(
    request: LibraryCallRequest,
    vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    remaining_gas: &mut u64,
) -> SyscallResult<LibraryCallResponse> {
    let entry_point = CallEntryPoint {
        class_hash: Some(request.class_hash),
        code_address: None,
        entry_point_type: EntryPointType::External,
        entry_point_selector: request.function_selector,
        calldata: request.calldata,
        // The call context remains the same in a library call.
        storage_address: syscall_handler.storage_address(),
        caller_address: syscall_handler.caller_address(),
        call_type: CallType::Delegate,
        // NOTE: this value might be overridden later on.
        initial_gas: *remaining_gas,
    };

    let retdata_segment = execute_inner_call(entry_point, vm, syscall_handler, remaining_gas)
        .map_err(|error| match error {
            SyscallExecutionError::Revert { .. } => error,
            _ => error.as_lib_call_execution_error(
                request.class_hash,
                syscall_handler.storage_address(),
                request.function_selector,
            ),
        })?;

    Ok(LibraryCallResponse { segment: retdata_segment })
}

// MetaTxV0 syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct MetaTxV0Request {
    pub contract_address: ContractAddress,
    pub entry_point_selector: EntryPointSelector,
    pub calldata: Calldata,
    pub signature: TransactionSignature,
}

impl SyscallRequest for MetaTxV0Request {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<MetaTxV0Request> {
        let contract_address = ContractAddress::try_from(felt_from_ptr(vm, ptr)?)?;
        let (entry_point_selector, calldata) = read_call_params(vm, ptr)?;
        let signature = TransactionSignature(read_felt_array::<SyscallExecutionError>(vm, ptr)?);

        Ok(MetaTxV0Request { contract_address, entry_point_selector, calldata, signature })
    }

    fn get_linear_factor_length(&self) -> usize {
        self.calldata.0.len()
    }
}

type MetaTxV0Response = CallContractResponse;

pub(crate) fn meta_tx_v0(
    request: MetaTxV0Request,
    vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    remaining_gas: &mut u64,
) -> SyscallResult<MetaTxV0Response> {
    if syscall_handler.is_validate_mode() {
        return Err(SyscallExecutionError::InvalidSyscallInExecutionMode {
            syscall_name: "meta_tx_v0".to_string(),
            execution_mode: syscall_handler.execution_mode(),
        });
    }

    // Increment the MetaTxV0 syscall's linear cost counter by the number of elements in the
    // calldata.
    syscall_handler
        .increment_linear_factor_by(&SyscallSelector::MetaTxV0, request.get_linear_factor_length());

    let storage_address = request.contract_address;
    let selector = request.entry_point_selector;
    let class_hash = syscall_handler.base.state.get_class_hash_at(storage_address)?;
    let entry_point = CallEntryPoint {
        class_hash: None,
        code_address: Some(storage_address),
        entry_point_type: EntryPointType::External,
        entry_point_selector: selector,
        calldata: request.calldata.clone(),
        storage_address,
        caller_address: ContractAddress::default(),
        call_type: CallType::Call,
        // NOTE: this value might be overridden later on.
        initial_gas: *remaining_gas,
    };

    let old_tx_context = syscall_handler.base.context.tx_context.clone();
    let only_query = old_tx_context.tx_info.only_query();

    // Compute meta-transaction hash.
    let transaction_hash = InvokeTransactionV0 {
        max_fee: Fee(0),
        signature: request.signature.clone(),
        contract_address: storage_address,
        entry_point_selector: selector,
        calldata: request.calldata,
    }
    .calculate_transaction_hash(
        &syscall_handler.base.context.tx_context.block_context.chain_info.chain_id,
        &signed_tx_version(&TransactionVersion::ZERO, &TransactionOptions { only_query }),
    )?;

    // Replace `tx_context`.
    let new_tx_info = TransactionInfo::Deprecated(DeprecatedTransactionInfo {
        common_fields: CommonAccountFields {
            transaction_hash,
            version: TransactionVersion::ZERO,
            signature: request.signature,
            nonce: Nonce(0.into()),
            sender_address: storage_address,
            only_query,
        },
        max_fee: Fee(0),
    });
    syscall_handler.base.context.tx_context = Arc::new(TransactionContext {
        block_context: old_tx_context.block_context.clone(),
        tx_info: new_tx_info,
    });

    let retdata_segment = execute_inner_call(entry_point, vm, syscall_handler, remaining_gas)
        .map_err(|error| match error {
            SyscallExecutionError::Revert { .. } => error,
            _ => {
                // TODO(lior): Change to meta-tx specific error.
                error.as_call_contract_execution_error(class_hash, storage_address, selector)
            }
        })?;

    // Restore the old `tx_context`.
    syscall_handler.base.context.tx_context = old_tx_context;

    Ok(MetaTxV0Response { segment: retdata_segment })
}

// ReplaceClass syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct ReplaceClassRequest {
    pub class_hash: ClassHash,
}

impl SyscallRequest for ReplaceClassRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<ReplaceClassRequest> {
        let class_hash = ClassHash(felt_from_ptr(vm, ptr)?);

        Ok(ReplaceClassRequest { class_hash })
    }
}

pub type ReplaceClassResponse = EmptyResponse;

pub fn replace_class(
    request: ReplaceClassRequest,
    _vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<ReplaceClassResponse> {
    syscall_handler.base.replace_class(request.class_hash)?;
    Ok(ReplaceClassResponse {})
}

// SendMessageToL1 syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct SendMessageToL1Request {
    pub message: MessageToL1,
}

impl SyscallRequest for SendMessageToL1Request {
    // The Cairo struct contains: `to_address`, `payload_size`, `payload`.
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<SendMessageToL1Request> {
        let to_address = EthAddress::try_from(felt_from_ptr(vm, ptr)?)?;
        let payload = L2ToL1Payload(read_felt_array::<SyscallExecutionError>(vm, ptr)?);

        Ok(SendMessageToL1Request { message: MessageToL1 { to_address, payload } })
    }
}

type SendMessageToL1Response = EmptyResponse;

pub fn send_message_to_l1(
    request: SendMessageToL1Request,
    _vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<SendMessageToL1Response> {
    syscall_handler.base.send_message_to_l1(request.message)?;
    Ok(SendMessageToL1Response {})
}

// TODO(spapini): Do something with address domain in read and write.
// StorageRead syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct StorageReadRequest {
    pub address_domain: Felt,
    pub address: StorageKey,
}

impl SyscallRequest for StorageReadRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<StorageReadRequest> {
        let address_domain = felt_from_ptr(vm, ptr)?;
        if address_domain != Felt::ZERO {
            return Err(SyscallExecutionError::InvalidAddressDomain { address_domain });
        }
        let address = StorageKey::try_from(felt_from_ptr(vm, ptr)?)?;
        Ok(StorageReadRequest { address_domain, address })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct StorageReadResponse {
    pub value: Felt,
}

impl SyscallResponse for StorageReadResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_felt(vm, ptr, self.value)?;
        Ok(())
    }
}

pub fn storage_read(
    request: StorageReadRequest,
    _vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<StorageReadResponse> {
    let value = syscall_handler.base.storage_read(request.address)?;
    Ok(StorageReadResponse { value })
}

// StorageWrite syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct StorageWriteRequest {
    pub address_domain: Felt,
    pub address: StorageKey,
    pub value: Felt,
}

impl SyscallRequest for StorageWriteRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<StorageWriteRequest> {
        let address_domain = felt_from_ptr(vm, ptr)?;
        if address_domain != Felt::ZERO {
            return Err(SyscallExecutionError::InvalidAddressDomain { address_domain });
        }
        let address = StorageKey::try_from(felt_from_ptr(vm, ptr)?)?;
        let value = felt_from_ptr(vm, ptr)?;
        Ok(StorageWriteRequest { address_domain, address, value })
    }
}

pub type StorageWriteResponse = EmptyResponse;

pub fn storage_write(
    request: StorageWriteRequest,
    _vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<StorageWriteResponse> {
    syscall_handler.base.storage_write(request.address, request.value)?;
    Ok(StorageWriteResponse {})
}

// Keccak syscall.

#[derive(Debug, Eq, PartialEq)]
pub struct KeccakRequest {
    pub input_start: Relocatable,
    pub input_end: Relocatable,
}

impl SyscallRequest for KeccakRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<KeccakRequest> {
        let input_start = vm.get_relocatable(*ptr)?;
        *ptr = (*ptr + 1)?;
        let input_end = vm.get_relocatable(*ptr)?;
        *ptr = (*ptr + 1)?;
        Ok(KeccakRequest { input_start, input_end })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct KeccakResponse {
    pub result_low: Felt,
    pub result_high: Felt,
}

impl SyscallResponse for KeccakResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_felt(vm, ptr, self.result_low)?;
        write_felt(vm, ptr, self.result_high)?;
        Ok(())
    }
}

pub fn keccak(
    request: KeccakRequest,
    vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    remaining_gas: &mut u64,
) -> SyscallResult<KeccakResponse> {
    let input_length = (request.input_end - request.input_start)?;

    let data = vm.get_integer_range(request.input_start, input_length)?;
    let data_u64: &[u64] = &data
        .iter()
        .map(|felt| {
            felt.to_u64().ok_or_else(|| SyscallExecutionError::InvalidSyscallInput {
                input: **felt,
                info: "Invalid input for the keccak syscall.".to_string(),
            })
        })
        .collect::<Result<Vec<u64>, _>>()?;

    let (state, n_rounds) = syscall_handler.base.keccak(data_u64, remaining_gas)?;

    // For the keccak system call we want to count the number of rounds rather than the number of
    // syscall invocations.
    syscall_handler.increment_syscall_count_by(&SyscallSelector::Keccak, n_rounds);

    Ok(KeccakResponse {
        result_low: (Felt::from(state[1]) * Felt::TWO.pow(64_u128)) + Felt::from(state[0]),
        result_high: (Felt::from(state[3]) * Felt::TWO.pow(64_u128)) + Felt::from(state[2]),
    })
}

// Sha256ProcessBlock syscall.
#[derive(Debug, Eq, PartialEq)]
pub struct Sha256ProcessBlockRequest {
    pub state_ptr: Relocatable,
    pub input_start: Relocatable,
}

impl SyscallRequest for Sha256ProcessBlockRequest {
    fn read(
        vm: &VirtualMachine,
        ptr: &mut Relocatable,
    ) -> SyscallResult<Sha256ProcessBlockRequest> {
        let state_start = vm.get_relocatable(*ptr)?;
        *ptr = (*ptr + 1)?;
        let input_start = vm.get_relocatable(*ptr)?;
        *ptr = (*ptr + 1)?;
        Ok(Sha256ProcessBlockRequest { state_ptr: state_start, input_start })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Sha256ProcessBlockResponse {
    pub state_ptr: Relocatable,
}

impl SyscallResponse for Sha256ProcessBlockResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_maybe_relocatable(vm, ptr, self.state_ptr)?;
        Ok(())
    }
}

pub fn sha256_process_block(
    request: Sha256ProcessBlockRequest,
    vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<Sha256ProcessBlockResponse> {
    const SHA256_BLOCK_SIZE: usize = 16;

    let data = vm.get_integer_range(request.input_start, SHA256_BLOCK_SIZE)?;
    const SHA256_STATE_SIZE: usize = 8;
    let prev_state = vm.get_integer_range(request.state_ptr, SHA256_STATE_SIZE)?;

    let data_as_bytes =
        sha2::digest::generic_array::GenericArray::from_exact_iter(data.iter().flat_map(|felt| {
            felt.to_bigint()
                .to_u32()
                .expect("libfunc should ensure the input is an [u32; 16].")
                .to_be_bytes()
        }))
        .expect(
            "u32.to_be_bytes() returns 4 bytes, and data.len() == 16. So data contains 64 bytes.",
        );

    let mut state_as_words: [u32; SHA256_STATE_SIZE] = core::array::from_fn(|i| {
        prev_state[i].to_bigint().to_u32().expect(
            "libfunc only accepts SHA256StateHandle which can only be created from an Array<u32>.",
        )
    });

    sha2::compress256(&mut state_as_words, &[data_as_bytes]);

    let segment = syscall_handler.sha256_segment_end_ptr.unwrap_or(vm.add_memory_segment());

    let response = segment;
    let data: Vec<MaybeRelocatable> =
        state_as_words.iter().map(|&arg| MaybeRelocatable::from(Felt::from(arg))).collect();

    syscall_handler.sha256_segment_end_ptr = Some(vm.load_data(segment, &data)?);

    Ok(Sha256ProcessBlockResponse { state_ptr: response })
}

// GetClassHashAt syscall.

pub(crate) type GetClassHashAtRequest = ContractAddress;
pub(crate) type GetClassHashAtResponse = ClassHash;

impl SyscallRequest for GetClassHashAtRequest {
    fn read(vm: &VirtualMachine, ptr: &mut Relocatable) -> SyscallResult<GetClassHashAtRequest> {
        let address = ContractAddress::try_from(felt_from_ptr(vm, ptr)?)?;
        Ok(address)
    }
}

impl SyscallResponse for GetClassHashAtResponse {
    fn write(self, vm: &mut VirtualMachine, ptr: &mut Relocatable) -> WriteResponseResult {
        write_felt(vm, ptr, *self)?;
        Ok(())
    }
}

pub(crate) fn get_class_hash_at(
    request: GetClassHashAtRequest,
    _vm: &mut VirtualMachine,
    syscall_handler: &mut SyscallHintProcessor<'_>,
    _remaining_gas: &mut u64,
) -> SyscallResult<GetClassHashAtResponse> {
    syscall_handler.base.get_class_hash_at(request)
}
