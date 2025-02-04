use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use futures::future::join_all;
use futures::TryFutureExt;
use mempool_test_utils::starknet_api_test_utils::{AccountId, MultiAccountTransactionGenerator};
use papyrus_network::network_manager::test_utils::create_connected_network_configs;
use papyrus_storage::StorageConfig;
use starknet_api::block::BlockNumber;
use starknet_api::core::Nonce;
use starknet_api::rpc_transaction::RpcTransaction;
use starknet_api::transaction::TransactionHash;
use starknet_infra_utils::test_utils::{
    AvailablePorts,
    TestIdentifier,
    MAX_NUMBER_OF_INSTANCES_PER_TEST,
};
use starknet_monitoring_endpoint::test_utils::MonitoringClient;
use starknet_sequencer_metrics::metric_definitions;
use starknet_sequencer_node::config::component_config::ComponentConfig;
use starknet_sequencer_node::config::component_execution_config::{
    ActiveComponentExecutionConfig,
    ReactiveComponentExecutionConfig,
};
use starknet_sequencer_node::test_utils::node_runner::spawn_run_node;
use tokio::task::JoinHandle;
use tracing::info;

use crate::integration_test_setup::{ExecutableSetup, NodeExecutionId};
use crate::monitoring_utils::await_execution;
use crate::utils::{
    create_chain_info,
    create_consensus_manager_configs_from_network_configs,
    create_mempool_p2p_configs,
    create_state_sync_configs,
    send_account_txs,
    TestScenario,
};

/// Holds the component configs for a set of sequencers, composing a single sequencer node.
struct NodeComponentConfigs {
    component_configs: Vec<ComponentConfig>,
    batcher_index: usize,
    http_server_index: usize,
}

impl NodeComponentConfigs {
    fn new(
        component_configs: Vec<ComponentConfig>,
        batcher_index: usize,
        http_server_index: usize,
    ) -> Self {
        Self { component_configs, batcher_index, http_server_index }
    }

    fn into_iter(self) -> impl Iterator<Item = ComponentConfig> {
        self.component_configs.into_iter()
    }

    fn len(&self) -> usize {
        self.component_configs.len()
    }

    fn get_batcher_index(&self) -> usize {
        self.batcher_index
    }

    fn get_http_server_index(&self) -> usize {
        self.http_server_index
    }
}

pub struct NodeSetup {
    executables: Vec<ExecutableSetup>,
    batcher_index: usize,
    http_server_index: usize,
}

impl NodeSetup {
    pub fn new(
        executables: Vec<ExecutableSetup>,
        batcher_index: usize,
        http_server_index: usize,
    ) -> Self {
        let len = executables.len();

        fn validate_index(index: usize, len: usize, label: &str) {
            assert!(
                index < len,
                "{} index {} is out of range. There are {} executables.",
                label,
                index,
                len
            );
        }

        validate_index(batcher_index, len, "Batcher");
        validate_index(http_server_index, len, "HTTP server");

        Self { executables, batcher_index, http_server_index }
    }

    async fn send_rpc_tx_fn(&self, rpc_tx: RpcTransaction) -> TransactionHash {
        self.executables[self.http_server_index].assert_add_tx_success(rpc_tx).await
    }

    pub fn batcher_monitoring_client(&self) -> &MonitoringClient {
        &self.executables[self.batcher_index].monitoring_client
    }

    pub fn get_executables(&self) -> &Vec<ExecutableSetup> {
        &self.executables
    }

    pub fn get_batcher_index(&self) -> usize {
        self.batcher_index
    }

    pub fn get_http_server_index(&self) -> usize {
        self.http_server_index
    }

    pub fn run(self) -> RunningNode {
        let executable_handles = self
            .get_executables()
            .iter()
            .map(|executable| {
                info!("Running {}.", executable.node_execution_id);
                spawn_run_node(
                    executable.node_config_path.clone(),
                    executable.node_execution_id.into(),
                )
            })
            .collect::<Vec<_>>();

        RunningNode { node_setup: self, executable_handles }
    }
    pub fn get_node_index(&self) -> Option<usize> {
        self.executables.first().map(|executable| executable.node_execution_id.get_node_index())
    }
}

pub struct RunningNode {
    node_setup: NodeSetup,
    executable_handles: Vec<JoinHandle<()>>,
}

impl RunningNode {
    async fn await_alive(&self, interval: u64, max_attempts: usize) {
        let await_alive_tasks = self.node_setup.executables.iter().map(|executable| {
            let result = executable.monitoring_client.await_alive(interval, max_attempts);
            result.unwrap_or_else(|_| {
                panic!("Executable {:?} should be alive.", executable.node_execution_id)
            })
        });

        join_all(await_alive_tasks).await;
    }
}

pub struct IntegrationTestManager {
    idle_nodes: HashMap<usize, NodeSetup>,
    running_nodes: HashMap<usize, RunningNode>,
}

impl IntegrationTestManager {
    pub fn new(idle_nodes: Vec<NodeSetup>, running_nodes: Vec<RunningNode>) -> Self {
        let idle_nodes_map = create_map(idle_nodes, |node| node.get_node_index());
        let running_nodes_map =
            create_map(running_nodes, |running_node| running_node.node_setup.get_node_index());

        Self { idle_nodes: idle_nodes_map, running_nodes: running_nodes_map }
    }
    pub async fn run(&mut self, nodes_to_run: HashSet<usize>) {
        info!("Running specified nodes.");

        nodes_to_run.into_iter().for_each(|index| {
            let node_setup = self
                .idle_nodes
                .remove(&index)
                .unwrap_or_else(|| panic!("Node {} does not exist in idle_nodes.", index));
            info!("Running node {}.", index);
            let running_node = node_setup.run();
            assert!(
                self.running_nodes.insert(index, running_node).is_none(),
                "Node {} is already in the running map.",
                index
            );
        });

        // Wait for the nodes to start
        self.await_alive(5000, 50).await;
    }

    /// This function tests and verifies the integration of the transaction flow.
    ///
    /// # Parameters
    /// - `expected_initial_value`: The initial amount of batched transactions. This represents the
    ///   starting state before any transactions are sent.
    /// - `n_txs`: The number of transactions that will be sent during the test. After the test
    ///   completes, the nonce in the batcher's storage is expected to be `expected_initial_value +
    ///   n_txs`.
    /// - `tx_generator`: A transaction generator used to create transactions for testing.
    /// - `sender_account`: The ID of the account sending the transactions.
    /// - `expected_block_number`: The block number up to which execution should be awaited.
    ///
    /// The function verifies the initial state, runs the test with the given number of
    /// transactions, waits for execution to complete, and then verifies the final state.
    pub async fn test_and_verify(
        &mut self,
        tx_generator: &mut MultiAccountTransactionGenerator,
        test_scenario: impl TestScenario,
        sender_account: AccountId,
        wait_for_block: BlockNumber,
    ) {
        // Verify the initial state
        self.verify_txs_accepted(tx_generator, sender_account).await;
        self.run_integration_test_simulator(tx_generator, &test_scenario, sender_account).await;
        self.await_execution(wait_for_block).await;
        self.verify_txs_accepted(tx_generator, sender_account).await;
    }

    async fn await_alive(&self, interval: u64, max_attempts: usize) {
        let await_alive_tasks =
            self.running_nodes.values().map(|node| node.await_alive(interval, max_attempts));

        join_all(await_alive_tasks).await;
    }

    async fn send_rpc_tx_fn(&self, rpc_tx: RpcTransaction) -> TransactionHash {
        let node_0 = self.running_nodes.get(&0).expect("Node 0 should running.");
        node_0.node_setup.send_rpc_tx_fn(rpc_tx).await
    }

    /// Returns the sequencer index of the first running node and its monitoring client.
    fn running_batcher_monitoring_client(&self) -> (usize, &MonitoringClient) {
        let running_node = self.running_nodes.get(&0).expect("Node 0 should be running.");
        (0, running_node.node_setup.batcher_monitoring_client())
    }

    pub fn shutdown_nodes(&mut self, nodes_to_shutdown: HashSet<usize>) {
        nodes_to_shutdown.into_iter().for_each(|index| {
            let running_node = self
                .running_nodes
                .remove(&index)
                .unwrap_or_else(|| panic!("Node {} is not in the running map.", index));
            running_node.executable_handles.iter().for_each(|handle| {
                assert!(!handle.is_finished(), "Node {} should still be running.", index);
                handle.abort();
            });
            assert!(
                self.idle_nodes.insert(index, running_node.node_setup).is_none(),
                "Node {} is already in the idle map.",
                index
            );
            info!("Node {} has been shut down.", index);
        });
    }

    pub async fn run_integration_test_simulator(
        &self,
        tx_generator: &mut MultiAccountTransactionGenerator,
        test_scenario: &impl TestScenario,
        sender_account: AccountId,
    ) {
        info!("Running integration test simulator.");
        let send_rpc_tx_fn = &mut |rpc_tx| self.send_rpc_tx_fn(rpc_tx);

        let n_txs = test_scenario.n_txs();
        info!("Sending {n_txs} txs.");
        let tx_hashes =
            send_account_txs(tx_generator, sender_account, test_scenario, send_rpc_tx_fn).await;
        assert_eq!(tx_hashes.len(), n_txs);
    }

    pub async fn await_execution(&self, expected_block_number: BlockNumber) {
        let running_node =
            self.running_nodes.iter().next().expect("At least one node should be running").1;
        await_execution(&running_node.node_setup, expected_block_number).await;
    }

    pub async fn verify_txs_accepted(
        &self,
        tx_generator: &mut MultiAccountTransactionGenerator,
        sender_account: AccountId,
    ) {
        let (sequencer_idx, monitoring_client) = self.running_batcher_monitoring_client();
        let account = tx_generator.account_with_id(sender_account);
        let expected_n_batched_txs = nonce_to_usize(account.get_nonce());
        info!("Verifying that sequencer {sequencer_idx} got {expected_n_batched_txs} batched txs.");
        let n_batched_txs = monitoring_client
            .get_metric::<usize>(metric_definitions::BATCHED_TRANSACTIONS.get_name())
            .await
            .expect("Failed to get batched txs metric.");
        assert_eq!(
            n_batched_txs, expected_n_batched_txs,
            "Sequencer {sequencer_idx} got an unexpected number of batched txs. Expected \
             {expected_n_batched_txs} got {n_batched_txs}"
        );
    }
}

fn nonce_to_usize(nonce: Nonce) -> usize {
    let prefixed_hex = nonce.0.to_hex_string();
    let unprefixed_hex = prefixed_hex.split_once("0x").unwrap().1;
    usize::from_str_radix(unprefixed_hex, 16).unwrap()
}

pub(crate) async fn get_sequencer_setup_configs(
    tx_generator: &MultiAccountTransactionGenerator,
    num_of_consolidated_nodes: usize,
    num_of_distributed_nodes: usize,
) -> (Vec<NodeSetup>, HashSet<usize>) {
    let test_unique_id = TestIdentifier::EndToEndIntegrationTest;

    // TODO(Nadin): Assign a dedicated set of available ports to each sequencer.
    let mut available_ports =
        AvailablePorts::new(test_unique_id.into(), MAX_NUMBER_OF_INSTANCES_PER_TEST - 1);

    let node_component_configs: Vec<NodeComponentConfigs> = {
        let mut combined = Vec::new();
        // Create elements in place.
        combined.extend(create_consolidated_sequencer_configs(num_of_consolidated_nodes));
        combined.extend(create_distributed_node_configs(
            &mut available_ports,
            num_of_distributed_nodes,
        ));
        combined
    };

    info!("Creating node configurations.");
    let chain_info = create_chain_info();
    let accounts = tx_generator.accounts();
    let n_distributed_sequencers = node_component_configs
        .iter()
        .map(|node_component_config| node_component_config.len())
        .sum();

    // TODO(Nadin): Refactor to avoid directly mutating vectors

    let mut consensus_manager_configs = create_consensus_manager_configs_from_network_configs(
        create_connected_network_configs(available_ports.get_next_ports(n_distributed_sequencers)),
        node_component_configs.len(),
    );

    let node_indices: HashSet<usize> = (0..node_component_configs.len()).collect();

    // TODO(Nadin): define the test storage here and pass it to the create_state_sync_configs and to
    // the ExecutableSetup
    let mut state_sync_configs = create_state_sync_configs(
        StorageConfig::default(),
        available_ports.get_next_ports(n_distributed_sequencers),
    );

    let mut mempool_p2p_configs = create_mempool_p2p_configs(
        chain_info.chain_id.clone(),
        available_ports.get_next_ports(n_distributed_sequencers),
    );

    // TODO(Nadin/Tsabary): There are redundant p2p configs here, as each distributed node
    // needs only one of them, but the current setup creates one per part. Need to refactor.

    let mut nodes = Vec::new();
    let mut global_index = 0;

    for (node_index, node_component_config) in node_component_configs.into_iter().enumerate() {
        let mut executables = Vec::new();
        let batcher_index = node_component_config.get_batcher_index();
        let http_server_index = node_component_config.get_http_server_index();

        for (executable_index, executable_component_config) in
            node_component_config.into_iter().enumerate()
        {
            let node_execution_id = NodeExecutionId::new(node_index, executable_index);
            let consensus_manager_config = consensus_manager_configs.remove(0);
            let mempool_p2p_config = mempool_p2p_configs.remove(0);
            let state_sync_config = state_sync_configs.remove(0);
            let chain_info = chain_info.clone();
            executables.push(
                ExecutableSetup::new(
                    accounts.to_vec(),
                    node_execution_id,
                    chain_info,
                    consensus_manager_config,
                    mempool_p2p_config,
                    state_sync_config,
                    AvailablePorts::new(test_unique_id.into(), global_index.try_into().unwrap()),
                    executable_component_config.clone(),
                )
                .await,
            );
            global_index += 1;
        }
        nodes.push(NodeSetup::new(executables, batcher_index, http_server_index));
    }

    (nodes, node_indices)
}

/// Generates configurations for a specified number of distributed sequencer nodes,
/// each consisting of an HTTP component configuration and a non-HTTP component configuration.
/// returns a vector of vectors, where each inner vector contains the two configurations.
fn create_distributed_node_configs(
    available_ports: &mut AvailablePorts,
    distributed_sequencers_num: usize,
) -> Vec<NodeComponentConfigs> {
    std::iter::repeat_with(|| {
        let gateway_socket = available_ports.get_next_local_host_socket();
        let mempool_socket = available_ports.get_next_local_host_socket();
        let mempool_p2p_socket = available_ports.get_next_local_host_socket();
        let state_sync_socket = available_ports.get_next_local_host_socket();

        NodeComponentConfigs::new(
            vec![
                get_http_container_config(
                    gateway_socket,
                    mempool_socket,
                    mempool_p2p_socket,
                    state_sync_socket,
                ),
                get_non_http_container_config(
                    gateway_socket,
                    mempool_socket,
                    mempool_p2p_socket,
                    state_sync_socket,
                ),
            ],
            // batcher is in executable index 1.
            1,
            // http server is in executable index 0.
            0,
        )
    })
    .take(distributed_sequencers_num)
    .collect()
}

fn create_consolidated_sequencer_configs(
    num_of_consolidated_nodes: usize,
) -> Vec<NodeComponentConfigs> {
    // Both batcher and http server are in executable index 0.
    std::iter::repeat_with(|| NodeComponentConfigs::new(vec![ComponentConfig::default()], 0, 0))
        .take(num_of_consolidated_nodes)
        .collect()
}

// TODO(Nadin/Tsabary): find a better name for this function.
fn get_http_container_config(
    gateway_socket: SocketAddr,
    mempool_socket: SocketAddr,
    mempool_p2p_socket: SocketAddr,
    state_sync_socket: SocketAddr,
) -> ComponentConfig {
    let mut config = ComponentConfig::disabled();
    config.http_server = ActiveComponentExecutionConfig::default();
    let local_url = "127.0.0.1".to_string();
    config.gateway = ReactiveComponentExecutionConfig::local_with_remote_enabled(
        local_url.clone(),
        gateway_socket.ip(),
        gateway_socket.port(),
    );
    config.mempool = ReactiveComponentExecutionConfig::local_with_remote_enabled(
        local_url.clone(),
        mempool_socket.ip(),
        mempool_socket.port(),
    );
    config.mempool_p2p = ReactiveComponentExecutionConfig::local_with_remote_enabled(
        local_url.clone(),
        mempool_p2p_socket.ip(),
        mempool_p2p_socket.port(),
    );
    config.state_sync = ReactiveComponentExecutionConfig::remote(
        local_url.clone(),
        state_sync_socket.ip(),
        state_sync_socket.port(),
    );
    config.monitoring_endpoint = ActiveComponentExecutionConfig::default();
    config
}

fn get_non_http_container_config(
    gateway_socket: SocketAddr,
    mempool_socket: SocketAddr,
    mempool_p2p_socket: SocketAddr,
    state_sync_socket: SocketAddr,
) -> ComponentConfig {
    let local_url = "127.0.0.1".to_string();
    ComponentConfig {
        http_server: ActiveComponentExecutionConfig::disabled(),
        monitoring_endpoint: Default::default(),
        gateway: ReactiveComponentExecutionConfig::remote(
            local_url.clone(),
            gateway_socket.ip(),
            gateway_socket.port(),
        ),
        mempool: ReactiveComponentExecutionConfig::remote(
            local_url.clone(),
            mempool_socket.ip(),
            mempool_socket.port(),
        ),
        mempool_p2p: ReactiveComponentExecutionConfig::remote(
            local_url.clone(),
            mempool_p2p_socket.ip(),
            mempool_p2p_socket.port(),
        ),
        state_sync: ReactiveComponentExecutionConfig::local_with_remote_enabled(
            local_url.clone(),
            state_sync_socket.ip(),
            state_sync_socket.port(),
        ),
        ..ComponentConfig::default()
    }
}

fn create_map<T, K, F>(items: Vec<T>, key_extractor: F) -> HashMap<K, T>
where
    F: Fn(&T) -> Option<K>,
    K: std::hash::Hash + Eq,
{
    items.into_iter().filter_map(|item| key_extractor(&item).map(|key| (key, item))).collect()
}
