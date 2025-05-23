use apollo_node::config::component_config::ComponentConfig;
use indexmap::IndexMap;
use serde::Serialize;
use strum::Display;
use strum_macros::{AsRefStr, EnumIter};

use crate::service::{
    GetComponentConfigs,
    Resource,
    Resources,
    Service,
    ServiceName,
    ServiceNameInner,
};

#[derive(Clone, Copy, Debug, Display, PartialEq, Eq, Hash, Serialize, AsRefStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum ConsolidatedNodeServiceName {
    Node,
}

impl From<ConsolidatedNodeServiceName> for ServiceName {
    fn from(service: ConsolidatedNodeServiceName) -> Self {
        ServiceName::ConsolidatedNode(service)
    }
}

impl GetComponentConfigs for ConsolidatedNodeServiceName {
    fn get_component_configs(_base_port: Option<u16>) -> IndexMap<ServiceName, ComponentConfig> {
        let mut component_config_map = IndexMap::new();
        component_config_map.insert(
            ServiceName::ConsolidatedNode(ConsolidatedNodeServiceName::Node),
            get_consolidated_config(),
        );
        component_config_map
    }
}

impl ServiceNameInner for ConsolidatedNodeServiceName {
    fn create_service(&self) -> Service {
        match self {
            ConsolidatedNodeServiceName::Node => Service::new(
                Into::<ServiceName>::into(*self),
                false,
                false,
                1,
                Some(32),
                Resources::new(Resource::new(1, 2), Resource::new(4, 8)),
                None,
            ),
        }
    }
}

fn get_consolidated_config() -> ComponentConfig {
    ComponentConfig::default()
}
