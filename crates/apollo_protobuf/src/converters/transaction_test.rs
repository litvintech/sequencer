use apollo_test_utils::{get_rng, GetTestInstance};
use lazy_static::lazy_static;
use rand::random;
use starknet_api::block::GasPrice;
use starknet_api::execution_resources::{Builtin, ExecutionResources, GasAmount, GasVector};
use starknet_api::transaction::fields::{AllResourceBounds, ResourceBounds, ValidResourceBounds};
use starknet_api::transaction::{
    DeclareTransaction,
    DeclareTransactionOutput,
    DeployAccountTransaction,
    DeployAccountTransactionOutput,
    DeployTransactionOutput,
    FullTransaction,
    InvokeTransaction,
    InvokeTransactionOutput,
    L1HandlerTransactionOutput,
    Transaction as StarknetApiTransaction,
    TransactionOutput,
};
use starknet_api::tx_hash;

use crate::sync::DataOrFin;

macro_rules! create_transaction_output {
    ($tx_output_type:ty, $tx_output_enum_variant:ident) => {{
        let mut rng = get_rng();
        let mut transaction_output = <$tx_output_type>::get_test_instance(&mut rng);
        transaction_output.execution_resources = EXECUTION_RESOURCES.clone();
        transaction_output.events = vec![];
        TransactionOutput::$tx_output_enum_variant(transaction_output)
    }};
}

#[test]
fn convert_l1_handler_transaction_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction = starknet_api::transaction::L1HandlerTransaction::get_test_instance(&mut rng);
    let transaction = StarknetApiTransaction::L1Handler(transaction);

    let transaction_output = create_transaction_output!(L1HandlerTransactionOutput, L1Handler);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_deploy_transaction_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction = starknet_api::transaction::DeployTransaction::get_test_instance(&mut rng);
    let transaction = StarknetApiTransaction::Deploy(transaction);

    let transaction_output = create_transaction_output!(DeployTransactionOutput, Deploy);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_declare_transaction_v0_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction =
        starknet_api::transaction::DeclareTransactionV0V1::get_test_instance(&mut rng);
    let transaction = StarknetApiTransaction::Declare(DeclareTransaction::V0(transaction));

    let transaction_output = create_transaction_output!(DeclareTransactionOutput, Declare);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_declare_transaction_v1_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction =
        starknet_api::transaction::DeclareTransactionV0V1::get_test_instance(&mut rng);
    let transaction = StarknetApiTransaction::Declare(DeclareTransaction::V1(transaction));

    let transaction_output = create_transaction_output!(DeclareTransactionOutput, Declare);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_declare_transaction_v2_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction = starknet_api::transaction::DeclareTransactionV2::get_test_instance(&mut rng);
    let transaction = StarknetApiTransaction::Declare(DeclareTransaction::V2(transaction));

    let transaction_output = create_transaction_output!(DeclareTransactionOutput, Declare);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_declare_transaction_v3_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let mut transaction =
        starknet_api::transaction::DeclareTransactionV3::get_test_instance(&mut rng);
    transaction.resource_bounds = *RESOURCE_BOUNDS_MAPPING;
    let transaction = StarknetApiTransaction::Declare(DeclareTransaction::V3(transaction));

    let transaction_output = create_transaction_output!(DeclareTransactionOutput, Declare);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_invoke_transaction_v0_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction = starknet_api::transaction::InvokeTransactionV0::get_test_instance(&mut rng);
    let transaction = StarknetApiTransaction::Invoke(InvokeTransaction::V0(transaction));

    let transaction_output = create_transaction_output!(InvokeTransactionOutput, Invoke);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_invoke_transaction_v1_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction = starknet_api::transaction::InvokeTransactionV1::get_test_instance(&mut rng);
    let transaction = StarknetApiTransaction::Invoke(InvokeTransaction::V1(transaction));

    let transaction_output = create_transaction_output!(InvokeTransactionOutput, Invoke);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_invoke_transaction_v3_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let mut transaction =
        starknet_api::transaction::InvokeTransactionV3::get_test_instance(&mut rng);
    transaction.resource_bounds = *RESOURCE_BOUNDS_MAPPING;
    let transaction = StarknetApiTransaction::Invoke(InvokeTransaction::V3(transaction));

    let transaction_output = create_transaction_output!(InvokeTransactionOutput, Invoke);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_deploy_account_transaction_v1_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let transaction =
        starknet_api::transaction::DeployAccountTransactionV1::get_test_instance(&mut rng);
    let transaction =
        StarknetApiTransaction::DeployAccount(DeployAccountTransaction::V1(transaction));

    let transaction_output =
        create_transaction_output!(DeployAccountTransactionOutput, DeployAccount);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn convert_deploy_account_transaction_v3_to_vec_u8_and_back() {
    let mut rng = get_rng();
    let mut transaction =
        starknet_api::transaction::DeployAccountTransactionV3::get_test_instance(&mut rng);
    transaction.resource_bounds = *RESOURCE_BOUNDS_MAPPING;
    let transaction =
        StarknetApiTransaction::DeployAccount(DeployAccountTransaction::V3(transaction));

    let transaction_output =
        create_transaction_output!(DeployAccountTransactionOutput, DeployAccount);
    assert_transaction_to_vec_u8_and_back(transaction, transaction_output);
}

#[test]
fn fin_transaction_to_bytes_and_back() {
    let bytes_data = Vec::<u8>::from(DataOrFin::<FullTransaction>(None));

    let res_data = DataOrFin::<FullTransaction>::try_from(bytes_data).unwrap();
    assert!(res_data.0.is_none());
}

fn assert_transaction_to_vec_u8_and_back(
    transaction: StarknetApiTransaction,
    transaction_output: TransactionOutput,
) {
    let random_transaction_hash = tx_hash!(random::<u64>());
    let data = DataOrFin(Some(FullTransaction {
        transaction,
        transaction_output,
        transaction_hash: random_transaction_hash,
    }));
    let bytes_data = Vec::<u8>::from(data.clone());
    let res_data = DataOrFin::try_from(bytes_data).unwrap();
    assert_eq!(data, res_data);
}

lazy_static! {
    static ref EXECUTION_RESOURCES: ExecutionResources = ExecutionResources {
        steps: 0,
        builtin_instance_counter: std::collections::HashMap::from([
            (Builtin::RangeCheck, 1),
            (Builtin::Pedersen, 2),
            (Builtin::Poseidon, 3),
            (Builtin::EcOp, 4),
            (Builtin::Ecdsa, 5),
            (Builtin::Bitwise, 6),
            (Builtin::Keccak, 7),
            (Builtin::SegmentArena, 0),
        ]),
        memory_holes: 0,
        da_gas_consumed: GasVector::default(),
        gas_consumed: GasVector::default(),
    };
    static ref RESOURCE_BOUNDS_MAPPING: ValidResourceBounds =
        ValidResourceBounds::AllResources(AllResourceBounds {
            l1_gas: ResourceBounds {
                max_amount: GasAmount(0x5),
                max_price_per_unit: GasPrice(0x6)
            },
            l2_gas: ResourceBounds {
                max_amount: GasAmount(0x500),
                max_price_per_unit: GasPrice(0x600)
            },
            l1_data_gas: ResourceBounds {
                max_amount: GasAmount(0x30),
                max_price_per_unit: GasPrice(0x30)
            }
        });
}
