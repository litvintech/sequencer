use std::collections::BTreeMap;
use std::time::Duration;

use apollo_config::converters::deserialize_seconds_to_duration;
use apollo_config::dumping::{ser_param, SerializeConfig};
use apollo_config::{ParamPath, ParamPrivacyInput, SerializedParam};
use serde::{Deserialize, Serialize};
use validator::Validate;

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Validate)]
pub struct MempoolConfig {
    pub enable_fee_escalation: bool,
    // TODO(AlonH): consider adding validations; should be bounded?
    // Percentage increase for tip and max gas price to enable transaction replacement.
    pub fee_escalation_percentage: u8, // E.g., 10 for a 10% increase.
    // Time-to-live for transactions in the mempool, in seconds.
    // Transactions older than this value will be lazily removed.
    #[serde(deserialize_with = "deserialize_seconds_to_duration")]
    pub transaction_ttl: Duration,
    // Time to wait before allowing a Declare transaction to be returned in `get_txs`.
    // Declare transactions are delayed to allow other nodes sufficient time to compile them.
    #[serde(deserialize_with = "deserialize_seconds_to_duration")]
    pub declare_delay: Duration,
    // Number of latest committed blocks for which committed account nonces are preserved.
    pub committed_nonce_retention_block_count: usize,
    // The maximum size of the mempool, in bytes.
    pub capacity_in_bytes: u64,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        MempoolConfig {
            enable_fee_escalation: true,
            fee_escalation_percentage: 10,
            transaction_ttl: Duration::from_secs(60), // 1 minute.
            declare_delay: Duration::from_secs(1),
            committed_nonce_retention_block_count: 100,
            capacity_in_bytes: 1 << 30, // 1GB.
        }
    }
}

impl SerializeConfig for MempoolConfig {
    fn dump(&self) -> BTreeMap<ParamPath, SerializedParam> {
        BTreeMap::from_iter([
            ser_param(
                "enable_fee_escalation",
                &self.enable_fee_escalation,
                "If true, transactions can be replaced with higher fee transactions.",
                ParamPrivacyInput::Public,
            ),
            ser_param(
                "fee_escalation_percentage",
                &self.fee_escalation_percentage,
                "Percentage increase for tip and max gas price to enable transaction replacement.",
                ParamPrivacyInput::Public,
            ),
            ser_param(
                "transaction_ttl",
                &self.transaction_ttl.as_secs(),
                "Time-to-live for transactions in the mempool, in seconds.",
                ParamPrivacyInput::Public,
            ),
            ser_param(
                "declare_delay",
                &self.declare_delay.as_secs(),
                "Time to wait before allowing a Declare transaction to be returned, in seconds.",
                ParamPrivacyInput::Public,
            ),
            ser_param(
                "committed_nonce_retention_block_count",
                &self.committed_nonce_retention_block_count,
                "Number of latest committed blocks for which committed account nonces are \
                 retained.",
                ParamPrivacyInput::Public,
            ),
            ser_param(
                "capacity_in_bytes",
                &self.capacity_in_bytes,
                "Maximum size of the mempool, in bytes.",
                ParamPrivacyInput::Public,
            ),
        ])
    }
}
