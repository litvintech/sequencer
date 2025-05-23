use cairo_lang_starknet_classes::contract_class::ContractEntryPoint;
use cairo_native::starknet::{ResourceBounds, TxV2Info};
use starknet_api::core::EntryPointSelector;
use starknet_api::transaction::fields::{AllResourceBounds, Resource, ValidResourceBounds};
use starknet_types_core::felt::Felt;

use crate::transaction::objects::CurrentTransactionInfo;

pub fn contract_entrypoint_to_entrypoint_selector(
    entrypoint: &ContractEntryPoint,
) -> EntryPointSelector {
    EntryPointSelector(Felt::from(&entrypoint.selector))
}

pub fn encode_str_as_felts(msg: &str) -> Vec<Felt> {
    const CHUNK_SIZE: usize = 32;

    let data = msg.as_bytes().chunks(CHUNK_SIZE - 1);
    let mut encoding = vec![Felt::default(); data.len()];
    for (i, data_chunk) in data.enumerate() {
        let mut chunk = [0_u8; CHUNK_SIZE];
        chunk[1..data_chunk.len() + 1].copy_from_slice(data_chunk);
        encoding[i] = Felt::from_bytes_be(&chunk);
    }
    encoding
}

pub fn default_tx_v2_info() -> TxV2Info {
    TxV2Info {
        version: Default::default(),
        account_contract_address: Default::default(),
        max_fee: 0,
        signature: vec![],
        transaction_hash: Default::default(),
        chain_id: Default::default(),
        nonce: Default::default(),
        resource_bounds: vec![],
        tip: 0,
        paymaster_data: vec![],
        nonce_data_availability_mode: 0,
        fee_data_availability_mode: 0,
        account_deployment_data: vec![],
    }
}

pub fn calculate_resource_bounds(
    tx_info: &CurrentTransactionInfo,
    exclude_l1_data_gas: bool,
) -> Vec<ResourceBounds> {
    let l1_gas_bounds = tx_info.resource_bounds.get_l1_bounds();
    let l2_gas_bounds = tx_info.resource_bounds.get_l2_bounds();
    let mut res = vec![
        ResourceBounds {
            resource: Felt::from_hex(Resource::L1Gas.to_hex()).unwrap(),
            max_amount: l1_gas_bounds.max_amount.0,
            max_price_per_unit: l1_gas_bounds.max_price_per_unit.0,
        },
        ResourceBounds {
            resource: Felt::from_hex(Resource::L2Gas.to_hex()).unwrap(),
            max_amount: l2_gas_bounds.max_amount.0,
            max_price_per_unit: l2_gas_bounds.max_price_per_unit.0,
        },
    ];
    match tx_info.resource_bounds {
        ValidResourceBounds::L1Gas(_) => return res,
        ValidResourceBounds::AllResources(AllResourceBounds { l1_data_gas, .. }) => {
            if !exclude_l1_data_gas {
                res.push(ResourceBounds {
                    resource: Felt::from_hex(Resource::L1DataGas.to_hex()).unwrap(),
                    max_amount: l1_data_gas.max_amount.0,
                    max_price_per_unit: l1_data_gas.max_price_per_unit.0,
                })
            }
        }
    }
    res
}
