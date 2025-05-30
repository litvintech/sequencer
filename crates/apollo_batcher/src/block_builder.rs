use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use apollo_batcher_types::batcher_types::ProposalCommitment;
use apollo_class_manager_types::transaction_converter::{
    TransactionConverter,
    TransactionConverterError,
    TransactionConverterResult,
    TransactionConverterTrait,
};
use apollo_class_manager_types::SharedClassManagerClient;
use apollo_config::dumping::{append_sub_config_name, ser_param, SerializeConfig};
use apollo_config::{ParamPath, ParamPrivacyInput, SerializedParam};
use apollo_state_reader::papyrus_state::{ClassReader, PapyrusReader};
use apollo_storage::StorageReader;
use async_trait::async_trait;
use blockifier::blockifier::config::TransactionExecutorConfig;
use blockifier::blockifier::transaction_executor::{
    BlockExecutionSummary,
    TransactionExecutor,
    TransactionExecutorError as BlockifierTransactionExecutorError,
    TransactionExecutorResult,
};
use blockifier::blockifier_versioned_constants::{VersionedConstants, VersionedConstantsOverrides};
use blockifier::bouncer::{BouncerConfig, BouncerWeights};
use blockifier::context::{BlockContext, ChainInfo};
use blockifier::state::cached_state::CommitmentStateDiff;
use blockifier::state::contract_class_manager::ContractClassManager;
use blockifier::state::errors::StateError;
use blockifier::transaction::objects::TransactionExecutionInfo;
use blockifier::transaction::transaction_execution::Transaction as BlockifierTransaction;
use indexmap::{IndexMap, IndexSet};
#[cfg(test)]
use mockall::automock;
use serde::{Deserialize, Serialize};
use starknet_api::block::{BlockHashAndNumber, BlockInfo};
use starknet_api::block_hash::state_diff_hash::calculate_state_diff_hash;
use starknet_api::consensus_transaction::InternalConsensusTransaction;
use starknet_api::core::{ContractAddress, Nonce};
use starknet_api::execution_resources::GasAmount;
use starknet_api::state::ThinStateDiff;
use starknet_api::transaction::TransactionHash;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, error, info, trace};

use crate::metrics::FULL_BLOCKS;
use crate::transaction_executor::TransactionExecutorTrait;
use crate::transaction_provider::{NextTxs, TransactionProvider, TransactionProviderError};

#[derive(Debug, Error)]
pub enum BlockBuilderError {
    #[error(transparent)]
    BlockifierStateError(#[from] StateError),
    #[error(transparent)]
    ExecutorError(#[from] BlockifierTransactionExecutorError),
    #[error(transparent)]
    GetTransactionError(#[from] TransactionProviderError),
    #[error(transparent)]
    StreamTransactionsError(
        #[from] tokio::sync::mpsc::error::SendError<InternalConsensusTransaction>,
    ),
    #[error(transparent)]
    FailOnError(FailOnErrorCause),
    #[error("The block builder was aborted.")]
    Aborted,
    #[error(transparent)]
    TransactionConverterError(#[from] TransactionConverterError),
}

pub type BlockBuilderResult<T> = Result<T, BlockBuilderError>;

#[derive(Debug, Error)]
pub enum FailOnErrorCause {
    #[error("Block is full")]
    BlockFull,
    #[error("Deadline has been reached")]
    DeadlineReached,
    #[error("Transaction failed: {0}")]
    TransactionFailed(BlockifierTransactionExecutorError),
}

#[cfg_attr(test, derive(Clone))]
#[derive(Debug, PartialEq)]
pub struct BlockExecutionArtifacts {
    // Note: The execution_infos must be ordered to match the order of the transactions in the
    // block.
    pub execution_data: BlockTransactionExecutionData,
    pub commitment_state_diff: CommitmentStateDiff,
    pub compressed_state_diff: Option<CommitmentStateDiff>,
    pub bouncer_weights: BouncerWeights,
    pub l2_gas_used: GasAmount,
}

impl BlockExecutionArtifacts {
    pub fn address_to_nonce(&self) -> HashMap<ContractAddress, Nonce> {
        HashMap::from_iter(
            self.commitment_state_diff
                .address_to_nonce
                .iter()
                .map(|(address, nonce)| (*address, *nonce)),
        )
    }

    pub fn tx_hashes(&self) -> HashSet<TransactionHash> {
        HashSet::from_iter(self.execution_data.execution_infos.keys().copied())
    }

    pub fn thin_state_diff(&self) -> ThinStateDiff {
        // TODO(Ayelet): Remove the clones.
        let commitment_state_diff = self.commitment_state_diff.clone();
        ThinStateDiff {
            deployed_contracts: commitment_state_diff.address_to_class_hash,
            storage_diffs: commitment_state_diff.storage_updates,
            declared_classes: commitment_state_diff.class_hash_to_compiled_class_hash,
            nonces: commitment_state_diff.address_to_nonce,
            // TODO(AlonH): Remove this when the structure of storage diffs changes.
            deprecated_declared_classes: Vec::new(),
        }
    }

    pub fn commitment(&self) -> ProposalCommitment {
        ProposalCommitment {
            state_diff_commitment: calculate_state_diff_hash(&self.thin_state_diff()),
        }
    }
}

/// The BlockBuilderTrait is responsible for building a new block from transactions provided by the
/// tx_provider. The block building will stop at time deadline.
/// The transactions that were added to the block will be streamed to the output_content_sender.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait BlockBuilderTrait: Send {
    async fn build_block(&mut self) -> BlockBuilderResult<BlockExecutionArtifacts>;
}

pub struct BlockBuilderExecutionParams {
    pub deadline: tokio::time::Instant,
    pub fail_on_err: bool,
}

pub struct BlockBuilder {
    // TODO(Yael 14/10/2024): make the executor thread safe and delete this mutex.
    executor: Arc<Mutex<dyn TransactionExecutorTrait>>,
    tx_provider: Box<dyn TransactionProvider>,
    output_content_sender: Option<tokio::sync::mpsc::UnboundedSender<InternalConsensusTransaction>>,
    abort_signal_receiver: tokio::sync::oneshot::Receiver<()>,
    transaction_converter: TransactionConverter,

    // Parameters to configure the block builder behavior.
    tx_chunk_size: usize,
    execution_params: BlockBuilderExecutionParams,
}

impl BlockBuilder {
    pub fn new(
        executor: impl TransactionExecutorTrait + 'static,
        tx_provider: Box<dyn TransactionProvider>,
        output_content_sender: Option<
            tokio::sync::mpsc::UnboundedSender<InternalConsensusTransaction>,
        >,
        abort_signal_receiver: tokio::sync::oneshot::Receiver<()>,
        transaction_converter: TransactionConverter,
        tx_chunk_size: usize,
        execution_params: BlockBuilderExecutionParams,
    ) -> Self {
        let executor = Arc::new(Mutex::new(executor));
        Self {
            executor,
            tx_provider,
            output_content_sender,
            abort_signal_receiver,
            transaction_converter,
            tx_chunk_size,
            execution_params,
        }
    }
}

#[async_trait]
impl BlockBuilderTrait for BlockBuilder {
    async fn build_block(&mut self) -> BlockBuilderResult<BlockExecutionArtifacts> {
        let mut block_is_full = false;
        let mut l2_gas_used = GasAmount::ZERO;
        let mut execution_data = BlockTransactionExecutionData::default();
        // TODO(yael 6/10/2024): delete the timeout condition once the executor has a timeout
        while !block_is_full {
            if tokio::time::Instant::now() >= self.execution_params.deadline {
                info!("Block builder deadline reached.");
                if self.execution_params.fail_on_err {
                    return Err(BlockBuilderError::FailOnError(FailOnErrorCause::DeadlineReached));
                }
                break;
            }
            if self.abort_signal_receiver.try_recv().is_ok() {
                info!("Received abort signal. Aborting block builder.");
                return Err(BlockBuilderError::Aborted);
            }
            let next_txs =
                self.tx_provider.get_txs(self.tx_chunk_size).await.inspect_err(|err| {
                    error!("Failed to get transactions from the transaction provider: {}", err);
                })?;
            let next_tx_chunk = match next_txs {
                NextTxs::Txs(txs) => txs,
                NextTxs::End => break,
            };
            debug!("Got {} transactions from the transaction provider.", next_tx_chunk.len());
            if next_tx_chunk.is_empty() {
                // TODO(AlonH): Consider what is the best sleep duration.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }

            let tx_convert_futures = next_tx_chunk.iter().map(|tx| async {
                convert_to_executable_blockifier_tx(&self.transaction_converter, tx.clone()).await
            });
            let executor_input_chunk = futures::future::try_join_all(tx_convert_futures).await?;

            // Execute the transactions on a separate thread pool to avoid blocking the executor
            // while waiting on `block_on` calls.
            let executor = self.executor.clone();
            let results = tokio::task::spawn_blocking(move || {
                executor
                    .try_lock() // Acquire the lock in a sync manner.
                    .expect("Only a single task should use the executor.")
                    .add_txs_to_block(executor_input_chunk.as_slice())
            })
            .await
            .expect("Failed to spawn blocking executor task.");
            trace!("Transaction execution results: {:?}", results);
            block_is_full = collect_execution_results_and_stream_txs(
                next_tx_chunk,
                results,
                &mut l2_gas_used,
                &mut execution_data,
                &self.output_content_sender,
                self.execution_params.fail_on_err,
            )
            .await?;
        }
        let BlockExecutionSummary { state_diff, compressed_state_diff, bouncer_weights } =
            self.executor.lock().await.close_block()?;
        Ok(BlockExecutionArtifacts {
            execution_data,
            commitment_state_diff: state_diff,
            compressed_state_diff,
            bouncer_weights,
            l2_gas_used,
        })
    }
}

async fn convert_to_executable_blockifier_tx(
    transaction_converter: &TransactionConverter,
    tx: InternalConsensusTransaction,
) -> TransactionConverterResult<BlockifierTransaction> {
    let executable_tx =
        transaction_converter.convert_internal_consensus_tx_to_executable_tx(tx).await?;
    Ok(BlockifierTransaction::new_for_sequencing(executable_tx))
}

/// Returns true if the block is full and should be closed, false otherwise.
async fn collect_execution_results_and_stream_txs(
    tx_chunk: Vec<InternalConsensusTransaction>,
    results: Vec<TransactionExecutorResult<TransactionExecutionInfo>>,
    l2_gas_used: &mut GasAmount,
    execution_data: &mut BlockTransactionExecutionData,
    output_content_sender: &Option<
        tokio::sync::mpsc::UnboundedSender<InternalConsensusTransaction>,
    >,
    fail_on_err: bool,
) -> BlockBuilderResult<bool> {
    assert!(
        results.len() <= tx_chunk.len(),
        "The number of results should be less than or equal to the number of transactions."
    );
    let mut block_is_full = false;
    // If the block is full, we won't get an error from the executor. We will just get only the
    // results of the transactions that were executed before the block was full.
    // see [TransactionExecutor::execute_txs].
    if results.len() < tx_chunk.len() {
        info!("Block is full.");
        if fail_on_err {
            return Err(BlockBuilderError::FailOnError(FailOnErrorCause::BlockFull));
        } else {
            FULL_BLOCKS.increment(1);
            block_is_full = true;
        }
    }
    for (input_tx, result) in tx_chunk.into_iter().zip(results.into_iter()) {
        match result {
            Ok(tx_execution_info) => {
                *l2_gas_used = l2_gas_used
                    .checked_add(tx_execution_info.receipt.gas.l2_gas)
                    .expect("Total L2 gas overflow.");

                let tx_hash = input_tx.tx_hash();
                execution_data.execution_infos.insert(tx_hash, tx_execution_info);
                if let InternalConsensusTransaction::L1Handler(_) = input_tx {
                    execution_data.accepted_l1_handler_tx_hashes.insert(tx_hash);
                }

                if let Some(output_content_sender) = output_content_sender {
                    output_content_sender.send(input_tx)?;
                }
            }
            // TODO(yael 18/9/2024): add timeout error handling here once this
            // feature is added.
            Err(err) => {
                debug!("Transaction {} failed with error: {}.", input_tx.tx_hash(), err);
                if fail_on_err {
                    return Err(BlockBuilderError::FailOnError(
                        FailOnErrorCause::TransactionFailed(err),
                    ));
                }
                execution_data.rejected_tx_hashes.insert(input_tx.tx_hash());
            }
        }
    }
    Ok(block_is_full)
}

pub struct BlockMetadata {
    pub block_info: BlockInfo,
    pub retrospective_block_hash: Option<BlockHashAndNumber>,
}

// Type definitions for the abort channel required to abort the block builder.
pub type AbortSignalSender = tokio::sync::oneshot::Sender<()>;

/// The BlockBuilderFactoryTrait is responsible for creating a new block builder.
#[cfg_attr(test, automock)]
pub trait BlockBuilderFactoryTrait: Send + Sync {
    // TODO(noamsp): Investigate and remove this clippy warning.
    #[allow(clippy::result_large_err)]
    fn create_block_builder(
        &self,
        block_metadata: BlockMetadata,
        execution_params: BlockBuilderExecutionParams,
        tx_provider: Box<dyn TransactionProvider>,
        output_content_sender: Option<
            tokio::sync::mpsc::UnboundedSender<InternalConsensusTransaction>,
        >,
        runtime: tokio::runtime::Handle,
    ) -> BlockBuilderResult<(Box<dyn BlockBuilderTrait>, AbortSignalSender)>;

    fn take_class_cache_miss_counter(&self) -> u64;

    fn take_class_cache_hit_counter(&self) -> u64;
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BlockBuilderConfig {
    pub chain_info: ChainInfo,
    pub execute_config: TransactionExecutorConfig,
    pub bouncer_config: BouncerConfig,
    pub tx_chunk_size: usize,
    pub versioned_constants_overrides: VersionedConstantsOverrides,
}

impl Default for BlockBuilderConfig {
    fn default() -> Self {
        Self {
            // TODO(AlonH): update the default values once the actual values are known.
            chain_info: ChainInfo::default(),
            execute_config: TransactionExecutorConfig::default(),
            bouncer_config: BouncerConfig::default(),
            tx_chunk_size: 100,
            versioned_constants_overrides: VersionedConstantsOverrides::default(),
        }
    }
}

impl SerializeConfig for BlockBuilderConfig {
    fn dump(&self) -> BTreeMap<ParamPath, SerializedParam> {
        let mut dump = append_sub_config_name(self.chain_info.dump(), "chain_info");
        dump.append(&mut append_sub_config_name(self.execute_config.dump(), "execute_config"));
        dump.append(&mut append_sub_config_name(self.bouncer_config.dump(), "bouncer_config"));
        dump.append(&mut BTreeMap::from([ser_param(
            "tx_chunk_size",
            &self.tx_chunk_size,
            "Number of transactions in each request from the tx_provider.",
            ParamPrivacyInput::Public,
        )]));
        dump.append(&mut append_sub_config_name(
            self.versioned_constants_overrides.dump(),
            "versioned_constants_overrides",
        ));
        dump
    }
}

pub struct BlockBuilderFactory {
    pub block_builder_config: BlockBuilderConfig,
    pub storage_reader: StorageReader,
    pub contract_class_manager: ContractClassManager,
    pub class_manager_client: SharedClassManagerClient,
}

impl BlockBuilderFactory {
    // TODO(noamsp): Investigate and remove this clippy warning.
    #[allow(clippy::result_large_err)]
    fn preprocess_and_create_transaction_executor(
        &self,
        block_metadata: BlockMetadata,
        runtime: tokio::runtime::Handle,
    ) -> BlockBuilderResult<TransactionExecutor<PapyrusReader>> {
        let height = block_metadata.block_info.block_number;
        let block_builder_config = self.block_builder_config.clone();
        let versioned_constants = VersionedConstants::get_versioned_constants(
            block_builder_config.versioned_constants_overrides,
        );
        let block_context = BlockContext::new(
            block_metadata.block_info,
            block_builder_config.chain_info,
            versioned_constants,
            block_builder_config.bouncer_config,
        );

        let class_reader = Some(ClassReader { reader: self.class_manager_client.clone(), runtime });
        let state_reader = PapyrusReader::new_with_class_manager(
            self.storage_reader.clone(),
            height,
            self.contract_class_manager.clone(),
            class_reader,
        );

        let executor = TransactionExecutor::pre_process_and_create(
            state_reader,
            block_context,
            block_metadata.retrospective_block_hash,
            block_builder_config.execute_config,
        )?;

        Ok(executor)
    }
}

impl BlockBuilderFactoryTrait for BlockBuilderFactory {
    fn create_block_builder(
        &self,
        block_metadata: BlockMetadata,
        execution_params: BlockBuilderExecutionParams,
        tx_provider: Box<dyn TransactionProvider>,
        output_content_sender: Option<
            tokio::sync::mpsc::UnboundedSender<InternalConsensusTransaction>,
        >,
        runtime: tokio::runtime::Handle,
    ) -> BlockBuilderResult<(Box<dyn BlockBuilderTrait>, AbortSignalSender)> {
        let executor = self.preprocess_and_create_transaction_executor(block_metadata, runtime)?;
        let (abort_signal_sender, abort_signal_receiver) = tokio::sync::oneshot::channel();
        let transaction_converter = TransactionConverter::new(
            self.class_manager_client.clone(),
            self.block_builder_config.chain_info.chain_id.clone(),
        );
        let block_builder = Box::new(BlockBuilder::new(
            executor,
            tx_provider,
            output_content_sender,
            abort_signal_receiver,
            transaction_converter,
            self.block_builder_config.tx_chunk_size,
            execution_params,
        ));
        Ok((block_builder, abort_signal_sender))
    }

    fn take_class_cache_hit_counter(&self) -> u64 {
        self.contract_class_manager.take_cache_hit_counter()
    }

    fn take_class_cache_miss_counter(&self) -> u64 {
        self.contract_class_manager.take_cache_miss_counter()
    }
}

/// Supplementary information for use by downstream services.
#[cfg_attr(test, derive(Clone))]
#[derive(Debug, Default, PartialEq)]
pub struct BlockTransactionExecutionData {
    pub execution_infos: IndexMap<TransactionHash, TransactionExecutionInfo>,
    pub rejected_tx_hashes: HashSet<TransactionHash>,
    pub accepted_l1_handler_tx_hashes: IndexSet<TransactionHash>,
}
