use blockifier::state::errors::StateError;
use cairo_vm::hint_processor::hint_processor_definition::HintExtension;
use cairo_vm::types::errors::math_errors::MathError;
use cairo_vm::types::relocatable::MaybeRelocatable;
use cairo_vm::vm::errors::exec_scope_errors::ExecScopeError;
use cairo_vm::vm::errors::hint_errors::HintError as VmHintError;
use cairo_vm::vm::errors::memory_errors::MemoryError;
use cairo_vm::vm::errors::vm_errors::VirtualMachineError;
use num_bigint::{BigUint, TryFromBigIntError};
use starknet_api::block::BlockNumber;
use starknet_api::core::ClassHash;
use starknet_api::executable_transaction::Transaction;
use starknet_api::StarknetApiError;
use starknet_types_core::felt::Felt;

use crate::hint_processor::execution_helper::ExecutionHelperError;
use crate::hint_processor::os_logger::OsLoggerError;
use crate::hints::enum_definition::AllHints;
use crate::hints::hint_implementation::kzg::utils::FftError;
use crate::hints::vars::{Const, Ids};
use crate::vm_utils::VmUtilsError;

#[derive(Debug, thiserror::Error)]
pub enum OsHintError {
    #[error("Assertion failed: {message}")]
    AssertionFailed { message: String },
    #[error("Unexpectedly assigned leaf bytecode segment.")]
    AssignedLeafBytecodeSegment,
    #[error("Block number is probably < {stored_block_hash_buffer}.")]
    BlockNumberTooSmall { stored_block_hash_buffer: Felt },
    #[error("{id:?} value {felt} is not a boolean.")]
    BooleanIdExpected { id: Ids, felt: Felt },
    #[error("Failed to convert {variant:?} felt value {felt:?} to type {ty}: {reason:?}.")]
    ConstConversion { variant: Const, felt: Felt, ty: String, reason: String },
    #[error("Tried to iterate past the end of {item_type}.")]
    EndOfIterator { item_type: String },
    #[error(transparent)]
    ExecutionScopes(#[from] ExecScopeError),
    #[error("{id:?} value {felt} is not a bit.")]
    ExpectedBit { id: Ids, felt: Felt },
    #[error(transparent)]
    Fft(#[from] FftError),
    #[error(transparent)]
    ExecutionHelper(#[from] ExecutionHelperError),
    #[error("Failed to convert {variant:?} felt value {felt:?} to type {ty}: {reason:?}.")]
    IdsConversion { variant: Ids, felt: Felt, ty: String, reason: String },
    #[error(
        "Inconsistent block numbers: {actual}, {expected}. The constant STORED_BLOCK_HASH_BUFFER \
         is probably out of sync."
    )]
    InconsistentBlockNumber { actual: BlockNumber, expected: BlockNumber },
    #[error("Inconsistent storage value. Actual: {actual}, expected: {expected}.")]
    InconsistentValue { actual: Felt, expected: Felt },
    #[error(transparent)]
    Math(#[from] MathError),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error("No bytecode segment structure for class hash: {0:?}.")]
    MissingBytecodeSegmentStructure(ClassHash),
    #[error("Hint {hint:?} has no nondet offset.")]
    MissingOffsetForHint { hint: AllHints },
    #[error("No preimage found for value {0:?}.")]
    MissingPreimage(Felt),
    #[error("No (selected) builtin found at address {builtin} (attempted decoding: {decoded:?}).")]
    MissingSelectedBuiltinPtr { builtin: MaybeRelocatable, decoded: Option<String> },
    #[error(
        "No (unselected) builtin found at address {builtin} (attempted decoding: {decoded:?})."
    )]
    MissingUnselectedBuiltinPtr { builtin: MaybeRelocatable, decoded: Option<String> },
    #[error(transparent)]
    OsLogger(#[from] OsLoggerError),
    #[error("{error:?} for json value {value}.")]
    SerdeJsonDeserialize { error: serde_json::Error, value: serde_json::value::Value },
    #[error(transparent)]
    StarknetApi(#[from] StarknetApiError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("Convert {n_bits} bits for {type_name}.")]
    StatelessCompressionOverflow { n_bits: usize, type_name: String },
    #[error(transparent)]
    TryFromBigUint(#[from] TryFromBigIntError<BigUint>),
    #[error("Unexpected tx type: {0:?}.")]
    UnexpectedTxType(Transaction),
    #[error("Unknown hint string: {0}")]
    UnknownHint(String),
    #[error(transparent)]
    Vm(#[from] VirtualMachineError),
    #[error(transparent)]
    VmHint(#[from] VmHintError),
    #[error(transparent)]
    VmUtils(#[from] VmUtilsError),
}

/// `OsHintError` and the VM's `HintError` must have conversions in both directions, as execution
/// can pass back and forth between the VM and the OS hint processor; errors should propagate.
// TODO(Dori): Consider replicating the blockifier's mechanism and keeping structured error data,
//   instead of converting to string.
impl From<OsHintError> for VmHintError {
    fn from(error: OsHintError) -> Self {
        Self::CustomHint(format!("{error}").into())
    }
}

pub type OsHintResult = Result<(), OsHintError>;
pub type OsHintExtensionResult = Result<HintExtension, OsHintError>;
