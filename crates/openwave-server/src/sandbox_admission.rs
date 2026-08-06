//! Startup-time routing for newly admitted background agent runs, and the
//! fail-closed detached-admission gate (issue #824).
//!
//! The container runtime is a detected capability, but checking it belongs at
//! process assembly rather than at every model-authored spawn. The resolved
//! execution location is copied into the turn worker and remains fixed for the
//! life of that server process.

use std::sync::Arc;

use openwave_code_execution::CodeExecutionProviderKind;
use openwave_core::{AgentRunExecutionLocation, Config, SandboxAdmissionMode};
use openwave_sandbox_protocol::provisioning::SandboxBackend;

use crate::sandbox_docker::{DockerConfig, DockerSandboxBackend};
use crate::scoped_model_token::{GatewayScopedTokenIssuer, ScopedModelTokenIssuer};

/// Container backend and admission route resolved once during server startup.
pub(crate) struct SandboxContainerAdmission {
    pub(crate) backend: Arc<DockerSandboxBackend>,
    pub(crate) execution_location: AgentRunExecutionLocation,
}

impl SandboxContainerAdmission {
    /// Whether both configuration and runtime availability selected containers.
    pub(crate) fn enabled(&self) -> bool {
        self.execution_location == AgentRunExecutionLocation::Container
    }
}

/// Resolve the backend and fixed location for this server process.
pub(crate) fn resolve(config: &Config) -> SandboxContainerAdmission {
    let docker = docker_config(config);
    let backend = Arc::new(DockerSandboxBackend::new(docker));
    let backend_available = config.container_execution_enabled && backend.is_available();
    let execution_location =
        execution_location(config.container_execution_enabled, backend_available);
    if execution_location == AgentRunExecutionLocation::Container
        && !backend.verifies_image_integrity()
    {
        // The development fallback (or an operator's mutable-tag override):
        // containers still run, but nothing verifies what the ref resolves to.
        tracing::warn!("sandbox container image is not digest-pinned; running it unverified");
    }
    SandboxContainerAdmission {
        backend,
        execution_location,
    }
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
/// container worker service. An absent override retains the adapter's default
/// image (the published documents image by digest, or the local development
/// build while no digest is pinned); an explicit override replaces the whole
/// ref, digest included, so a mutable-tag override is honestly unverified.
pub(crate) fn docker_config(config: &Config) -> DockerConfig {
    let mut docker = DockerConfig::default();
    if let Some(image) = &config.container_image {
        docker.image.clone_from(image);
    }
    docker
}

/// The facts the detached-admission decision consumes, gathered by the caller
/// at run admission.
///
/// Every field defaults to the unmet state, so a caller that cannot establish
/// a precondition does not have to remember to deny it — constructing the
/// struct with `..Default::default()` is already the closed position. The
/// preconditions are the ones docs/sandbox-providers.md fixes for detached
/// admission; each maps one-to-one onto a [`DetachedAdmissionDenial`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct DetachedPreconditions {
    /// A short-lived, scoped, revocable model token can be minted for this
    /// run (a gateway session or a provider that issues them). A long-lived
    /// provider API key never satisfies this.
    pub(crate) scoped_model_token_available: bool,
    /// The backend enforces a sandbox lifetime cap from outside the sandbox,
    /// set at provisioning to no more than the run's absolute deadline.
    pub(crate) external_lifetime_cap: bool,
    /// The agent image is verified within the topology's trust root.
    pub(crate) image_verified: bool,
    /// The run's tool surface can reach a host-authority operation (work that
    /// needs consent mid-run). Such a run is refused detached admission at
    /// the start, not parked indefinitely in the middle.
    pub(crate) host_authority_tool_surface: bool,
    /// The run carries third-party credentials (connected-app or similar).
    /// The scoped model token itself is deliberately exempt.
    pub(crate) carries_third_party_credentials: bool,
    /// Egress policy is enforced from outside the sandbox by a mechanism the
    /// host knows out-of-band. Consulted only for credential-bearing runs.
    pub(crate) external_egress_enforcement: bool,
}

impl Default for DetachedPreconditions {
    /// The closed position: nothing established, host-authority reach and
    /// credential carriage assumed present. Evaluating the default denies.
    fn default() -> Self {
        Self {
            scoped_model_token_available: false,
            external_lifetime_cap: false,
            image_verified: false,
            host_authority_tool_surface: true,
            carries_third_party_credentials: true,
            external_egress_enforcement: false,
        }
    }
}

/// One unmet detached-admission precondition, named for surfacing (the
/// settings slice renders these per provider) and for the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetachedAdmissionDenial {
    /// No issuer of short-lived, scoped, revocable model tokens.
    NoScopedModelToken,
    /// Nothing outside the sandbox bounds its lifetime.
    NoExternalLifetimeCap,
    /// The agent image is not verified within the topology's trust root.
    ImageNotVerified,
    /// The tool surface reaches a host-authority operation.
    HostAuthorityToolSurface,
    /// The run carries third-party credentials without externally enforced
    /// egress policy.
    CredentialsWithoutExternalEgress,
}

/// The detached-admission decision: admitted only when every precondition
/// holds, otherwise denied with every unmet precondition named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetachedAdmission {
    /// Every precondition held; the run may be recorded as detached-admitted.
    Admitted,
    /// At least one precondition failed; the run falls back to attached-only.
    Denied(Vec<DetachedAdmissionDenial>),
}

impl DetachedAdmission {
    /// The durable admission mode this decision records.
    pub(crate) fn mode(&self) -> SandboxAdmissionMode {
        match self {
            Self::Admitted => SandboxAdmissionMode::Detached,
            Self::Denied(_) => SandboxAdmissionMode::AttachedOnly,
        }
    }
}

/// Evaluate the doc's detached-admission preconditions, failing closed.
///
/// The contract: `Admitted` requires **all** preconditions to hold, and a
/// denial names every unmet one rather than the first, so the settings
/// surface can say exactly what a provider is missing.
pub(crate) fn evaluate_detached_admission(
    preconditions: DetachedPreconditions,
) -> DetachedAdmission {
    let mut denials = Vec::new();
    if !preconditions.scoped_model_token_available {
        denials.push(DetachedAdmissionDenial::NoScopedModelToken);
    }
    if !preconditions.external_lifetime_cap {
        denials.push(DetachedAdmissionDenial::NoExternalLifetimeCap);
    }
    if !preconditions.image_verified {
        denials.push(DetachedAdmissionDenial::ImageNotVerified);
    }
    if preconditions.host_authority_tool_surface {
        denials.push(DetachedAdmissionDenial::HostAuthorityToolSurface);
    }
    if preconditions.carries_third_party_credentials && !preconditions.external_egress_enforcement {
        denials.push(DetachedAdmissionDenial::CredentialsWithoutExternalEgress);
    }
    if denials.is_empty() {
        DetachedAdmission::Admitted
    } else {
        DetachedAdmission::Denied(denials)
    }
}

/// The preconditions the current run shape establishes, given the three facts
/// owned by other components: token issuance (the issuer's honest
/// availability), the backend's external lifetime cap, and the backend's
/// image verification.
///
/// The remaining fields are the run shape every sandbox run has today — the
/// container capability host grants ModelInference only, so no reverse
/// capability reaches a host-authority operation, and no third-party
/// credential is ever delivered into a run. The runner and the settings
/// surface both read this one function, so what settings says a provider is
/// missing is by construction what the gate would deny it for.
pub(crate) fn structural_preconditions(
    scoped_model_token_available: bool,
    external_lifetime_cap: bool,
    image_verified: bool,
) -> DetachedPreconditions {
    DetachedPreconditions {
        scoped_model_token_available,
        external_lifetime_cap,
        image_verified,
        // The container capability host grants ModelInference only; no
        // reverse capability reaches a host-authority operation.
        host_authority_tool_surface: false,
        // No third-party credential is ever delivered into a sandbox run.
        carries_third_party_credentials: false,
        external_egress_enforcement: false,
    }
}

/// Per-provider detached-admission evaluation for the settings surface: what
/// the gate would decide for a run hosted by each execution provider, from
/// declared capabilities only — nothing is provisioned to answer this.
///
/// The token-issuer fact is deployment-wide (the gateway either can mint
/// run-scoped tokens or it cannot); the lifetime-cap fact is per backend. The
/// local provider reads the real Docker backend's declaration. E2B, Daytona,
/// and the container exec backend have no sandbox backend at all today — they
/// are exec adapters, not hosts for background agent runs — so no capability
/// is established for them and the fail-closed evaluation names everything
/// missing. The container exec backend in particular is not the sandbox-agent
/// container tier: the two are deliberately orthogonal, and running commands
/// in a container through `docker exec` establishes none of the preconditions
/// a detached run needs.
pub(crate) fn settings_detached_admissions(
    config: &Config,
) -> Vec<(CodeExecutionProviderKind, DetachedAdmission)> {
    let scoped_token = GatewayScopedTokenIssuer.available();
    let local = DockerSandboxBackend::new(docker_config(config));
    let local_facts = (
        local.enforces_external_lifetime_cap(),
        local.verifies_image_integrity(),
    );
    [
        (CodeExecutionProviderKind::Local, local_facts),
        (CodeExecutionProviderKind::E2b, (false, false)),
        (CodeExecutionProviderKind::Daytona, (false, false)),
        (CodeExecutionProviderKind::Docker, (false, false)),
    ]
    .into_iter()
    .map(|(provider, (lifetime_cap, image_verified))| {
        (
            provider,
            evaluate_detached_admission(structural_preconditions(
                scoped_token,
                lifetime_cap,
                image_verified,
            )),
        )
    })
    .collect()
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

    /// The fail-closed contract of the detached gate: admission requires
    /// every precondition, any single unmet one denies with its name, and the
    /// default (nothing established) denies.
    #[test]
    fn detached_admission_fails_closed_on_any_unmet_precondition() {
        let all_met = DetachedPreconditions {
            scoped_model_token_available: true,
            external_lifetime_cap: true,
            image_verified: true,
            host_authority_tool_surface: false,
            carries_third_party_credentials: false,
            external_egress_enforcement: false,
        };
        assert_eq!(
            evaluate_detached_admission(all_met),
            DetachedAdmission::Admitted
        );
        assert_eq!(
            evaluate_detached_admission(all_met).mode(),
            SandboxAdmissionMode::Detached
        );

        let single_failures = [
            (
                DetachedPreconditions {
                    scoped_model_token_available: false,
                    ..all_met
                },
                DetachedAdmissionDenial::NoScopedModelToken,
            ),
            (
                DetachedPreconditions {
                    external_lifetime_cap: false,
                    ..all_met
                },
                DetachedAdmissionDenial::NoExternalLifetimeCap,
            ),
            (
                DetachedPreconditions {
                    image_verified: false,
                    ..all_met
                },
                DetachedAdmissionDenial::ImageNotVerified,
            ),
            (
                DetachedPreconditions {
                    host_authority_tool_surface: true,
                    ..all_met
                },
                DetachedAdmissionDenial::HostAuthorityToolSurface,
            ),
            (
                DetachedPreconditions {
                    carries_third_party_credentials: true,
                    ..all_met
                },
                DetachedAdmissionDenial::CredentialsWithoutExternalEgress,
            ),
        ];
        for (preconditions, expected) in single_failures {
            let decision = evaluate_detached_admission(preconditions);
            assert_eq!(decision, DetachedAdmission::Denied(vec![expected]));
            assert_eq!(decision.mode(), SandboxAdmissionMode::AttachedOnly);
        }

        // Credential-bearing work with externally enforced egress is not a
        // denial on its own.
        assert_eq!(
            evaluate_detached_admission(DetachedPreconditions {
                carries_third_party_credentials: true,
                external_egress_enforcement: true,
                ..all_met
            }),
            DetachedAdmission::Admitted
        );

        // The default is the closed position, and the denial names every
        // unmet precondition, not the first.
        let DetachedAdmission::Denied(denials) =
            evaluate_detached_admission(DetachedPreconditions::default())
        else {
            panic!("the default preconditions must deny");
        };
        assert_eq!(denials.len(), 5);
    }

    #[test]
    fn sandbox_container_image_only_overrides_the_docker_default_when_set() {
        let mut config = Config::desktop("/data");
        assert_eq!(docker_config(&config).image, DockerConfig::default().image);

        config.container_image = Some("openwave-sandbox-agent:dev".into());
        assert_eq!(docker_config(&config).image, "openwave-sandbox-agent:dev");
    }
}
