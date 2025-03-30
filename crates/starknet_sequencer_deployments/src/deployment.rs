use std::net::{IpAddr, Ipv4Addr};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use serde::Serialize;
use starknet_api::core::ChainId;
use starknet_monitoring_endpoint::config::MonitoringEndpointConfig;
use starknet_sequencer_node::config::config_utils::{
    get_deployment_from_config_path,
    PresetConfig,
};

use crate::service::{DeploymentName, Service};

const DEPLOYMENT_IMAGE: &str = "ghcr.io/starkware-libs/sequencer/sequencer:dev";

pub struct DeploymentAndPreset {
    deployment: Deployment,
    // TODO(Tsabary): consider using PathBuf instead.
    dump_file_path: &'static str,
    base_app_config_file_path: &'static str,
}

impl DeploymentAndPreset {
    pub fn new(
        deployment: Deployment,
        dump_file_path: &'static str,
        base_app_config_file_path: &'static str,
    ) -> Self {
        Self { deployment, dump_file_path, base_app_config_file_path }
    }

    pub fn get_deployment(&self) -> &Deployment {
        &self.deployment
    }

    pub fn get_dump_file_path(&self) -> &'static str {
        self.dump_file_path
    }

    pub fn get_base_app_config_file_path(&self) -> &'static str {
        self.base_app_config_file_path
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Deployment {
    chain_id: ChainId,
    image: &'static str,
    application_config_subdir: String,
    #[serde(skip_serializing)]
    deployment_name: DeploymentName,
    services: Vec<Service>,
}

impl Deployment {
    pub fn new(chain_id: ChainId, deployment_name: DeploymentName) -> Self {
        let service_names = deployment_name.all_service_names();
        let services =
            service_names.iter().map(|service_name| service_name.create_service()).collect();
        Self {
            chain_id,
            image: DEPLOYMENT_IMAGE,
            application_config_subdir: deployment_name.get_path(),
            deployment_name,
            services,
        }
    }

    pub fn dump_application_config_files(&self, base_app_config_file_path: &str) {
        let deployment_base_app_config = get_deployment_from_config_path(base_app_config_file_path);

        let component_configs = self.deployment_name.get_component_configs(None);

        // Iterate over the service component configs
        for (service, component_config) in component_configs.iter() {
            let mut service_deployment_base_app_config = deployment_base_app_config.clone();

            let config_path =
                PathBuf::from(&self.application_config_subdir).join(service.get_config_file_path());

            let preset_config = PresetConfig {
                component_config: component_config.clone(),
                monitoring_endpoint_config: MonitoringEndpointConfig {
                    ip: IpAddr::from(Ipv4Addr::UNSPECIFIED),
                    // TODO(Tsabary): services use 8082 for their monitoring. Fix that as a const
                    // and ensure throughout the deployment code.
                    port: 8082,
                    collect_metrics: true,
                    collect_profiling_metrics: true,
                },
            };

            service_deployment_base_app_config.update_config_with_preset(preset_config.clone());
            service_deployment_base_app_config.dump_config_file(&config_path);
        }
    }

    #[cfg(test)]
    pub(crate) fn assert_application_configs_exist(&self) {
        for service in &self.services {
            // Concatenate paths.
            let subdir_path = Path::new(&self.application_config_subdir);
            let full_path = subdir_path.join(service.get_config_path());
            // Assert existence.
            assert!(full_path.exists(), "File does not exist: {:?}", full_path);
        }
    }
}
