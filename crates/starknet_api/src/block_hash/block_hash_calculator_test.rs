use rstest::rstest;
use starknet_types_core::felt::Felt;

use super::concat_counts;
use crate::block::{
    BlockHash,
    BlockHeaderWithoutHash,
    BlockNumber,
    BlockTimestamp,
    GasPricePerToken,
};
use crate::block_hash::block_hash_calculator::{
    calculate_block_commitments,
    calculate_block_hash,
    BlockHashVersion,
    BlockHeaderCommitments,
    TransactionHashingData,
};
use crate::block_hash::test_utils::{get_state_diff, get_transaction_output};
use crate::core::{
    ContractAddress,
    EventCommitment,
    GlobalRoot,
    PatriciaKey,
    ReceiptCommitment,
    SequencerContractAddress,
    StateDiffCommitment,
    TransactionCommitment,
};
use crate::data_availability::L1DataAvailabilityMode;
use crate::hash::PoseidonHash;
use crate::transaction::fields::TransactionSignature;
use crate::{felt, tx_hash};

/// Macro to test if changing any field in the header or commitments
/// results a change in the block hash.
/// The macro clones the original header and commitments, modifies each specified field,
/// and asserts that the block hash changes as a result.
/// Allows excluding specific fields that aren't part of the hash.
macro_rules! test_hash_changes {
    (
        BlockHeaderWithoutHash { $($header_field:ident: $header_value:expr),* $(,)? },
        BlockHeaderCommitments { $($commitments_field:ident: $commitments_value:expr),* $(,)? },
        exclude_header_fields = [ $( $excluded_field:ident ),* $(,)? ]
    ) => {
        {
            let excluded_fields = vec![$(stringify!($excluded_field)),*];

            let header = BlockHeaderWithoutHash {
                l1_da_mode: L1DataAvailabilityMode::Blob,
                starknet_version: BlockHashVersion::V0_13_4.into(),
                $($header_field: $header_value),*
            };
            let commitments = BlockHeaderCommitments {
                $($commitments_field: $commitments_value),*
            };
            let original_hash = calculate_block_hash(header.clone(), commitments.clone()).unwrap();

            $(
                let mut modified_header = header.clone();
                modified_header.$header_field = Default::default();
                let new_hash = calculate_block_hash(modified_header, commitments.clone()).unwrap();
                if excluded_fields.contains(&stringify!($header_field)) {
                    // Hash should not change.
                    assert_eq!(original_hash, new_hash, concat!("Hash should NOT change when ", stringify!($header_field), " is modified"));
                }
                else{
                    // Hash should change.
                    assert_ne!(original_hash, new_hash, concat!("Hash should change when ", stringify!($header_field), " is modified"));
                }
            )*

            $(
                // Test changing the field in the commitments.
                let mut modified_commitments = commitments.clone();
                modified_commitments.$commitments_field = Default::default();
                let new_hash = calculate_block_hash(header.clone(), modified_commitments).unwrap();
                assert_ne!(original_hash, new_hash, concat!("Hash should change when ", stringify!($commitments_field), " is modified"));
            )*
        }
    };
}

#[rstest]
fn test_block_hash_regression(
    #[values(BlockHashVersion::V0_13_2, BlockHashVersion::V0_13_4)]
    block_hash_version: BlockHashVersion,
) {
    let block_header = BlockHeaderWithoutHash {
        block_number: BlockNumber(1_u64),
        state_root: GlobalRoot(Felt::from(2_u8)),
        sequencer: SequencerContractAddress(ContractAddress(PatriciaKey::from(3_u8))),
        timestamp: BlockTimestamp(4),
        l1_da_mode: L1DataAvailabilityMode::Blob,
        l1_gas_price: GasPricePerToken { price_in_fri: 6_u8.into(), price_in_wei: 7_u8.into() },
        l1_data_gas_price: GasPricePerToken {
            price_in_fri: 10_u8.into(),
            price_in_wei: 9_u8.into(),
        },
        l2_gas_price: GasPricePerToken { price_in_fri: 11_u8.into(), price_in_wei: 12_u8.into() },
        l2_gas_consumed: 13,
        next_l2_gas_price: 14,
        starknet_version: block_hash_version.clone().into(),
        parent_hash: BlockHash(Felt::from(11_u8)),
    };
    let transactions_data = vec![TransactionHashingData {
        transaction_signature: TransactionSignature(vec![Felt::TWO, Felt::THREE]),
        transaction_output: get_transaction_output(),
        transaction_hash: tx_hash!(1),
    }];

    let state_diff = get_state_diff();
    let block_commitments = calculate_block_commitments(
        &transactions_data,
        &state_diff,
        block_header.l1_da_mode,
        &block_hash_version.to_owned().into(),
    );

    let expected_hash = match block_hash_version {
        BlockHashVersion::V0_13_2 => {
            felt!("0xe248d6ce583f8fa48d1d401d4beb9ceced3733e38d8eacb0d8d3669a7d901c")
        }
        BlockHashVersion::V0_13_4 => {
            felt!("0x3d6174623c812f5dc03fa3faa07c42c06fd90ad425629ee5f39e149df65c3ca")
        }
    };

    assert_eq!(
        BlockHash(expected_hash),
        calculate_block_hash(block_header, block_commitments).unwrap()
    );
}

#[test]
fn l2_gas_price_pre_v0_13_4() {
    let block_header = {
        |l2_gas_price: u8| BlockHeaderWithoutHash {
            l2_gas_price: GasPricePerToken {
                price_in_fri: l2_gas_price.into(),
                price_in_wei: l2_gas_price.into(),
            },
            starknet_version: BlockHashVersion::V0_13_2.into(),
            ..Default::default()
        }
    };

    assert_eq!(
        calculate_block_hash(block_header(1), BlockHeaderCommitments::default()),
        calculate_block_hash(block_header(2), BlockHeaderCommitments::default())
    );
}

#[test]
fn concat_counts_test() {
    let concated = concat_counts(4, 3, 2, L1DataAvailabilityMode::Blob);
    let expected_felt = felt!("0x0000000000000004000000000000000300000000000000028000000000000000");
    assert_eq!(concated, expected_felt)
}

/// Test that if one of the input to block hash changes, the hash changes.
#[test]
fn change_field_of_hash_input() {
    // Set non-default values for the header and the commitments fields. Test that changing any of
    // these fields changes the hash.
    test_hash_changes!(
        BlockHeaderWithoutHash {
            parent_hash: BlockHash(Felt::ONE),
            block_number: BlockNumber(1),
            l1_gas_price: GasPricePerToken { price_in_fri: 1_u8.into(), price_in_wei: 1_u8.into() },
            l1_data_gas_price: GasPricePerToken {
                price_in_fri: 1_u8.into(),
                price_in_wei: 1_u8.into(),
            },
            l2_gas_price: GasPricePerToken { price_in_fri: 1_u8.into(), price_in_wei: 1_u8.into() },
            l2_gas_consumed: 1,
            next_l2_gas_price: 1,
            state_root: GlobalRoot(Felt::ONE),
            sequencer: SequencerContractAddress(ContractAddress::from(1_u128)),
            timestamp: BlockTimestamp(1)
        },
        BlockHeaderCommitments {
            transaction_commitment: TransactionCommitment(Felt::ONE),
            event_commitment: EventCommitment(Felt::ONE),
            receipt_commitment: ReceiptCommitment(Felt::ONE),
            state_diff_commitment: StateDiffCommitment(PoseidonHash(Felt::ONE)),
            concatenated_counts: Felt::ONE
        },
        // Excluding l2_gas_consumed, l2_gas_consumed as they're not currently included in the
        // block hash.
        // TODO(Ayelet): Remove these fields after 0.14.0, once they are included in the hash.
        exclude_header_fields = [l2_gas_consumed, next_l2_gas_price]
    );
    // TODO(Aviv, 10/06/2024): add tests that changes the first hash input, and the const zero.
}
