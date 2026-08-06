//! Host-owned code-execution configuration, credentials, and egress policy.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openwave_code_execution::{
    CodeExecutionError, CodeExecutionProviderKind, CodeExecutionUnavailableReason,
    DaytonaCredential, DaytonaExecutionProvider, DockerExecutionProvider, E2BCredential,
    E2BExecutionProvider, LocalExecutionProvider, RemoteSessionPool, DAYTONA_CREDENTIAL_KEY,
    E2B_CREDENTIAL_KEY, PACKAGE_MANAGER_DOMAINS,
};
use openwave_core::{ChatId, HostRootId, NetworkPolicy, ProjectId, Result, SecretProvider, Store};
use openwave_egress::{
    CidrBlock, DomainPattern, EgressAllowlist, EgressEnforcement, EgressError, EgressPolicy,
};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;

pub(super) const CODE_EXECUTION_SETTING: &str = "code_execution";
/// Generous enough for a cold `pip install` that pulls compiled wheels
/// (lxml, Pillow); 20s proved too tight and cut installs off mid-retry with
/// empty stderr. Still host-owned: the model cannot request a longer limit.
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
pub(super) const MAX_NETWORK_ALLOWED_HOSTS: usize = 64;

/// The interpreter the local sandbox resolves from its fixed PATH; host-side
/// cache acquisition runs the same one, so cached wheels are compatible with
/// the sandbox runtime by construction.
pub(super) const SANDBOX_PYTHON: &str = "/usr/bin/python3";

/// Whether a per-chat policy admits package-registry downloads, mirroring the
/// operating prompt's truth table.
pub(super) fn permits_package_installs(policy: &NetworkPolicy) -> bool {
    match policy {
        NetworkPolicy::Off => false,
        NetworkPolicy::PackageManagers | NetworkPolicy::Open => true,
        NetworkPolicy::AllowedHosts {
            package_managers, ..
        } => *package_managers,
    }
}

/// Validate and canonicalize one user-authored per-chat network policy.
///
/// Custom entries are exact DNS hosts. Wildcards, address literals, and
/// duplicate spellings are refused or collapsed before persistence so every
/// renderer and provider compiles the same authority.
pub(crate) fn normalize_network_policy(
    policy: &mut NetworkPolicy,
) -> std::result::Result<(), ServerError> {
    let NetworkPolicy::AllowedHosts { allowed_hosts, .. } = policy else {
        return Ok(());
    };
    if allowed_hosts.len() > MAX_NETWORK_ALLOWED_HOSTS {
        return Err(ServerError::bad_request(format!(
            "network policy accepts at most {MAX_NETWORK_ALLOWED_HOSTS} allowed hosts"
        )));
    }
    let mut normalized = Vec::with_capacity(allowed_hosts.len());
    let mut seen = HashSet::new();
    for host in allowed_hosts.iter() {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.starts_with("*.") {
            return Err(ServerError::bad_request(
                "network policy allowed hosts must be exact DNS names",
            ));
        }
        let pattern = DomainPattern::parse(&host)
            .map_err(|error| ServerError::bad_request(error.to_string()))?;
        let host = pattern.to_string();
        if seen.insert(host.clone()) {
            normalized.push(host);
        }
    }
    *allowed_hosts = normalized;
    Ok(())
}

pub(super) fn network_egress_config(policy: &NetworkPolicy) -> EgressConfig {
    match policy {
        NetworkPolicy::Off => EgressConfig::Allowlist {
            domains: Vec::new(),
            cidrs: Vec::new(),
        },
        NetworkPolicy::PackageManagers => EgressConfig::Allowlist {
            domains: PACKAGE_MANAGER_DOMAINS
                .iter()
                .map(|domain| (*domain).to_owned())
                .collect(),
            cidrs: Vec::new(),
        },
        NetworkPolicy::AllowedHosts {
            allowed_hosts,
            package_managers,
        } => {
            let mut domains = allowed_hosts.clone();
            if *package_managers {
                domains.extend(
                    PACKAGE_MANAGER_DOMAINS
                        .iter()
                        .map(|domain| (*domain).to_owned()),
                );
            }
            domains.sort();
            domains.dedup();
            EgressConfig::Allowlist {
                domains,
                cidrs: Vec::new(),
            }
        }
        NetworkPolicy::Open => EgressConfig::Open,
    }
}

/// Exact product attachment projection sent to the trusted desktop host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecFolderGrantQuery {
    pub chat_id: ChatId,
    pub project_id: Option<ProjectId>,
    pub root_ids: Vec<HostRootId>,
}

/// One live broker-authorized folder returned to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExecFolderGrant {
    pub root_id: HostRootId,
    pub path: PathBuf,
    pub writable: bool,
    /// Where this turn stages writes for the folder, when it stages them.
    ///
    /// Set by the server after the broker answers, never by the broker: it is
    /// a property of the turn, not of the attachment.
    pub overlay: Option<PathBuf>,
    /// The broker granted writes, but this turn could not stage them safely.
    ///
    /// Such a folder is deliberately downgraded to read-only. Keeping the
    /// reason distinct lets the operating prompt explain the restriction
    /// instead of presenting it as ordinary read-only authority.
    pub staging_unavailable: bool,
}

/// Native-only bridge from product root attachments to live broker authority.
///
/// Implementations must intersect the supplied product IDs with current
/// broker attachment and capability state. The model never calls this surface.
#[async_trait]
pub trait ExecFolderGrantResolver: Send + Sync {
    async fn resolve(
        &self,
        query: ExecFolderGrantQuery,
    ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, String>;
}

/// The fixed managed providers this host can hold a credential for. Local needs
/// none. Keeping the allow-list here means a local API route can never turn an
/// arbitrary path segment into a keychain key.
pub(super) const CREDENTIAL_PROVIDERS: [CodeExecutionProviderKind; 2] = [
    CodeExecutionProviderKind::E2b,
    CodeExecutionProviderKind::Daytona,
];

/// Every execution provider this build ships, in the order the settings
/// surface reports them. Availability is computed per row, so a host with no
/// usable provider says so with a reason for each rather than presenting a
/// selection that cannot run.
pub(super) const EXECUTION_PROVIDERS: [CodeExecutionProviderKind; 4] = [
    CodeExecutionProviderKind::Local,
    CodeExecutionProviderKind::E2b,
    CodeExecutionProviderKind::Daytona,
    CodeExecutionProviderKind::Docker,
];

/// Host-owned, non-secret egress policy for the managed exec sandboxes.
///
/// The model never sets this (invariant 1): it is host configuration, carries
/// no secret, and accepts no endpoint. `Open` is the default and preserves
/// exec's out-of-the-box behavior — E2B and Daytona are created with open
/// internet access, as they always have been. Egress restriction is opt-in:
/// an `Allowlist` switches every managed sandbox created afterwards to
/// deny-by-default and compiles the listed domain patterns and CIDR blocks
/// into the vendor's per-sandbox network controls. An empty allowlist denies
/// everything on both axes.
///
/// The strings are validated to the same [`DomainPattern`] and [`CidrBlock`]
/// grammar the decision layer uses, so a malformed grant is rejected at
/// `PUT` time rather than silently widening egress at sandbox creation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EgressConfig {
    /// Unrestricted egress — today's default. No policy is applied and the
    /// managed adapters create open-internet sandboxes.
    #[default]
    Open,
    /// Deny-by-default egress restricted to these domain patterns and CIDR
    /// blocks. An empty allowlist blocks all egress.
    Allowlist {
        domains: Vec<String>,
        cidrs: Vec<String>,
    },
}

impl EgressConfig {
    /// The egress policy to compile into a managed sandbox at creation, or
    /// `None` to keep today's open-internet creation.
    ///
    /// Returns an error rather than silently opening egress when a stored
    /// pattern does not parse, so an invalid grant fails closed at the network
    /// boundary instead of degrading to unrestricted.
    pub(super) fn to_policy(&self) -> std::result::Result<Option<EgressPolicy>, EgressError> {
        match self {
            Self::Open => Ok(None),
            Self::Allowlist { domains, cidrs } => {
                let domains = domains
                    .iter()
                    .map(DomainPattern::parse)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let cidrs = cidrs
                    .iter()
                    .map(CidrBlock::parse)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(Some(EgressPolicy::Allowlist(EgressAllowlist::new(
                    domains, cidrs,
                ))))
            }
        }
    }
}

/// Non-secret host selection. Local is the default only where its mandatory
/// sandbox actually exists; that sandbox confines writes and enforces each
/// chat's network policy outside the workload. `None` explicitly removes
/// execution from service without changing the stable tool contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeExecutionConfig {
    #[serde(default)]
    pub provider: Option<CodeExecutionProviderKind>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Egress policy for the managed adapters. Absent in configs written
    /// before this field existed; those default to `Open`, preserving the
    /// open-internet creation they already had.
    #[serde(default)]
    pub egress: EgressConfig,
    /// E2B template id to create sandboxes from. `None` — the normal case —
    /// uses the OpenWave documents template E2B publishes publicly, so an
    /// account that has only pasted an API key still gets the official image
    /// with no template setup of its own. Set this only to override that with
    /// an account's own template; the override is used verbatim and, unlike the
    /// default, never falls back to E2B's public code-interpreter template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2b_template: Option<String>,
    /// Daytona snapshot name to create sandboxes from. An escape hatch, not
    /// the normal path: `None` lets the provider register and use the official
    /// OpenWave documents snapshot in the caller's own Daytona organization on
    /// first use, so a pasted API key needs no further setup. Setting a name
    /// disables that entirely — the named snapshot is used verbatim, with no
    /// auto-registration and no fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daytona_snapshot: Option<String>,
}

impl Default for CodeExecutionConfig {
    fn default() -> Self {
        Self {
            // Local is the default only on hosts where it can actually run.
            // Selecting it elsewhere would be a dead selection: every exec
            // would fail and the surface would report a configured provider
            // that never works. No provider at all is the truthful state, and
            // the settings surface says which providers are unavailable and
            // why.
            provider: LocalExecutionProvider::availability()
                .is_ok()
                .then_some(CodeExecutionProviderKind::Local),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            egress: EgressConfig::Open,
            e2b_template: None,
            daytona_snapshot: None,
        }
    }
}

impl CodeExecutionConfig {
    pub(super) fn disabled() -> Self {
        Self {
            provider: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            egress: EgressConfig::Open,
            e2b_template: None,
            daytona_snapshot: None,
        }
    }

    pub(super) fn validate(&self) -> std::result::Result<(), ServerError> {
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(ServerError::bad_request(format!(
                "code execution timeout_ms must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}"
            )));
        }
        // A malformed allowlist is a bad request, not a silent open egress.
        self.egress
            .to_policy()
            .map_err(|error| ServerError::bad_request(error.to_string()))?;
        Ok(())
    }
}

pub(super) const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// Compile the stored egress config into a policy at the last boundary before
/// a managed sandbox is created. A malformed stored allowlist fails closed by
/// refusing execution rather than degrading to open egress.
pub(super) fn resolve_egress_policy(
    egress: &EgressConfig,
) -> std::result::Result<Option<EgressPolicy>, CodeExecutionError> {
    egress.to_policy().map_err(|error| {
        CodeExecutionError::InvalidRequest(format!("invalid egress policy: {error}"))
    })
}

/// Build the E2B adapter with the configured egress policy applied.
///
/// This is the wiring a dropped-policy regression would silently break —
/// reverting a configured allowlist to open egress — so it is a named function
/// the resolve path and its test share, rather than an inline arm nothing
/// exercises. `Open` leaves today's open-internet creation intact.
pub(super) fn configured_e2b(
    credential: E2BCredential,
    timeout: Duration,
    pool: RemoteSessionPool,
    egress: &EgressConfig,
    template: Option<&str>,
) -> std::result::Result<E2BExecutionProvider, CodeExecutionError> {
    let mut provider = E2BExecutionProvider::with_session_pool(credential, timeout, pool)?;
    if let Some(template) = template {
        provider = provider.with_template(template);
    }
    Ok(match resolve_egress_policy(egress)? {
        Some(policy) => provider.with_egress_policy(policy),
        None => provider,
    })
}

/// Build the Daytona adapter with the configured egress policy applied. The
/// same policy compiles into Daytona's block-all switch and allowlists; an
/// over-limit allowlist is rejected here before any sandbox is created.
pub(super) fn configured_daytona(
    credential: DaytonaCredential,
    timeout: Duration,
    pool: RemoteSessionPool,
    egress: &EgressConfig,
    snapshot: Option<&str>,
    preparation: Option<Arc<dyn openwave_code_execution::SandboxPreparationSink>>,
) -> std::result::Result<DaytonaExecutionProvider, CodeExecutionError> {
    let mut provider = DaytonaExecutionProvider::with_session_pool(credential, timeout, pool)?;
    if let Some(snapshot) = snapshot {
        provider = provider.with_snapshot(snapshot);
    }
    if let Some(sink) = preparation {
        provider = provider.with_preparation_sink(sink);
    }
    match resolve_egress_policy(egress)? {
        Some(policy) => provider.with_egress_policy(policy),
        None => Ok(provider),
    }
}

/// Build the container adapter. It holds no credential and compiles no
/// egress policy — the configured policy is deliberately not passed here,
/// because nothing in this backend would enforce it and accepting it would
/// imply otherwise. See [`egress_enforcement_status`].
pub(super) fn configured_docker(
    timeout: Duration,
    pool: RemoteSessionPool,
) -> std::result::Result<DockerExecutionProvider, CodeExecutionError> {
    DockerExecutionProvider::with_session_pool(timeout, pool)
}

/// Renderer-safe configuration and readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct CodeExecutionConfigInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<CodeExecutionProviderKind>,
    pub timeout_ms: u64,
    pub available: bool,
    /// Why the *selected* provider cannot run, when it cannot. Absent while
    /// execution is available or no provider is selected at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unavailable_reason: Option<CodeExecutionUnavailableReason>,
    pub has_credential: bool,
    /// One row per shipped provider: whether it could run here at all, and the
    /// reason it could not. This is what makes an unusable host legible —
    /// "paste an E2B key" is visible instead of being inferred from a generic
    /// execution failure.
    pub providers: Vec<CodeExecutionProviderAvailability>,
    /// The configured egress policy and each managed provider's enforcement
    /// status, so the renderer can present the policy and disclose which
    /// providers actually restrict egress today.
    pub egress: CodeExecutionEgressInfo,
    /// Per-provider detached-admission evaluation: for each execution
    /// provider, whether the fail-closed gate (issue #824) would admit a
    /// detached run it hosted, and every named precondition it fails. Derived
    /// by running the real admission evaluator over each provider's declared
    /// capabilities — the settings surface and the gate cannot disagree.
    pub detached_admission: Vec<DetachedAdmissionProviderInfo>,
}

/// Renderer-safe egress policy plus per-provider enforcement disclosure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct CodeExecutionEgressInfo {
    /// The configured host policy. `Open` is the default: managed sandboxes are
    /// created with open internet access. An allowlist restricts every managed
    /// sandbox created afterwards.
    pub policy: EgressConfig,
    /// One row per managed provider, stating whether its egress restriction is
    /// confirmed against the live vendor API or still pending confirmation.
    pub enforcement: Vec<CodeExecutionEgressEnforcement>,
}

/// The honest state of a managed provider's egress enforcement.
///
/// Derived from the shipped enforcement model, never asserted per provider, so
/// the settings surface and the decision layer cannot disagree: if the model
/// says a vendor's mechanism leaves a general-purpose destination reachable,
/// the surface must not present it as a full boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum EgressEnforcementStatus {
    /// External enforcement with no general-purpose holes and no unmet
    /// precondition: a full network boundary the host can rely on
    /// unconditionally. No managed provider reaches this today — the only
    /// unconditional boundary is the local sandbox, outside this list.
    Boundary,
    /// A full boundary *when enforced*, gated on a precondition the host cannot
    /// verify statically. Daytona's per-sandbox egress is a strict, externally
    /// enforced allowlist, but the per-sandbox override requires Daytona org
    /// tier 3+; on tier 1–2 the org default applies and the boundary is not
    /// guaranteed. Disclosed with the requirement inline so it never reads as an
    /// unconditional green boundary.
    ConditionalBoundary,
    /// External enforcement is applied, but the vendor's mechanism leaves
    /// general-purpose destinations reachable, so a configured allowlist is
    /// not a full boundary and must not be presented as one.
    AppliedWithGaps,
    /// A policy is sent at creation, but the vendor's enforcement is not yet
    /// confirmed against the live API.
    Unconfirmed,
    /// No egress restriction is applied at all: the backend creates its
    /// sandbox with ordinary outbound access and the conversation's network
    /// policy reaches nothing. Distinct from [`Self::Unconfirmed`], which is
    /// a policy whose effect is unproven — here there is no policy to prove.
    NotEnforced,
}

/// A managed provider's egress-enforcement status, as host knowledge rather
/// than a claim the backend makes about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct CodeExecutionEgressEnforcement {
    pub provider: CodeExecutionProviderKind,
    pub status: EgressEnforcementStatus,
    /// Destinations the vendor's mechanism keeps reachable regardless of the
    /// configured policy — each a short purpose string straight from the
    /// enforcement model, so the settings surface can show the caveat inline
    /// instead of burying it in prose the user skims past.
    pub gaps: Vec<String>,
    /// A precondition the boundary is gated on that the host cannot verify
    /// statically ("Daytona org tier 3+"). Present only for a
    /// [`EgressEnforcementStatus::ConditionalBoundary`], so the surface can
    /// state the condition inline rather than implying an unconditional
    /// boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub requirement: Option<String>,
}

/// One provider's detached-admission verdict, renderer-safe.
///
/// `denials` is what the real evaluator returned for this provider's declared
/// capabilities: empty exactly when `admitted`. The rows exist even for
/// providers that cannot host background runs at all — every precondition is
/// simply unestablished for them, and the fail-closed evaluation names each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct DetachedAdmissionProviderInfo {
    pub provider: CodeExecutionProviderKind,
    /// Whether the gate would admit a detached run hosted by this provider.
    pub admitted: bool,
    /// Every unmet precondition, named — not just the first.
    pub denials: Vec<DetachedAdmissionDenialReason>,
}

/// Wire mirror of the admission gate's typed denial reasons
/// ([`crate::sandbox_admission::DetachedAdmissionDenial`]), so the renderer
/// maps each to user-facing language instead of receiving prose the server
/// composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum DetachedAdmissionDenialReason {
    /// No issuer of short-lived, scoped, revocable model tokens.
    NoScopedModelToken,
    /// Nothing outside the sandbox bounds its lifetime.
    NoExternalLifetimeCap,
    /// The agent image is not verified within the topology's trust root.
    ImageNotVerified,
    /// The tool surface reaches a host-authority operation.
    HostAuthorityToolSurface,
    /// Third-party credentials without externally enforced egress policy.
    CredentialsWithoutExternalEgress,
}

impl From<crate::sandbox_admission::DetachedAdmissionDenial> for DetachedAdmissionDenialReason {
    fn from(denial: crate::sandbox_admission::DetachedAdmissionDenial) -> Self {
        use crate::sandbox_admission::DetachedAdmissionDenial as Denial;
        match denial {
            Denial::NoScopedModelToken => Self::NoScopedModelToken,
            Denial::NoExternalLifetimeCap => Self::NoExternalLifetimeCap,
            Denial::ImageNotVerified => Self::ImageNotVerified,
            Denial::HostAuthorityToolSurface => Self::HostAuthorityToolSurface,
            Denial::CredentialsWithoutExternalEgress => Self::CredentialsWithoutExternalEgress,
        }
    }
}

/// Project the per-provider admission evaluation into the settings payload.
pub(super) fn detached_admission_info(
    host_config: &openwave_core::Config,
) -> Vec<DetachedAdmissionProviderInfo> {
    use crate::sandbox_admission::DetachedAdmission;
    crate::sandbox_admission::settings_detached_admissions(host_config)
        .into_iter()
        .map(|(provider, decision)| match decision {
            DetachedAdmission::Admitted => DetachedAdmissionProviderInfo {
                provider,
                admitted: true,
                denials: Vec::new(),
            },
            DetachedAdmission::Denied(denials) => DetachedAdmissionProviderInfo {
                provider,
                admitted: false,
                denials: denials.into_iter().map(Into::into).collect(),
            },
        })
        .collect()
}

/// The precondition Daytona's per-sandbox egress boundary is gated on. The
/// per-sandbox network override requires Daytona org tier 3+; on tier 1–2 the
/// override is refused and the org default applies, so the boundary is not
/// guaranteed. The host cannot read the account's tier statically, so the
/// requirement is surfaced inline rather than assumed met.
pub(super) const DAYTONA_TIER_REQUIREMENT: &str = "Daytona org tier 3+";

/// The managed providers' egress-enforcement disclosure, derived from the
/// shipped [`EgressEnforcement`] declarations.
///
/// E2B's enforcement is confirmed against the live API, so its status is
/// whatever the model reports — and the model reports *not a boundary*, because
/// its domain rules cover only HTTP/HTTPS ports and DNS stays open. Daytona's
/// per-sandbox enforcement is now confirmed live (issue #888): it is a strict,
/// externally enforced allowlist with no general-purpose carve-out, so the
/// model reports it as a credential boundary. The one thing the host cannot
/// establish statically is the account tier the per-sandbox override needs, so
/// Daytona is disclosed as a *conditional* boundary with that requirement
/// inline, never an unconditional green one.
pub(super) fn egress_enforcement_status() -> Vec<CodeExecutionEgressEnforcement> {
    vec![
        enforcement_row(
            CodeExecutionProviderKind::E2b,
            &E2BExecutionProvider::egress_enforcement(),
            true,
            None,
        ),
        enforcement_row(
            CodeExecutionProviderKind::Daytona,
            &DaytonaExecutionProvider::egress_enforcement(),
            true,
            Some(DAYTONA_TIER_REQUIREMENT),
        ),
        // The container backend applies no network policy at all. It is not
        // derived from an enforcement model because there is no enforcement
        // to model: saying so plainly is the only honest row, and it is what
        // stops a conversation set to "no network" from reading as enforced
        // when it is running on this backend.
        CodeExecutionEgressEnforcement {
            provider: CodeExecutionProviderKind::Docker,
            status: EgressEnforcementStatus::NotEnforced,
            gaps: vec![DOCKER_OPEN_EGRESS_GAP.to_owned()],
            requirement: None,
        },
    ]
}

/// What the container backend leaves reachable: everything. Stated as a gap
/// so the settings surface lists it inline beside the vendors' caveats.
pub(super) const DOCKER_OPEN_EGRESS_GAP: &str =
    "every destination — the container runs with ordinary internet access";

/// Project one provider's enforcement declaration into the renderer-safe row.
///
/// The status reads straight from the model: an unconfirmed vendor is
/// `Unconfirmed`; otherwise `is_credential_boundary()` — external tier with no
/// general-purpose holes — decides boundary versus `AppliedWithGaps`. A
/// `requirement` is a precondition the host cannot verify statically: when the
/// model backs a boundary but such a precondition exists, the row is a
/// `ConditionalBoundary` carrying the requirement, so the surface can never
/// present it as an unconditional boundary. Every declared exception is
/// surfaced as a gap so nothing the vendor leaves open is hidden from the user.
pub(super) fn enforcement_row(
    provider: CodeExecutionProviderKind,
    enforcement: &EgressEnforcement,
    confirmed: bool,
    requirement: Option<&'static str>,
) -> CodeExecutionEgressEnforcement {
    let status = if !confirmed {
        EgressEnforcementStatus::Unconfirmed
    } else if enforcement.is_credential_boundary() {
        if requirement.is_some() {
            EgressEnforcementStatus::ConditionalBoundary
        } else {
            EgressEnforcementStatus::Boundary
        }
    } else {
        EgressEnforcementStatus::AppliedWithGaps
    };
    let gaps = enforcement
        .exceptions()
        .iter()
        .map(|exception| exception.purpose.to_owned())
        .collect();
    CodeExecutionEgressEnforcement {
        provider,
        status,
        gaps,
        requirement: requirement.map(str::to_owned),
    }
}

impl CodeExecutionEgressInfo {
    fn from_config(policy: EgressConfig) -> Self {
        Self {
            policy,
            enforcement: egress_enforcement_status(),
        }
    }
}

/// Renderer-safe readiness for one managed provider's fixed credential slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct CodeExecutionCredentialReadiness {
    pub provider: CodeExecutionProviderKind,
    pub has_credential: bool,
}

/// Structured capability report for one execution provider on this host.
///
/// `available` and `unavailable_reason` are two views of one decision, made in
/// [`provider_availability`], so no surface has to re-derive whether a platform
/// supports a provider or whether a key is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct CodeExecutionProviderAvailability {
    pub provider: CodeExecutionProviderKind,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unavailable_reason: Option<CodeExecutionUnavailableReason>,
}

impl CodeExecutionProviderAvailability {
    fn new(
        provider: CodeExecutionProviderKind,
        unavailable_reason: Option<CodeExecutionUnavailableReason>,
    ) -> Self {
        Self {
            provider,
            available: unavailable_reason.is_none(),
            unavailable_reason,
        }
    }
}

/// The single place that decides whether a provider can execute here.
///
/// Local asks the adapter's own platform probe; the managed providers are
/// available exactly when their credential slot is filled. Everything else —
/// the default selection, the settings rows, the selected provider's status —
/// reads this, so they cannot disagree.
pub(super) async fn provider_availability(
    secrets: &dyn SecretProvider,
    provider: CodeExecutionProviderKind,
) -> CodeExecutionProviderAvailability {
    let reason = match provider {
        CodeExecutionProviderKind::Local => LocalExecutionProvider::availability().err(),
        // The container backend needs no credential; what it needs is a
        // runtime that answers. The probe distinguishes an absent runtime
        // from an installed one whose daemon is down, and caches its answer,
        // so rendering this surface costs at most one probe.
        CodeExecutionProviderKind::Docker => DockerExecutionProvider::availability().await.err(),
        _ => (!has_credential(secrets, provider).await)
            .then_some(CodeExecutionUnavailableReason::MissingCredential),
    };
    CodeExecutionProviderAvailability::new(provider, reason)
}

/// Credential readiness for every managed provider this host supports, so the
/// renderer can offer a key field per provider without selecting one first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeExecutionCredentialsInfo {
    pub credentials: Vec<CodeExecutionCredentialReadiness>,
}

/// Partial update accepted by `PUT /code-execution`. An explicit null disables
/// all providers; an absent field leaves the current value unchanged.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeExecutionConfigUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub provider: Option<Option<CodeExecutionProviderKind>>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Replace the egress policy. Absent leaves the current policy unchanged;
    /// no secret or endpoint is accepted here — only domain patterns and CIDRs.
    #[serde(default)]
    pub egress: Option<EgressConfig>,
    /// Replace the E2B template id. An explicit null (or empty string) returns
    /// to E2B's default template; absent leaves the current value unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub e2b_template: Option<Option<String>>,
    /// Replace the Daytona snapshot name. An explicit null (or empty string)
    /// returns to Daytona's default snapshot; absent leaves the current value
    /// unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub daytona_snapshot: Option<Option<String>>,
}

pub(super) fn double_option<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// Read configured host policy. Invalid hand-edited state fails closed.
pub async fn read_config(store: &dyn Store) -> Result<CodeExecutionConfig> {
    let Some(value) = store.get_setting(CODE_EXECUTION_SETTING).await? else {
        return Ok(CodeExecutionConfig::default());
    };
    let Ok(config) = serde_json::from_value::<CodeExecutionConfig>(value) else {
        return Ok(CodeExecutionConfig::disabled());
    };
    if config.validate().is_err() {
        return Ok(CodeExecutionConfig::disabled());
    }
    Ok(config)
}

pub(super) async fn write_config(store: &dyn Store, config: &CodeExecutionConfig) -> Result<()> {
    store
        .set_setting(CODE_EXECUTION_SETTING, &serde_json::to_value(config)?)
        .await
}

pub async fn config_info(
    host_config: &openwave_core::Config,
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<CodeExecutionConfigInfo> {
    let config = read_config(store).await?;
    let has_credential = match config.provider {
        Some(provider) => has_credential(secrets, provider).await,
        None => false,
    };
    let mut providers = Vec::with_capacity(EXECUTION_PROVIDERS.len());
    for provider in EXECUTION_PROVIDERS {
        providers.push(provider_availability(secrets, provider).await);
    }
    // The selected provider's status is read out of the same rows, so the
    // headline and the per-provider list can never contradict each other.
    let selected = config.provider.and_then(|provider| {
        providers
            .iter()
            .find(|candidate| candidate.provider == provider)
            .copied()
    });
    Ok(CodeExecutionConfigInfo {
        provider: config.provider,
        timeout_ms: config.timeout_ms,
        available: selected.is_some_and(|row| row.available),
        unavailable_reason: selected.and_then(|row| row.unavailable_reason),
        has_credential,
        providers,
        egress: CodeExecutionEgressInfo::from_config(config.egress),
        detached_admission: detached_admission_info(host_config),
    })
}

/// Report readiness for every managed provider without reading or returning any
/// key material.
pub async fn credentials_info(secrets: &dyn SecretProvider) -> CodeExecutionCredentialsInfo {
    let mut credentials = Vec::with_capacity(CREDENTIAL_PROVIDERS.len());
    for provider in CREDENTIAL_PROVIDERS {
        credentials.push(CodeExecutionCredentialReadiness {
            provider,
            has_credential: has_credential(secrets, provider).await,
        });
    }
    CodeExecutionCredentialsInfo { credentials }
}

pub async fn update_config(
    host_config: &openwave_core::Config,
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    update: CodeExecutionConfigUpdate,
) -> std::result::Result<CodeExecutionConfigInfo, ServerError> {
    let mut config = read_config(store).await?;
    if let Some(provider) = update.provider {
        config.provider = provider;
    }
    if let Some(timeout_ms) = update.timeout_ms {
        config.timeout_ms = timeout_ms;
    }
    if let Some(egress) = update.egress {
        config.egress = egress;
    }
    if let Some(template) = update.e2b_template {
        config.e2b_template = template.filter(|value| !value.is_empty());
    }
    if let Some(snapshot) = update.daytona_snapshot {
        config.daytona_snapshot = snapshot.filter(|value| !value.is_empty());
    }
    config.validate()?;
    write_config(store, &config).await?;
    config_info(host_config, store, secrets)
        .await
        .map_err(Into::into)
}

pub async fn write_credential(
    secrets: &dyn SecretProvider,
    provider: CodeExecutionProviderKind,
    api_key: &str,
) -> std::result::Result<CodeExecutionCredentialReadiness, ServerError> {
    let (key, label) = credential_spec(provider)?;
    secrets
        .set_secret(key, api_key)
        .await
        .map_err(|_| ServerError::internal(format!("{label} credential storage is unavailable")))?;
    Ok(CodeExecutionCredentialReadiness {
        provider,
        has_credential: true,
    })
}

pub async fn delete_credential(
    secrets: &dyn SecretProvider,
    provider: CodeExecutionProviderKind,
) -> std::result::Result<CodeExecutionCredentialReadiness, ServerError> {
    let (key, label) = credential_spec(provider)?;
    secrets
        .delete_secret(key)
        .await
        .map_err(|_| ServerError::internal(format!("{label} credential storage is unavailable")))?;
    Ok(CodeExecutionCredentialReadiness {
        provider,
        has_credential: false,
    })
}

pub fn credential_provider(
    value: &str,
) -> std::result::Result<CodeExecutionProviderKind, ServerError> {
    CREDENTIAL_PROVIDERS
        .into_iter()
        .find(|provider| provider.as_str() == value)
        .ok_or_else(|| {
            ServerError::not_found(format!(
                "unknown credentialed code execution provider kind: {value}"
            ))
        })
}

pub(super) async fn has_credential(
    secrets: &dyn SecretProvider,
    provider: CodeExecutionProviderKind,
) -> bool {
    match provider {
        CodeExecutionProviderKind::E2b => {
            E2BCredential::load(secrets).await.ok().flatten().is_some()
        }
        CodeExecutionProviderKind::Daytona => DaytonaCredential::load(secrets)
            .await
            .ok()
            .flatten()
            .is_some(),
        CodeExecutionProviderKind::Local | CodeExecutionProviderKind::Docker => false,
        _ => false,
    }
}

pub(super) fn credential_spec(
    provider: CodeExecutionProviderKind,
) -> std::result::Result<(&'static str, &'static str), ServerError> {
    match provider {
        CodeExecutionProviderKind::E2b => Ok((E2B_CREDENTIAL_KEY, "E2B")),
        CodeExecutionProviderKind::Daytona => Ok((DAYTONA_CREDENTIAL_KEY, "Daytona")),
        _ => Err(ServerError::not_found(format!(
            "unknown credentialed code execution provider kind: {provider}"
        ))),
    }
}
