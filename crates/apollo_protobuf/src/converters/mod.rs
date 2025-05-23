mod class;
pub(crate) mod common;
// TODO(matan): Internalize once we remove the dependency on the protobuf crate.
pub mod consensus;
mod event;
mod header;
mod receipt;
pub mod rpc_transaction;
mod state_diff;
#[cfg(test)]
mod test_instances;
mod transaction;

use prost::DecodeError;
use starknet_api::compression_utils::CompressionError;

#[derive(thiserror::Error, PartialEq, Debug, Clone)]
pub enum ProtobufConversionError {
    #[error("Type `{type_description}` got out of range value {value_as_str}")]
    OutOfRangeValue { type_description: &'static str, value_as_str: String },
    #[error("Missing field `{field_description}`")]
    MissingField { field_description: &'static str },
    #[error("Type `{type_description}` should be {num_expected} bytes but it got {value:?}.")]
    BytesDataLengthMismatch { type_description: &'static str, num_expected: usize, value: Vec<u8> },
    #[error("Type `{type_description}` got unexpected enum variant {value_as_str}")]
    WrongEnumVariant {
        type_description: &'static str,
        value_as_str: String,
        expected: &'static str,
    },
    #[error(transparent)]
    DecodeError(#[from] DecodeError),
    /// For CompressionError and serde_json::Error we put the string of the error instead of the
    /// original error because the original error does not derive ParitalEq and Clone Traits
    #[error("Unexpected compression utils error: {0}")]
    CompressionError(String),
    #[error("Unexpected serde_json error: {0}")]
    SerdeJsonError(String),
}

impl From<CompressionError> for ProtobufConversionError {
    fn from(error: CompressionError) -> Self {
        ProtobufConversionError::CompressionError(error.to_string())
    }
}

impl From<serde_json::Error> for ProtobufConversionError {
    fn from(error: serde_json::Error) -> Self {
        ProtobufConversionError::SerdeJsonError(error.to_string())
    }
}

#[macro_export]
macro_rules! auto_impl_into_and_try_from_vec_u8 {
    ($T:ty, $ProtobufT:ty) => {
        impl From<$T> for Vec<u8> {
            fn from(value: $T) -> Self {
                let protobuf_value = <$ProtobufT>::from(value);
                protobuf_value.encode_to_vec()
            }
        }
        $crate::auto_impl_try_from_vec_u8!($T, $ProtobufT);
    };
}

// TODO(shahak): Remove this macro once all types implement both directions.
#[macro_export]
macro_rules! auto_impl_try_from_vec_u8 {
    ($T:ty, $ProtobufT:ty) => {
        impl TryFrom<Vec<u8>> for $T {
            type Error = ProtobufConversionError;
            fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
                let protobuf_value = <$ProtobufT>::decode(&value[..])?;
                <$T>::try_from(protobuf_value)
            }
        }
    };
}
