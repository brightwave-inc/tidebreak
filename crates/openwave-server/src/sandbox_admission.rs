//! Startup-time routing for newly admitted background agent runs.
//!
//! The container runtime is a detected capability, but checking it belongs at
//! process assembly rather than at every model-authored spawn. The resolved
//! execution location is copied into the turn worker and remains fixed for the
//! life of that server process.

use openwave_core::{AgentRunExecutionLocation, Config};

use crate::sandbox_docker::{DockerConfig, DockerSandboxBackend};

/// Resolve where this server process admits newly spawned background runs.
pub(crate) fn resolve_execution_location(config: &Config) -> AgentRunExecutionLocation {
    let backend = DockerSandboxBackend::new(docker_config(config));
    let backend_available = config.container_execution_enabled && backend.is_available();
    execution_location(config.container_execution_enabled, backend_available)
}

/// Apply the routing decision independently from capability detection.
fn execution_location(
    container_execution_enabled: bool,
    backend_available: bool,
) -> AgentRunExecutionLocation {
    if container_execution_enabled && backend_available {
        AgentRunExecutionLocation::Container
    } else {
        AgentRunExecutionLocation::InProcess
    }
}

/// Build the Docker configuration shared by admission detection and the
/// container worker service. An absent override retains the adapter's
/// documented placeholder image.
pub(crate) fn docker_config(config: &Config) -> DockerConfig {
    let mut docker = DockerConfig::default();
    if let Some(image) = &config.container_image {
        docker.image.clone_from(image);
    }
    docker
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_container_routing_requires_enablement_and_availability() {
        assert_eq!(
            execution_location(true, true),
            AgentRunExecutionLocation::Container
        );
        assert_eq!(
            execution_location(false, true),
            AgentRunExecutionLocation::InProcess
        );
        assert_eq!(
            execution_location(true, false),
            AgentRunExecutionLocation::InProcess
        );
    }

    #[test]
    fn sandbox_container_image_only_overrides_the_docker_default_when_set() {
        let mut config = Config::desktop("/data");
        assert_eq!(docker_config(&config).image, DockerConfig::default().image);

        config.container_image = Some("openwave-sandbox-agent:dev".into());
        assert_eq!(docker_config(&config).image, "openwave-sandbox-agent:dev");
    }
}
