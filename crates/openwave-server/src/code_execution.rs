//! Host-owned code-execution provider selection and policy.
//!
//! The model cannot select a provider or timeout. The foreground `exec` tool
//! calls [`ConfiguredCodeExecutionProvider`], which reads the current host
//! setting at the last possible boundary and delegates to the selected adapter.
//! Local and managed adapters implement the same provider contract without
//! changing the tool schema or persisted tool-call arguments.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use openwave_code_execution::{
    resolve_scratch_directory, sync, CodeExecutionError, CodeExecutionProvider,
    CodeExecutionProviderKind, CodeExecutionRequest, CodeExecutionResponse, DaytonaCredential,
    DaytonaExecutionProvider, E2BCredential, E2BExecutionProvider, ExecFolderAccess,
    ExecFolderGrant, ExecutionId, ExecutionWorkspaceId, LocalExecutionProvider,
    MaterializationPrecondition, MaterializedChangeKind, OutputArtifactEntry, OutputArtifactScan,
    OutputArtifactStatus, PreviewScan, RejectedChangeReason, RemoteSessionPool, SharedPackageCache,
    StagedUpload, WorkspaceFilePath, WorkspaceLifecycle, WorkspaceListing, WriteOverlay,
    WriteSnapshotSink, DAYTONA_CREDENTIAL_KEY, DOCUMENT_SCRIPTS_DIR, DOCUMENT_SCRIPT_FILES,
    E2B_CREDENTIAL_KEY, PACKAGE_CACHE_DIR, PACKAGE_MANAGER_DOMAINS,
};
use openwave_core::{
    exec_attachment_file_name, BlobStore, CallId, Chat, ChatId, ExecFileRejectionReason,
    ExecFileRejectionRecord, HostRootId, MessageDocumentAttachment, NetworkPolicy, ProjectId,
    Result, RevisionProducer, SecretProvider, Store, TurnId, MAX_EXEC_WORKSPACE_FILE_BYTES,
};
use openwave_egress::{
    CidrBlock, DomainPattern, EgressAllowlist, EgressEnforcement, EgressError, EgressPolicy,
};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;
use crate::exec_write_snapshot::TurnSnapshotSink;
use crate::state::BlobWriteGuard;

const CODE_EXECUTION_SETTING: &str = "code_execution";
/// Generous enough for a cold `pip install` that pulls compiled wheels
/// (lxml, Pillow); 20s proved too tight and cut installs off mid-retry with
/// empty stderr. Still host-owned: the model cannot request a longer limit.
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_NETWORK_ALLOWED_HOSTS: usize = 64;

/// The interpreter the local sandbox resolves from its fixed PATH; host-side
/// cache acquisition runs the same one, so cached wheels are compatible with
/// the sandbox runtime by construction.
const SANDBOX_PYTHON: &str = "/usr/bin/python3";

/// Whether a per-chat policy admits package-registry downloads, mirroring the
/// operating prompt's truth table.
fn permits_package_installs(policy: &NetworkPolicy) -> bool {
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

fn network_egress_config(policy: &NetworkPolicy) -> EgressConfig {
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
const CREDENTIAL_PROVIDERS: [CodeExecutionProviderKind; 2] = [
    CodeExecutionProviderKind::E2b,
    CodeExecutionProviderKind::Daytona,
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
    fn to_policy(&self) -> std::result::Result<Option<EgressPolicy>, EgressError> {
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

/// Non-secret host selection. Local is usable by default because its mandatory
/// sandbox confines writes and enforces each chat's network policy outside the
/// workload. `None` explicitly removes execution from service without changing
/// the stable tool contract.
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
}

impl Default for CodeExecutionConfig {
    fn default() -> Self {
        Self {
            provider: Some(CodeExecutionProviderKind::Local),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            egress: EgressConfig::Open,
        }
    }
}

impl CodeExecutionConfig {
    fn disabled() -> Self {
        Self {
            provider: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            egress: EgressConfig::Open,
        }
    }

    fn validate(&self) -> std::result::Result<(), ServerError> {
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

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// Compile the stored egress config into a policy at the last boundary before
/// a managed sandbox is created. A malformed stored allowlist fails closed by
/// refusing execution rather than degrading to open egress.
fn resolve_egress_policy(
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
fn configured_e2b(
    credential: E2BCredential,
    timeout: Duration,
    pool: RemoteSessionPool,
    egress: &EgressConfig,
) -> std::result::Result<E2BExecutionProvider, CodeExecutionError> {
    let provider = E2BExecutionProvider::with_session_pool(credential, timeout, pool)?;
    Ok(match resolve_egress_policy(egress)? {
        Some(policy) => provider.with_egress_policy(policy),
        None => provider,
    })
}

/// Build the Daytona adapter with the configured egress policy applied. The
/// same policy compiles into Daytona's block-all switch and allowlists; an
/// over-limit allowlist is rejected here before any sandbox is created.
fn configured_daytona(
    credential: DaytonaCredential,
    timeout: Duration,
    pool: RemoteSessionPool,
    egress: &EgressConfig,
) -> std::result::Result<DaytonaExecutionProvider, CodeExecutionError> {
    let provider = DaytonaExecutionProvider::with_session_pool(credential, timeout, pool)?;
    match resolve_egress_policy(egress)? {
        Some(policy) => provider.with_egress_policy(policy),
        None => Ok(provider),
    }
}

/// Renderer-safe configuration and readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct CodeExecutionConfigInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<CodeExecutionProviderKind>,
    pub timeout_ms: u64,
    pub available: bool,
    pub has_credential: bool,
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
fn detached_admission_info(
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
const DAYTONA_TIER_REQUIREMENT: &str = "Daytona org tier 3+";

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
fn egress_enforcement_status() -> Vec<CodeExecutionEgressEnforcement> {
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
    ]
}

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
fn enforcement_row(
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
}

fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
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

async fn write_config(store: &dyn Store, config: &CodeExecutionConfig) -> Result<()> {
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
    let available = match config.provider {
        Some(CodeExecutionProviderKind::Local) => LocalExecutionProvider::is_supported(),
        Some(CodeExecutionProviderKind::E2b | CodeExecutionProviderKind::Daytona) => has_credential,
        None => false,
        _ => false,
    };
    Ok(CodeExecutionConfigInfo {
        provider: config.provider,
        timeout_ms: config.timeout_ms,
        available,
        has_credential,
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

async fn has_credential(secrets: &dyn SecretProvider, provider: CodeExecutionProviderKind) -> bool {
    match provider {
        CodeExecutionProviderKind::E2b => {
            E2BCredential::load(secrets).await.ok().flatten().is_some()
        }
        CodeExecutionProviderKind::Daytona => DaytonaCredential::load(secrets)
            .await
            .ok()
            .flatten()
            .is_some(),
        CodeExecutionProviderKind::Local => false,
        _ => false,
    }
}

fn credential_spec(
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

/// Late-binding provider used by the stable foreground tool registration.
pub struct ConfiguredCodeExecutionProvider {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    blobs: Option<Arc<dyn BlobStore>>,
    scratch_root: PathBuf,
    document_scripts_source: Option<PathBuf>,
    /// Built-in skills validated once at configuration and staged into every
    /// exec workspace; the prompt catalog is derived from the same load.
    skills: Arc<Vec<openwave_code_execution::LoadedSkill>>,
    /// Per-install directory of user-authored skill packages, re-read at each
    /// staging so an added or edited skill is picked up on the next turn
    /// without a restart. `None` disables user skills entirely.
    user_skills_dir: Option<PathBuf>,
    folder_grant_resolver: Option<Arc<dyn ExecFolderGrantResolver>>,
    /// Cross-process exclusion for the blobs a write-back snapshot publishes.
    blob_writes: Option<Arc<BlobWriteGuard>>,
    remote_sessions: RemoteSessionPool,
    /// The write overlay each chat's current turn is staging into.
    ///
    /// A turn opens one entry when it resolves its folder grants and closes it
    /// when the turn ends; every `exec` in between finds it here and points the
    /// sandbox at the staged copy instead of the user's folder.
    write_overlays: Mutex<HashMap<ChatId, StagedTurn>>,
    /// The shared package cache's runtime key, probed from the sandbox
    /// interpreter once per process. `None` disables the cache.
    package_cache_runtime: tokio::sync::OnceCell<Option<String>>,
    /// Whether a host-side cache population pass is running or has succeeded;
    /// cleared again on failure so a later exec can retry.
    package_cache_population: Arc<std::sync::atomic::AtomicBool>,
}

/// One turn's staging for one chat.
///
/// The overlay itself is addressed by folder path, which is what exec needs.
/// The host folder tools arrive with a product root id instead, so the same
/// staging is also indexed that way rather than making every caller re-resolve
/// a path through the broker to find it.
struct StagedTurn {
    /// The turn that opened this staging. The journal written at close belongs
    /// to the turn that staged the changes, not to whatever is running when the
    /// write-back applies them.
    turn: TurnId,
    overlay: WriteOverlay,
    staged_roots: HashMap<HostRootId, PathBuf>,
}

/// Where a chat's current turn stages writes for one granted folder.
///
/// For the length of a turn, exec addresses a private copy of each writable
/// granted folder rather than the folder itself, so the user's folder is the
/// stale view: a file the agent has just written is not in it yet, and one the
/// agent has deleted is still there. A host tool that reads the same folder in
/// the same turn consults this first, so the model is never shown two versions
/// of one folder.
///
/// The broker knows nothing about turn staging, but remains the live authority
/// behind every root resolution. Structured publications resolve that
/// authority again immediately before entering the shared materializer.
#[async_trait]
pub trait StagedFolders: Send + Sync {
    /// The staged copy of `root_id` for this chat's current turn, if the turn
    /// stages that folder. `None` covers every case where the user's folder is
    /// still the only view — no turn in flight, a read-only grant, or a folder
    /// the overlay could not stage.
    fn staged_root(&self, chat: ChatId, root_id: HostRootId) -> Option<PathBuf>;

    /// Publish one trusted file through the same conditional materializer and
    /// turn journal as an overlay write.
    async fn materialize_connected_file(
        &self,
        chat: ChatId,
        turn: TurnId,
        root_id: HostRootId,
        relative: &str,
        content: &[u8],
        expected: MaterializationPrecondition,
    ) -> std::result::Result<MaterializedChangeKind, RejectedChangeReason>;

    /// Reconcile an interrupted publication against its exact content.
    async fn connected_file_matches(
        &self,
        chat: ChatId,
        root_id: HostRootId,
        relative: &str,
        byte_len: u64,
        sha256: [u8; 32],
    ) -> bool;
}

#[async_trait]
impl StagedFolders for ConfiguredCodeExecutionProvider {
    fn staged_root(&self, chat: ChatId, root_id: HostRootId) -> Option<PathBuf> {
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .get(&chat)?
            .staged_roots
            .get(&root_id)
            .cloned()
    }

    async fn materialize_connected_file(
        &self,
        chat: ChatId,
        turn: TurnId,
        root_id: HostRootId,
        relative: &str,
        content: &[u8],
        expected: MaterializationPrecondition,
    ) -> std::result::Result<MaterializedChangeKind, RejectedChangeReason> {
        let folder = self.writable_connected_root(chat, root_id).await?;
        let snapshots =
            self.blobs
                .as_ref()
                .zip(self.blob_writes.as_ref())
                .map(|(blobs, blob_writes)| {
                    TurnSnapshotSink::new(self.store.clone(), blobs.clone(), blob_writes.clone())
                });
        let result = openwave_code_execution::materialize_file(
            &folder,
            relative,
            content,
            expected,
            snapshots
                .as_ref()
                .map(|sink| sink as &dyn WriteSnapshotSink),
        )
        .await;
        if result.is_ok() {
            if let Some(sink) = snapshots {
                if let Err(error) = sink.commit(chat, turn).await {
                    tracing::error!(
                        chat = %chat,
                        turn = %turn,
                        %error,
                        "could not journal a connected-folder publication; undo is unavailable"
                    );
                }
            }
        }
        result
    }

    async fn connected_file_matches(
        &self,
        chat: ChatId,
        root_id: HostRootId,
        relative: &str,
        byte_len: u64,
        sha256: [u8; 32],
    ) -> bool {
        let Ok(folder) = self.writable_connected_root(chat, root_id).await else {
            return false;
        };
        openwave_code_execution::materialized_file_matches(&folder, relative, byte_len, sha256)
            .await
    }
}

impl ConfiguredCodeExecutionProvider {
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        scratch_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            secrets,
            blobs: None,
            scratch_root: scratch_root.into(),
            document_scripts_source: None,
            skills: Arc::new(Vec::new()),
            user_skills_dir: None,
            folder_grant_resolver: None,
            blob_writes: None,
            remote_sessions: RemoteSessionPool::default(),
            write_overlays: Mutex::new(HashMap::new()),
            package_cache_runtime: tokio::sync::OnceCell::new(),
            package_cache_population: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Install the blob lifecycle lock the write-back snapshot publishes under.
    ///
    /// Without it — and without a blob store — staged writes are applied with no
    /// snapshot, which is the behavior granted folders had before this existed.
    #[must_use]
    pub(crate) fn with_blob_write_locks(mut self, blob_writes: Arc<BlobWriteGuard>) -> Self {
        self.blob_writes = Some(blob_writes);
        self
    }

    /// Install the blob store used to backfill attached documents before exec.
    #[must_use]
    pub fn with_blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// Install a trusted bundled helper directory into every exec workspace.
    #[must_use]
    pub fn with_document_scripts(mut self, source: Option<PathBuf>) -> Self {
        self.document_scripts_source = source;
        self
    }

    /// Load and install the built-in skill packages staged into every exec
    /// workspace. Malformed packages are skipped (with a warning) at this one
    /// load, so staging and the prompt catalog always agree. Headless
    /// embeddings leave the source absent.
    #[must_use]
    pub fn with_skills(mut self, source: Option<PathBuf>) -> Self {
        self.skills = Arc::new(
            source
                .as_deref()
                .map(|source| {
                    openwave_code_execution::load_skills(
                        source,
                        openwave_code_execution::SkillOrigin::Builtin,
                    )
                })
                .unwrap_or_default(),
        );
        self
    }

    /// Install the per-install directory user-authored skill packages are
    /// loaded from. The directory is created here (best effort) so the user
    /// has a place to drop a skill; its contents are re-read at each staging,
    /// so a new or edited skill takes effect on the next turn.
    #[must_use]
    pub fn with_user_skills(mut self, source: Option<PathBuf>) -> Self {
        if let Some(source) = source.as_deref() {
            if let Err(error) = std::fs::create_dir_all(source) {
                tracing::warn!(
                    "user skills directory {} could not be created: {error}",
                    source.display()
                );
            }
        }
        self.user_skills_dir = source;
        self
    }

    /// The built-in skills merged with a fresh read of the user skills
    /// directory. Built-ins were validated once at configuration; user
    /// packages go through the same strict loader here, so one staging and
    /// the catalog derived from it always agree. The read is a handful of
    /// small files at most.
    fn current_skills(&self) -> Vec<openwave_code_execution::LoadedSkill> {
        openwave_code_execution::merged_skills(&self.skills, self.user_skills_dir.as_deref())
    }

    /// The host-derived (name, description) catalog for prompt composition.
    pub(crate) fn skill_catalog(&self) -> Vec<openwave_code_execution::SkillPackage> {
        self.current_skills()
            .into_iter()
            .map(|skill| skill.package)
            .collect()
    }

    /// Stage the chat's workspace at turn start, before the model runs.
    ///
    /// The operating prompt tells the model to `read_file` a skill's
    /// `SKILL.md` *before* producing that kind of document, so the staging
    /// that `execute` performs on the first command comes strictly too late:
    /// the read races ahead of any exec and finds nothing. This runs the same
    /// idempotent preparation when the turn surface is composed. Best-effort
    /// on purpose — prompt enrichment is not an authority boundary, and
    /// `execute` re-prepares (with the provider-correct mirroring flag)
    /// before any command runs.
    pub(crate) async fn stage_turn_workspace(&self, chat_id: ChatId) {
        let skills = self.current_skills();
        if skills.is_empty() {
            return;
        }
        let host_dir = self.scratch_root.join(chat_id.to_string());
        if let Err(error) = prepare_execution_directories(
            &host_dir,
            false,
            self.document_scripts_source.as_deref(),
            &skills,
        )
        .await
        {
            tracing::warn!("turn-start workspace staging failed for chat {chat_id}: {error}");
        }
    }

    /// The shared package cache keyspace for the local sandbox interpreter.
    ///
    /// The wheel-compatibility runtime key is probed from the same interpreter
    /// the sandbox runs, once per process; `None` (an unusable interpreter, a
    /// non-macOS host, or an unopenable cache directory) disables the cache
    /// without affecting execution.
    async fn shared_package_cache(&self) -> Option<SharedPackageCache> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let key = self
            .package_cache_runtime
            .get_or_init(|| async {
                SharedPackageCache::runtime_key(std::path::Path::new(SANDBOX_PYTHON)).await
            })
            .await
            .clone()?;
        SharedPackageCache::open(&self.scratch_root.join(PACKAGE_CACHE_DIR), &key).ok()
    }

    /// Whether verified offline package installs are currently possible on the
    /// selected provider, for truthful operating-prompt steering.
    pub(crate) async fn offline_package_cache_ready(&self) -> bool {
        let Ok(config) = read_config(&*self.store).await else {
            return false;
        };
        if config.provider != Some(CodeExecutionProviderKind::Local) {
            return false;
        }
        match self.shared_package_cache().await {
            Some(cache) => cache.is_ready(),
            None => false,
        }
    }

    /// The host-configured per-command time limit, for truthful
    /// operating-prompt steering. Execution re-reads the setting per
    /// invocation; this is the same value rendered ahead of time so the model
    /// can plan long-running commands around it.
    pub(crate) async fn current_timeout_ms(&self) -> u64 {
        match read_config(&*self.store).await {
            Ok(config) => config.timeout_ms,
            Err(_) => DEFAULT_TIMEOUT_MS,
        }
    }

    /// Best-effort host-side acquisition of the built-in skills' pinned
    /// dependencies, spawned once per process when a networked local exec
    /// shows the cache could be used. Failure clears the latch so a later
    /// exec retries; conversations keep their network install path either way.
    /// User-authored skills are deliberately excluded: the pass runs once,
    /// user pins change under it, and their installs use the ordinary
    /// networked path like any other package.
    fn spawn_package_cache_population(&self, cache: SharedPackageCache) {
        use std::sync::atomic::Ordering;
        let pin_sets = self
            .skills
            .iter()
            .map(|skill| skill.package.python_deps.clone())
            .filter(|pins| !pins.is_empty())
            .collect::<Vec<_>>();
        if pin_sets.is_empty() {
            return;
        }
        if self.package_cache_population.swap(true, Ordering::SeqCst) {
            return;
        }
        let latch = self.package_cache_population.clone();
        tokio::spawn(async move {
            let mut failed = false;
            // Per-skill acquisition: each skill's pins resolve as one
            // consistent closure, and one unresolvable skill cannot sink the
            // others' artifacts.
            for pins in pin_sets {
                match cache
                    .populate_with_pip(std::path::Path::new(SANDBOX_PYTHON), &pins)
                    .await
                {
                    Ok(report) => tracing::info!(
                        promoted = report.promoted,
                        refused = report.refused,
                        invalidated = report.invalidated,
                        evicted = report.evicted,
                        "shared package cache population pass finished"
                    ),
                    Err(error) => {
                        failed = true;
                        tracing::warn!(%error, "shared package cache population failed");
                    }
                }
            }
            if failed {
                latch.store(false, Ordering::SeqCst);
            }
        });
    }

    /// Install the native bridge that resolves product root IDs through the
    /// live host broker. Non-desktop embeddings leave this absent.
    #[must_use]
    pub fn with_folder_grant_resolver(
        mut self,
        resolver: Option<Arc<dyn ExecFolderGrantResolver>>,
    ) -> Self {
        self.folder_grant_resolver = resolver;
        self
    }

    /// Resolve the local-exec roots visible in one turn's operating prompt.
    ///
    /// Managed providers cannot mount host folders, so they deliberately
    /// receive an empty list. The execution boundary resolves again on every
    /// invocation so a revocation after the prompt snapshot still fails closed.
    pub(crate) async fn folder_grants_for_chat(
        &self,
        chat: &Chat,
        turn: TurnId,
    ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, CodeExecutionError> {
        let config = read_config(&*self.store).await.map_err(|_| {
            CodeExecutionError::Unavailable("configuration storage is unavailable".into())
        })?;
        if config.provider != Some(CodeExecutionProviderKind::Local) || !cfg!(target_os = "macos") {
            return Ok(Vec::new());
        }
        let mut grants = self.resolve_chat_folder_grants(chat).await?;
        self.open_write_overlay(chat.id, turn, &mut grants).await;
        Ok(grants)
    }

    /// Stage this turn's writes for every writable granted folder.
    ///
    /// Called once, when the turn resolves the grants it will show the model.
    /// A folder that cannot be staged is downgraded to read-only for this turn:
    /// silently restoring direct writes would remove the overlay precisely for
    /// the largest or most unusual folders.
    async fn open_write_overlay(
        &self,
        chat: ChatId,
        turn: TurnId,
        grants: &mut [ResolvedExecFolderGrant],
    ) {
        let _ = self.close_write_overlay(chat).await;
        let writable = grants
            .iter()
            .filter(|grant| grant.writable)
            .map(|grant| grant.path.clone())
            .collect::<Vec<_>>();
        let scope = chat.to_string();
        let Some(overlay) = WriteOverlay::prepare(&self.scratch_root, &scope, &writable).await
        else {
            for grant in grants.iter_mut().filter(|grant| grant.writable) {
                grant.writable = false;
                grant.staging_unavailable = true;
            }
            return;
        };
        let mut staged_roots = HashMap::new();
        for slot in overlay.slots() {
            for grant in grants
                .iter_mut()
                .filter(|grant| grant.path == slot.source())
            {
                grant.overlay = Some(slot.overlay().to_path_buf());
                staged_roots.insert(grant.root_id, slot.overlay().to_path_buf());
            }
        }
        for grant in grants
            .iter_mut()
            .filter(|grant| grant.writable && grant.overlay.is_none())
        {
            grant.writable = false;
            grant.staging_unavailable = true;
        }
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .insert(
                chat,
                StagedTurn {
                    turn,
                    overlay,
                    staged_roots,
                },
            );
    }

    /// Apply this turn's staged writes to the user's folders and end staging.
    ///
    /// Every file the write-back replaces has its prior bytes retained first
    /// and journaled against `turn`, so the change summary and undo have
    /// something to work from. A turn that never staged anything finds nothing
    /// to do. A turn that is abandoned rather than finished never reaches here,
    /// and its staged writes are discarded when the next turn sweeps them:
    /// applying them later would write a folder that has since moved on.
    pub(crate) async fn close_write_overlay(&self, chat: ChatId) -> Option<TurnId> {
        let staged = self
            .write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .remove(&chat);
        let StagedTurn { turn, overlay, .. } = staged?;
        let snapshots =
            self.blobs
                .as_ref()
                .zip(self.blob_writes.as_ref())
                .map(|(blobs, blob_writes)| {
                    TurnSnapshotSink::new(self.store.clone(), blobs.clone(), blob_writes.clone())
                });
        let outcome = overlay
            .materialize(
                snapshots
                    .as_ref()
                    .map(|sink| sink as &dyn WriteSnapshotSink),
            )
            .await;
        let has_changes = !outcome.written.is_empty() || !outcome.rejected.is_empty();
        // The journal commits after the folders are written, not before: the
        // bytes it points at are already published, and a row for a write that
        // was refused would offer an undo for a change that never happened.
        if let Some(sink) = snapshots {
            if let Err(error) = sink.commit(chat, turn).await {
                tracing::error!(
                    chat = %chat,
                    turn = %turn,
                    %error,
                    "could not journal this turn's changes to granted folders; undo is unavailable for them"
                );
            }
        }
        let rejected = outcome
            .rejected
            .iter()
            .map(|file| ExecFileRejectionRecord {
                folder_path: file.folder.display().to_string(),
                relative_path: file.relative.clone(),
                reason: match file.reason {
                    RejectedChangeReason::Stale => ExecFileRejectionReason::Stale,
                    RejectedChangeReason::SnapshotUnavailable => {
                        ExecFileRejectionReason::SnapshotUnavailable
                    }
                    RejectedChangeReason::StagedFileTooLarge => {
                        ExecFileRejectionReason::StagedFileTooLarge
                    }
                    RejectedChangeReason::TrashUnavailable => {
                        ExecFileRejectionReason::TrashUnavailable
                    }
                    RejectedChangeReason::Unavailable => ExecFileRejectionReason::Unavailable,
                },
            })
            .collect::<Vec<_>>();
        if let Err(error) = self
            .store
            .record_exec_file_rejections(chat, turn, &rejected)
            .await
        {
            tracing::error!(
                chat = %chat,
                turn = %turn,
                %error,
                "could not journal this turn's rejected staged files"
            );
        }
        if !outcome.written.is_empty() || !outcome.rejected.is_empty() {
            let deleted = outcome
                .written
                .iter()
                .filter(|file| {
                    file.change == openwave_code_execution::MaterializedChangeKind::Deleted
                })
                .count();
            tracing::info!(
                chat = %chat,
                turn = %turn,
                written = outcome.written.len().saturating_sub(deleted),
                deleted,
                rejected = outcome.rejected.len(),
                "applied staged exec writes to granted folders"
            );
        }
        has_changes.then_some(turn)
    }

    /// The folder-to-overlay pairs this chat's current turn is staging.
    ///
    /// Only the paths leave the lock. The overlay itself is owned by the
    /// registry for exactly the length of the turn, so nothing an in-flight
    /// execution holds can keep it alive past the point where its writes are
    /// applied.
    fn staged_folders(&self, chat: ChatId) -> HashMap<PathBuf, PathBuf> {
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .get(&chat)
            .map(|staged| {
                staged
                    .overlay
                    .slots()
                    .iter()
                    .map(|slot| (slot.source().to_path_buf(), slot.overlay().to_path_buf()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn overlay_inspector(&self, chat: ChatId) -> Option<openwave_code_execution::OverlayInspector> {
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .get(&chat)
            .map(|staged| staged.overlay.inspector())
    }

    async fn resolve_chat_folder_grants(
        &self,
        chat: &Chat,
    ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, CodeExecutionError> {
        let Some(resolver) = self.folder_grant_resolver.as_ref() else {
            return Ok(Vec::new());
        };
        let root_ids = chat
            .root_attachments
            .iter()
            .map(|attachment| attachment.root_id)
            .collect::<Vec<_>>();
        if root_ids.is_empty() {
            return Ok(Vec::new());
        }
        let allowed = root_ids.iter().copied().collect::<HashSet<_>>();
        let resolved = resolver
            .resolve(ExecFolderGrantQuery {
                chat_id: chat.id,
                project_id: chat.project_id,
                root_ids: root_ids.clone(),
            })
            .await
            .map_err(CodeExecutionError::Sandbox)?;
        if resolved.len() > root_ids.len() {
            return Err(CodeExecutionError::Sandbox(
                "host returned too many execution folder grants".into(),
            ));
        }
        let mut by_id = HashMap::new();
        for grant in resolved {
            if !allowed.contains(&grant.root_id) || by_id.insert(grant.root_id, grant).is_some() {
                return Err(CodeExecutionError::Sandbox(
                    "host returned an invalid execution folder grant".into(),
                ));
            }
        }
        let mut ordered = Vec::new();
        for root_id in root_ids {
            if let Some(grant) = by_id.remove(&root_id) {
                ExecFolderGrant::new(
                    &grant.path,
                    if grant.writable {
                        ExecFolderAccess::ReadWrite
                    } else {
                        ExecFolderAccess::ReadOnly
                    },
                )?;
                ordered.push(grant);
            }
        }
        Ok(ordered)
    }

    async fn writable_connected_root(
        &self,
        chat_id: ChatId,
        root_id: HostRootId,
    ) -> std::result::Result<PathBuf, RejectedChangeReason> {
        let chat = self
            .store
            .get_chat(chat_id)
            .await
            .map_err(|_| RejectedChangeReason::Unavailable)?
            .ok_or(RejectedChangeReason::Unavailable)?;
        self.resolve_chat_folder_grants(&chat)
            .await
            .map_err(|_| RejectedChangeReason::Unavailable)?
            .into_iter()
            .find(|grant| grant.root_id == root_id && grant.writable)
            .map(|grant| grant.path)
            .ok_or(RejectedChangeReason::Unavailable)
    }

    /// Resolve the currently selected adapter at the last boundary before use.
    async fn resolve(
        &self,
        network_policy: Option<&NetworkPolicy>,
    ) -> std::result::Result<
        (CodeExecutionProviderKind, Box<dyn CodeExecutionProvider>),
        CodeExecutionError,
    > {
        let config = read_config(&*self.store).await.map_err(|_| {
            CodeExecutionError::Unavailable("configuration storage is unavailable".into())
        })?;
        let Some(provider) = config.provider else {
            return Err(CodeExecutionError::NotConfigured);
        };
        let resolved: Box<dyn CodeExecutionProvider> = match provider {
            CodeExecutionProviderKind::Local => {
                // Mounted only once verified artifacts exist; an empty or
                // unusable cache leaves execution exactly as it was.
                let package_cache = match self.shared_package_cache().await {
                    Some(cache) if cache.is_ready() => Some(cache.wheels_dir()),
                    _ => None,
                };
                Box::new(
                    LocalExecutionProvider::new(
                        &self.scratch_root,
                        Duration::from_millis(config.timeout_ms),
                    )?
                    .with_network_policy(network_policy.cloned().unwrap_or_default())
                    .with_document_scripts(self.document_scripts_source.clone())
                    .with_shared_package_cache(package_cache),
                )
            }
            CodeExecutionProviderKind::E2b => {
                let credential = E2BCredential::load(&*self.secrets)
                    .await?
                    .ok_or(CodeExecutionError::NotConfigured)?;
                let egress = network_policy
                    .map(network_egress_config)
                    .unwrap_or_else(|| config.egress.clone());
                Box::new(configured_e2b(
                    credential,
                    Duration::from_millis(config.timeout_ms),
                    self.remote_sessions.clone(),
                    &egress,
                )?)
            }
            CodeExecutionProviderKind::Daytona => {
                let credential = DaytonaCredential::load(&*self.secrets)
                    .await?
                    .ok_or(CodeExecutionError::NotConfigured)?;
                let egress = network_policy
                    .map(network_egress_config)
                    .unwrap_or_else(|| config.egress.clone());
                Box::new(configured_daytona(
                    credential,
                    Duration::from_millis(config.timeout_ms),
                    self.remote_sessions.clone(),
                    &egress,
                )?)
            }
            _ => {
                return Err(CodeExecutionError::Unavailable(
                    "selected provider is not supported by this build".into(),
                ))
            }
        };
        Ok((provider, resolved))
    }

    /// The configured provider's optional durable-workspace surface.
    ///
    /// Returns `Ok(None)` when execution is disabled, no provider is fully
    /// configured, or the selected backend has no workspace lifecycle, so host
    /// callers degrade instead of failing. This is a host-internal API; no
    /// model-facing tool is registered over it.
    pub async fn workspace(
        &self,
    ) -> std::result::Result<Option<ConfiguredWorkspace>, CodeExecutionError> {
        let provider = match self.resolve(None).await {
            Ok((_, provider)) => provider,
            Err(CodeExecutionError::NotConfigured) => return Ok(None),
            Err(error) => return Err(error),
        };
        if provider.workspace_lifecycle().is_none() {
            return Ok(None);
        }
        Ok(Some(ConfiguredWorkspace { provider }))
    }
}

fn exec_folder_grant_for_turn(
    grant: ResolvedExecFolderGrant,
    staged: &HashMap<PathBuf, PathBuf>,
) -> std::result::Result<ExecFolderGrant, CodeExecutionError> {
    let overlay = grant
        .writable
        .then(|| staged.get(&grant.path))
        .flatten()
        .cloned();
    // A live broker write grant is necessary but no longer sufficient: this
    // turn must also have staged the folder. Missing staging fails closed
    // instead of quietly restoring unrestricted writes to the real root.
    let writable = grant.writable && overlay.is_some();
    let resolved = ExecFolderGrant::new(
        grant.path,
        if writable {
            ExecFolderAccess::ReadWrite
        } else {
            ExecFolderAccess::ReadOnly
        },
    )?;
    match overlay {
        Some(overlay) => resolved.staged_at(overlay),
        None => Ok(resolved),
    }
}

#[async_trait]
impl CodeExecutionProvider for ConfiguredCodeExecutionProvider {
    async fn execute(
        &self,
        mut request: CodeExecutionRequest,
    ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
        if !request.folder_grants.is_empty() {
            return Err(CodeExecutionError::InvalidRequest(
                "execution folder grants are host-resolved state".into(),
            ));
        }
        let chat_id = request
            .workspace_id
            .as_str()
            .parse::<ChatId>()
            .map_err(|_| {
                CodeExecutionError::InvalidRequest(
                    "execution workspace does not identify a conversation".into(),
                )
            })?;
        let chat = self
            .store
            .get_chat(chat_id)
            .await
            .map_err(|_| {
                CodeExecutionError::Unavailable("conversation storage is unavailable".into())
            })?
            .ok_or_else(|| {
                CodeExecutionError::InvalidRequest("execution conversation does not exist".into())
            })?;
        let (kind, provider) = self.resolve(Some(&chat.network_policy)).await?;
        if kind == CodeExecutionProviderKind::Local
            && permits_package_installs(&chat.network_policy)
        {
            // A networked local exec is the signal that installs are wanted:
            // the same pins a conversation installs under its per-chat HOME
            // are acquired host-side into the shared cache, so a later
            // conversation can install them with the network off.
            if let Some(cache) = self.shared_package_cache().await {
                self.spawn_package_cache_population(cache);
            }
        }
        if kind == CodeExecutionProviderKind::Local && cfg!(target_os = "macos") {
            // Authority is resolved again here rather than reused from the
            // turn's prompt snapshot, so a revocation mid-turn fails closed.
            // Staging is looked up rather than re-established: the overlay
            // belongs to the turn, and every command in it writes to the same
            // staged tree.
            let staged = self.staged_folders(chat_id);
            let grants = self
                .resolve_chat_folder_grants(&chat)
                .await?
                .into_iter()
                .map(|grant| exec_folder_grant_for_turn(grant, &staged))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            request = request.with_folder_grants(grants)?;
        }
        let host_dir = self.scratch_root.join(request.workspace_id.as_str());
        let skills = self.current_skills();
        prepare_execution_directories(
            &host_dir,
            kind != CodeExecutionProviderKind::Local,
            self.document_scripts_source.as_deref(),
            &skills,
        )
        .await?;
        if let Some(blobs) = self.blobs.as_deref() {
            let chat_id = request
                .workspace_id
                .as_str()
                .parse::<ChatId>()
                .map_err(|_| {
                    CodeExecutionError::InvalidRequest(
                        "execution workspace does not identify a conversation".into(),
                    )
                })?;
            materialize_chat_attachments(&*self.store, blobs, chat_id, &host_dir).await?;
        }
        // A remote sandbox has its own filesystem, but the model is shown one
        // path vocabulary across the file tools and exec. Stage exactly the
        // paths the model listed on this call into the workspace before the
        // command, and pull only output/ and preview/ back out afterwards —
        // the two directories the host output and preview scans read. The
        // local provider already runs inside scratch, so nothing is staged
        // there, but the listed paths are validated identically so a bad path
        // fails the same way on every provider.
        let lifecycle = match kind {
            CodeExecutionProviderKind::Local => None,
            _ => provider.workspace_lifecycle(),
        };
        let Some(lifecycle) = lifecycle else {
            sync::validate_staged_paths(&host_dir, &request.files).await?;
            let inspector = request
                .workspace_id
                .as_str()
                .parse::<ChatId>()
                .ok()
                .and_then(|chat| self.overlay_inspector(chat));
            let mut response = provider.execute(request).await?;
            if let Some(inspector) = inspector {
                response.sync_notes.extend(inspector.notes().await);
            }
            return Ok(response);
        };
        // A staging that fails outright fails the execution: a listed path
        // that does not exist, an over-bound expansion, or an unreachable
        // workspace would otherwise surface as a baffling not-found inside the
        // sandbox. Entries a listed directory had to leave behind individually
        // ride along as notes instead.
        let mut staged_paths =
            implicit_staged_paths(self.document_scripts_source.is_some(), !skills.is_empty());
        staged_paths.extend(request.files.iter().cloned());
        let mut notes =
            sync::stage_listed_paths(lifecycle, &request.workspace_id, &host_dir, &staged_paths)
                .await?
                .notes;
        let mut response = provider.execute(request.clone()).await?;
        // A failed pull keeps the execution's output — the command did run —
        // and says the host copies are stale instead of failing the call.
        match sync::pull_result_dirs(lifecycle, &request.workspace_id, &host_dir).await {
            Ok(pulled) => notes.extend(pulled.notes),
            Err(error) => notes.push(format!(
                "output files were not copied back to private scratch: {error}"
            )),
        }
        // A failed command plus an empty or thin staged set usually means the
        // command's inputs were never listed; one bounded line points there.
        if response.timed_out || response.exit_code != Some(0) {
            notes.push(staged_set_note(&request.files));
        }
        response.sync_notes.extend(notes);
        Ok(response)
    }

    async fn collect_preview_images(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<PreviewScan, CodeExecutionError> {
        let preview_dir = self.scratch_root.join(workspace.as_str()).join("preview");
        tokio::task::spawn_blocking(move || {
            openwave_code_execution::scan_preview_directory(&preview_dir)
        })
        .await
        .map_err(|_| CodeExecutionError::Sandbox("preview scan task failed".into()))
    }

    async fn collect_output_artifacts(
        &self,
        workspace: &ExecutionWorkspaceId,
        execution: &ExecutionId,
    ) -> std::result::Result<OutputArtifactScan, CodeExecutionError> {
        let chat_id = workspace.as_str().parse::<ChatId>().map_err(|_| {
            CodeExecutionError::InvalidRequest(
                "execution workspace does not identify a conversation".into(),
            )
        })?;
        let call_id = execution.as_str().parse::<CallId>().map_err(|_| {
            CodeExecutionError::InvalidRequest(
                "execution does not carry a canonical tool-call identity".into(),
            )
        })?;
        // The revision's producer is the turn that owns this exec call, read
        // from the durable call record rather than anything the model asserts.
        let calls = self.store.list_tool_calls(chat_id).await.map_err(|_| {
            CodeExecutionError::Unavailable("tool-call storage is unavailable".into())
        })?;
        let turn_id = calls
            .into_iter()
            .find(|call| call.id == call_id)
            .map(|call| call.turn_id)
            .ok_or_else(|| {
                CodeExecutionError::InvalidRequest(
                    "execution identity is not owned by this conversation".into(),
                )
            })?;

        let scratch_path = self.scratch_root.join(workspace.as_str());
        let scratch = tokio::task::spawn_blocking(move || {
            cap_std::fs::Dir::open_ambient_dir(&scratch_path, cap_std::ambient_authority())
        })
        .await
        .map_err(|_| CodeExecutionError::Sandbox("output scan task failed".into()))?
        .map_err(|_| CodeExecutionError::Sandbox("the private workspace is unavailable".into()))?;

        let sync = openwave_core::sync_output_directory(
            &*self.store,
            &scratch,
            chat_id,
            call_id,
            RevisionProducer::Turn(turn_id),
            Utc::now(),
        )
        .await
        .map_err(|error| {
            CodeExecutionError::Unavailable(format!("outputs could not be recorded: {error}"))
        })?;
        Ok(OutputArtifactScan {
            entries: sync
                .entries
                .into_iter()
                .map(|entry| OutputArtifactEntry {
                    filename: entry.filename,
                    output_id: entry.output_id.to_string(),
                    ordinal: entry.ordinal,
                    status: match entry.status {
                        openwave_core::OutputSyncStatus::Created => OutputArtifactStatus::Created,
                        openwave_core::OutputSyncStatus::Updated => OutputArtifactStatus::Updated,
                        openwave_core::OutputSyncStatus::Unchanged => {
                            OutputArtifactStatus::Unchanged
                        }
                    },
                })
                .collect(),
            notes: sync.notes,
        })
    }

    // `workspace_lifecycle` stays `None` here on purpose: the capability of
    // this late-binding wrapper depends on the configuration read at call
    // time, which the synchronous trait flag cannot express. Host callers use
    // [`ConfiguredCodeExecutionProvider::workspace`] instead.
}

/// Host infrastructure staged into every managed workspace regardless of the
/// model's listed set: the conventional-directory markers that make `output/`
/// and `preview/` exist remotely so commands can write into them, and the
/// bundled document helpers the tool description tells the model to invoke
/// without listing. All of it is host-authored, bounded, and digest-skipped on
/// a reused session.
fn implicit_staged_paths(with_document_scripts: bool, with_skills: bool) -> Vec<WorkspaceFilePath> {
    let mut paths = vec![
        "output/.openwave-directory".to_owned(),
        "preview/.openwave-directory".to_owned(),
    ];
    if with_document_scripts {
        paths.push(DOCUMENT_SCRIPTS_DIR.to_owned());
    }
    if with_skills {
        paths.push(openwave_code_execution::SKILLS_DIR.to_owned());
    }
    paths
        .into_iter()
        .filter_map(|path| WorkspaceFilePath::parse(path).ok())
        .collect()
}

/// One bounded line naming what this call staged, appended to a failed managed
/// command so a missing-input failure points at the `files` argument.
fn staged_set_note(files: &[WorkspaceFilePath]) -> String {
    const SHOWN: usize = 8;
    if files.is_empty() {
        return "staged: none — list the files the command needs in the exec 'files' argument"
            .into();
    }
    let shown: Vec<&str> = files
        .iter()
        .take(SHOWN)
        .map(WorkspaceFilePath::as_str)
        .collect();
    let mut note = format!("staged: {}", shown.join(", "));
    let omitted = files.len().saturating_sub(SHOWN);
    if omitted > 0 {
        note.push_str(&format!(" (+{omitted} more)"));
    }
    note
}

async fn materialize_chat_attachments(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    chat_id: ChatId,
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    let attachments = store
        .list_message_document_attachments(chat_id)
        .await
        .map_err(|_| CodeExecutionError::Unavailable("attachment storage is unavailable".into()))?;
    materialize_attachments(&attachments, blobs, host_dir).await
}

async fn materialize_attachments(
    attachments: &[MessageDocumentAttachment],
    blobs: &dyn BlobStore,
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    let documents_dir = host_dir.join("documents");
    let metadata = tokio::fs::symlink_metadata(&documents_dir)
        .await
        .map_err(|_| CodeExecutionError::Sandbox("documents/ is unavailable".into()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CodeExecutionError::Sandbox(
            "documents/ is not a private workspace directory".into(),
        ));
    }

    let mut materialized = HashSet::new();
    for attachment in attachments {
        let Some(source_blob) = attachment.source_blob.as_ref() else {
            continue;
        };
        if source_blob.byte_len > MAX_EXEC_WORKSPACE_FILE_BYTES as u64 {
            continue;
        }
        let file_name =
            exec_attachment_file_name(attachment.title.as_deref(), attachment.document_id);
        if !materialized.insert(file_name.clone()) {
            continue;
        }
        let bytes = blobs.get(source_blob.id).await.map_err(|_| {
            CodeExecutionError::Unavailable("attached document bytes are unavailable".into())
        })?;
        let Some(bytes) = bytes else {
            return Err(CodeExecutionError::Unavailable(
                "attached document bytes are unavailable".into(),
            ));
        };
        if bytes.len() > MAX_EXEC_WORKSPACE_FILE_BYTES
            || openwave_core::DocumentSourceBlob::from_bytes(&bytes) != *source_blob
        {
            return Err(CodeExecutionError::Unavailable(
                "attached document bytes do not match their stored descriptor".into(),
            ));
        }
        let destination = documents_dir.join(&file_name);
        match tokio::fs::symlink_metadata(&destination).await {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(CodeExecutionError::Sandbox(format!(
                    "documents/{file_name} is not a regular workspace file"
                )));
            }
            Ok(_) => {
                if tokio::fs::read(&destination)
                    .await
                    .is_ok_and(|existing| existing == bytes)
                {
                    continue;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CodeExecutionError::Sandbox(format!(
                    "documents/{file_name} is unavailable"
                )));
            }
        }
        tokio::fs::write(&destination, bytes).await.map_err(|_| {
            CodeExecutionError::Sandbox(format!(
                "attached document documents/{file_name} could not be materialized"
            ))
        })?;
    }
    Ok(())
}

async fn prepare_execution_directories(
    host_dir: &std::path::Path,
    mirrored: bool,
    document_scripts_source: Option<&std::path::Path>,
    skills: &[openwave_code_execution::LoadedSkill],
) -> std::result::Result<(), CodeExecutionError> {
    // The scratch directory itself is host-owned and named after the chat, but
    // everything inside it is writable by local exec, which can plant
    // `<scratch>/output -> /any/dir` between two runs. `create_dir_all` and a
    // plain `write` both follow a symlinked parent, so each conventional
    // directory is resolved a component at a time into an open descriptor and
    // the marker is written relative to that descriptor, without following a
    // link at the final component either.
    tokio::fs::create_dir_all(host_dir).await.map_err(|_| {
        CodeExecutionError::Sandbox("the private workspace directory is unavailable".into())
    })?;
    for name in ["output", "preview", "documents"] {
        let unavailable = || {
            CodeExecutionError::Sandbox(format!(
                "private workspace directory '{name}/' is unavailable"
            ))
        };
        let directory = resolve_scratch_directory(host_dir, name, true)
            .await
            .ok_or_else(unavailable)?;
        if mirrored {
            // Staging transfers files rather than empty directories. A hidden
            // zero-byte marker makes the conventional directories exist in
            // managed workspaces without becoming a user artifact.
            directory
                .write_file(".openwave-directory", &[])
                .await
                .map_err(|_| unavailable())?;
        }
    }
    if let Some(source) = document_scripts_source {
        install_document_scripts(source, host_dir).await?;
    }
    install_skills(skills, host_dir).await?;
    Ok(())
}

/// Stage the validated skills (built-in and user-authored) into
/// `.openwave/skills/<name>/`.
///
/// Each destination is resolved a component at a time for the same reason the
/// helper install is: `.openwave/` is writable by local exec, so a planted
/// symlink must not relocate the staged files. Content was validated at
/// configuration; a failure here means the workspace itself is unusable.
async fn install_skills(
    skills: &[openwave_code_execution::LoadedSkill],
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    for skill in skills {
        let name = &skill.package.name;
        let destination = resolve_scratch_directory(
            host_dir,
            &format!("{}/{name}", openwave_code_execution::SKILLS_DIR),
            true,
        )
        .await
        .ok_or_else(|| {
            CodeExecutionError::Sandbox(format!("skill directory '{name}' is unavailable"))
        })?;
        destination
            .write_file(
                openwave_code_execution::SKILL_MANIFEST_FILE,
                skill.manifest.as_bytes(),
            )
            .await
            .map_err(|_| {
                CodeExecutionError::Sandbox(format!("skill '{name}' could not be installed"))
            })?;
    }
    Ok(())
}

async fn install_document_scripts(
    source: &std::path::Path,
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    // `.openwave/` sits inside the scratch directory local exec writes to, so
    // a planted `.openwave -> /elsewhere` would relocate the helper install
    // and truncate known filenames there. Resolve it a component at a time and
    // keep the descriptor, so the helpers land in the directory the walk proved
    // rather than whatever the name points at by the time we write.
    let destination = resolve_scratch_directory(host_dir, DOCUMENT_SCRIPTS_DIR, true)
        .await
        .ok_or_else(|| {
            CodeExecutionError::Sandbox("document helper directory is unavailable".into())
        })?;
    for name in DOCUMENT_SCRIPT_FILES {
        let source_file = source.join(name);
        let metadata = tokio::fs::symlink_metadata(&source_file)
            .await
            .map_err(|_| {
                CodeExecutionError::Sandbox(format!(
                    "bundled document helper '{name}' is unavailable"
                ))
            })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' is not a regular file"
            )));
        }
        let content = tokio::fs::read(&source_file).await.map_err(|_| {
            CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' could not be read"
            ))
        })?;
        if content.len() > openwave_code_execution::MAX_WORKSPACE_FILE_BYTES {
            return Err(CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' exceeds the workspace file limit"
            )));
        }
        destination.write_file(name, &content).await.map_err(|_| {
            CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' could not be installed"
            ))
        })?;
    }
    Ok(())
}

/// A resolved workspace-lifecycle handle over the currently selected provider.
pub struct ConfiguredWorkspace {
    provider: Box<dyn CodeExecutionProvider>,
}

impl ConfiguredWorkspace {
    fn lifecycle(&self) -> std::result::Result<&dyn WorkspaceLifecycle, CodeExecutionError> {
        // Checked when this handle was constructed; re-checked instead of
        // unwrapped so a defect degrades into an error, not a panic.
        self.provider.workspace_lifecycle().ok_or_else(|| {
            CodeExecutionError::Unavailable("selected provider lost its workspace surface".into())
        })
    }
}

#[async_trait]
impl WorkspaceLifecycle for ConfiguredWorkspace {
    async fn create_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<(), CodeExecutionError> {
        self.lifecycle()?.create_workspace(workspace).await
    }

    async fn connect_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<bool, CodeExecutionError> {
        self.lifecycle()?.connect_workspace(workspace).await
    }

    async fn destroy_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<(), CodeExecutionError> {
        self.lifecycle()?.destroy_workspace(workspace).await
    }

    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> std::result::Result<(), CodeExecutionError> {
        self.lifecycle()?
            .put_workspace_file(workspace, path, content)
            .await
    }

    async fn stage_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> std::result::Result<StagedUpload, CodeExecutionError> {
        // Delegated rather than left to the trait default so the selected
        // backend's session memory is not bypassed by the wrapper.
        self.lifecycle()?
            .stage_workspace_file(workspace, path, content)
            .await
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> std::result::Result<Vec<u8>, CodeExecutionError> {
        self.lifecycle()?.get_workspace_file(workspace, path).await
    }

    async fn list_workspace_files(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: Option<&WorkspaceFilePath>,
    ) -> std::result::Result<WorkspaceListing, CodeExecutionError> {
        self.lifecycle()?
            .list_workspace_files(workspace, path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use openwave_core::{
        AgentError, ChatRootAttachment, DbStore, DocumentId, DocumentSourceBlob,
        DocumentSourceUpsert, FsBlobStore, PermissionMode, RootAttachmentOrigin, TurnId,
    };
    use std::sync::Mutex;
    use uuid::Uuid;

    struct NoSecrets;

    struct RecordingFolderResolver {
        queries: Mutex<Vec<ExecFolderGrantQuery>>,
        roots: Vec<ResolvedExecFolderGrant>,
    }

    #[async_trait]
    impl ExecFolderGrantResolver for RecordingFolderResolver {
        async fn resolve(
            &self,
            query: ExecFolderGrantQuery,
        ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, String> {
            self.queries.lock().unwrap().push(query);
            Ok(self.roots.clone())
        }
    }

    #[async_trait]
    impl SecretProvider for NoSecrets {
        async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }

        async fn delete_secret(&self, _key: &str) -> Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }
    }

    async fn test_store() -> (DbStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("code-execution.db").display()
        ))
        .await
        .unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn folder_resolution_is_fenced_to_the_chat_projection() {
        let (store, _database) = test_store().await;
        let granted = HostRootId::from_uuid(Uuid::new_v4()).unwrap();
        let injected = HostRootId::from_uuid(Uuid::new_v4()).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let resolver = Arc::new(RecordingFolderResolver {
            queries: Mutex::new(Vec::new()),
            roots: vec![ResolvedExecFolderGrant {
                root_id: granted,
                path: folder.path().to_path_buf(),
                writable: false,
                overlay: None,
                staging_unavailable: false,
            }],
        });
        let provider = ConfiguredCodeExecutionProvider::new(
            Arc::new(store),
            Arc::new(NoSecrets),
            tempfile::tempdir().unwrap().path(),
        )
        .with_folder_grant_resolver(Some(resolver.clone()));
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: Some(PermissionMode::Ask),
            network_policy: Default::default(),
            attachment_revision: 1,
            root_attachments: vec![ChatRootAttachment {
                root_id: granted,
                origin: RootAttachmentOrigin::Conversation,
            }],
            created_at: Utc::now(),
        };

        let resolved = provider.resolve_chat_folder_grants(&chat).await.unwrap();
        assert_eq!(resolved[0].root_id, granted);
        assert_eq!(resolver.queries.lock().unwrap()[0].root_ids, vec![granted]);

        let bad_resolver = Arc::new(RecordingFolderResolver {
            queries: Mutex::new(Vec::new()),
            roots: vec![ResolvedExecFolderGrant {
                root_id: injected,
                path: folder.path().to_path_buf(),
                writable: true,
                overlay: None,
                staging_unavailable: false,
            }],
        });
        let (store, _database) = test_store().await;
        let bad_provider = ConfiguredCodeExecutionProvider::new(
            Arc::new(store),
            Arc::new(NoSecrets),
            tempfile::tempdir().unwrap().path(),
        )
        .with_folder_grant_resolver(Some(bad_resolver));
        assert!(bad_provider
            .resolve_chat_folder_grants(&chat)
            .await
            .is_err());
    }

    /// A tree outside the overlay's bounded contract must lose exec write
    /// access rather than regaining direct access to the user's real folder.
    #[tokio::test]
    async fn a_folder_that_cannot_be_staged_fails_closed_and_stays_visible() {
        let (store, _database) = test_store().await;
        let scratch = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut nested = folder.path().to_path_buf();
        for depth in 0..30 {
            nested.push(format!("level-{depth}"));
            std::fs::create_dir(&nested).unwrap();
        }
        let root_id = HostRootId::from_uuid(Uuid::new_v4()).unwrap();
        let provider = ConfiguredCodeExecutionProvider::new(
            Arc::new(store),
            Arc::new(NoSecrets),
            scratch.path(),
        );
        let mut grants = vec![ResolvedExecFolderGrant {
            root_id,
            path: folder.path().to_path_buf(),
            writable: true,
            overlay: None,
            staging_unavailable: false,
        }];

        provider
            .open_write_overlay(ChatId::new(), TurnId::new(), &mut grants)
            .await;

        assert!(!grants[0].writable);
        assert!(grants[0].staging_unavailable);
        assert!(grants[0].overlay.is_none());
        let effective = exec_folder_grant_for_turn(grants.remove(0), &HashMap::new()).unwrap();
        assert_eq!(effective.access, ExecFolderAccess::ReadOnly);
        assert!(effective.writable_path().is_none());
    }

    /// The files-first creation path end to end at the provider seam: a file
    /// written to `output/` becomes a turn-attributed output, an identical
    /// rerun mints nothing, and changed bytes append a revision.
    #[tokio::test]
    async fn output_files_publish_as_turn_attributed_outputs() {
        use openwave_core::model::{ToolCallExecution, ToolCallRecord, ToolCallStatus};
        use openwave_core::{CallId, TurnId};

        let (store, _database) = test_store().await;
        let store = Arc::new(store);
        let scratch_root = tempfile::tempdir().unwrap();
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        let accept_exec_call = |store: Arc<DbStore>| async move {
            let call_id = CallId::new();
            store
                .accept_tool_call(&ToolCallRecord {
                    id: call_id,
                    chat_id: chat.id,
                    turn_id,
                    provider_id: format!("provider-{call_id}"),
                    name: "exec".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    result_preview: None,
                    error_code: None,
                    error_detail: None,
                    client_executor_id: None,
                    client_lease_expires_at: None,
                    created_at: Utc::now(),
                    resolved_at: None,
                })
                .await
                .unwrap();
            call_id
        };
        let call_id = accept_exec_call(store.clone()).await;

        let output_dir = scratch_root.path().join(chat.id.to_string()).join("output");
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(output_dir.join("report.md"), b"# Draft").unwrap();

        let provider = ConfiguredCodeExecutionProvider::new(
            store.clone(),
            Arc::new(NoSecrets),
            scratch_root.path(),
        );
        let workspace = ExecutionWorkspaceId::parse(chat.id.to_string()).unwrap();
        let execution = ExecutionId::parse(call_id.to_string()).unwrap();

        let scan = provider
            .collect_output_artifacts(&workspace, &execution)
            .await
            .unwrap();
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].status, OutputArtifactStatus::Created);
        let outputs = store.list_outputs(chat.id, 10).await.unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].filename, "report.md");
        let revision = store
            .get_output_revision(outputs[0].current_revision)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(revision.turn_id, Some(turn_id));

        // Identical rerun: nothing minted.
        let rerun = provider
            .collect_output_artifacts(&workspace, &execution)
            .await
            .unwrap();
        assert_eq!(rerun.entries[0].status, OutputArtifactStatus::Unchanged);
        assert_eq!(
            store.list_outputs(chat.id, 10).await.unwrap()[0].revision_count,
            1
        );

        // Changed bytes from a later call: a revision on the same output. (The
        // same call identity republishing different bytes is refused by the
        // write-once path, so each update rides its own call.)
        std::fs::write(output_dir.join("report.md"), b"# Final").unwrap();
        let later_call = accept_exec_call(store.clone()).await;
        let later_execution = ExecutionId::parse(later_call.to_string()).unwrap();
        let changed = provider
            .collect_output_artifacts(&workspace, &later_execution)
            .await
            .unwrap();
        assert_eq!(changed.entries[0].status, OutputArtifactStatus::Updated);
        let updated = store.list_outputs(chat.id, 10).await.unwrap();
        assert_eq!(updated[0].id, outputs[0].id);
        assert_eq!(updated[0].revision_count, 2);

        // A call identity the conversation does not own publishes nothing.
        let foreign = ExecutionId::parse(CallId::new().to_string()).unwrap();
        assert!(provider
            .collect_output_artifacts(&workspace, &foreign)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn exec_workspace_conventions_exist_before_a_command_runs() {
        let dir = tempfile::tempdir().unwrap();
        prepare_execution_directories(dir.path(), true, None, &[])
            .await
            .unwrap();

        for name in ["output", "preview", "documents"] {
            assert!(dir.path().join(name).is_dir());
            assert!(dir.path().join(name).join(".openwave-directory").is_file());
        }
    }

    #[tokio::test]
    async fn attached_documents_backfill_lazily_with_collision_and_size_limits() {
        let (store, database) = test_store().await;
        let blobs = FsBlobStore::new(database.path().join("blobs"));
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let first_bytes = b"first opaque attachment";
        let first_blob = DocumentSourceBlob::from_bytes(first_bytes);
        blobs
            .put(first_blob.id, first_bytes.to_vec())
            .await
            .unwrap();
        let second_bytes = b"second opaque attachment";
        let second_blob = DocumentSourceBlob::from_bytes(second_bytes);
        blobs
            .put(second_blob.id, second_bytes.to_vec())
            .await
            .unwrap();
        let oversized_blob =
            DocumentSourceBlob::from_digest([7; 32], MAX_EXEC_WORKSPACE_FILE_BYTES as u64 + 1);
        let first_id = DocumentId::new();
        let second_id = DocumentId::new();
        let oversized_id = DocumentId::new();
        for source in [
            DocumentSourceUpsert {
                id: first_id,
                chat_id: Some(chat.id),
                project_id: None,
                source_uri: None,
                media_type: "application/pdf".into(),
                title: Some("report.pdf".into()),
                source_blob: first_blob,
                canonical_text: String::new(),
                updated_at: Utc::now(),
            },
            DocumentSourceUpsert {
                id: second_id,
                chat_id: Some(chat.id),
                project_id: None,
                source_uri: None,
                media_type: "application/pdf".into(),
                title: Some("report.pdf".into()),
                source_blob: second_blob,
                canonical_text: String::new(),
                updated_at: Utc::now(),
            },
            DocumentSourceUpsert {
                id: oversized_id,
                chat_id: Some(chat.id),
                project_id: None,
                source_uri: None,
                media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                    .into(),
                title: Some("large.xlsx".into()),
                source_blob: oversized_blob,
                canonical_text: String::new(),
                updated_at: Utc::now(),
            },
        ] {
            store.accept_document_source(&source).await.unwrap();
        }
        store
            .accept_turn_with_attachments(
                TurnId::new(),
                chat.id,
                "gpt-5",
                "inspect these",
                &[],
                &[first_id, second_id, oversized_id],
            )
            .await
            .unwrap();

        let workspace = database.path().join("scratch").join(chat.id.to_string());
        prepare_execution_directories(&workspace, false, None, &[])
            .await
            .unwrap();
        materialize_chat_attachments(&store, &blobs, chat.id, &workspace)
            .await
            .unwrap();

        let first_path = workspace
            .join("documents")
            .join(exec_attachment_file_name(Some("report.pdf"), first_id));
        let second_path = workspace
            .join("documents")
            .join(exec_attachment_file_name(Some("report.pdf"), second_id));
        let oversized_path = workspace
            .join("documents")
            .join(exec_attachment_file_name(Some("large.xlsx"), oversized_id));
        assert_ne!(first_path, second_path);
        assert_eq!(std::fs::read(&first_path).unwrap(), first_bytes);
        assert_eq!(std::fs::read(&second_path).unwrap(), second_bytes);
        assert!(!oversized_path.exists());

        // Materialization runs at each invocation, so a later first exec or a
        // modified workspace still sees the immutable original attachment.
        std::fs::write(&first_path, b"workspace edit").unwrap();
        materialize_chat_attachments(&store, &blobs, chat.id, &workspace)
            .await
            .unwrap();
        assert_eq!(std::fs::read(first_path).unwrap(), first_bytes);
    }

    #[tokio::test]
    async fn bundled_document_helpers_are_installed_as_one_library() {
        let source = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        for name in DOCUMENT_SCRIPT_FILES {
            std::fs::write(source.path().join(name), format!("helper:{name}")).unwrap();
        }

        let skill = openwave_code_execution::LoadedSkill {
            package: openwave_code_execution::SkillPackage {
                name: "pdf-documents".into(),
                description: "Produce PDFs.".into(),
                python_deps: vec!["fpdf2==2.8.3".into()],
                host_deps: Vec::new(),
                origin: openwave_code_execution::SkillOrigin::Builtin,
            },
            manifest: "---\nname: pdf-documents\n---\nbody".into(),
        };
        prepare_execution_directories(
            workspace.path(),
            false,
            Some(source.path()),
            std::slice::from_ref(&skill),
        )
        .await
        .unwrap();

        for name in DOCUMENT_SCRIPT_FILES {
            let installed = workspace.path().join(DOCUMENT_SCRIPTS_DIR).join(name);
            assert_eq!(
                std::fs::read_to_string(installed).unwrap(),
                format!("helper:{name}")
            );
        }
        let staged_skill = workspace
            .path()
            .join(openwave_code_execution::SKILLS_DIR)
            .join("pdf-documents")
            .join(openwave_code_execution::SKILL_MANIFEST_FILE);
        assert_eq!(
            std::fs::read_to_string(staged_skill).unwrap(),
            skill.manifest
        );
    }

    /// Turn-start staging pins the contract the prompt catalog relies on:
    /// every advertised skill's `SKILL.md` is readable in the chat's private
    /// scratch — the directory the `read_file` surface resolves against —
    /// before any exec has run.
    #[tokio::test]
    async fn turn_start_staging_makes_skills_readable_before_any_exec() {
        let (store, _database) = test_store().await;
        let store = Arc::new(store);
        let scratch_root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let manifest = "---\n\
            name: presentations\n\
            description: Create PowerPoint decks.\n\
            ---\n\
            Body.\n";
        let skill_dir = source.path().join("presentations");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join(openwave_code_execution::SKILL_MANIFEST_FILE),
            manifest,
        )
        .unwrap();

        let provider = ConfiguredCodeExecutionProvider::new(
            store.clone(),
            Arc::new(NoSecrets),
            scratch_root.path(),
        )
        .with_skills(Some(source.path().to_owned()))
        .with_user_skills(Some(scratch_root.path().join("user-skills")));
        let chat_id = ChatId::new();

        provider.stage_turn_workspace(chat_id).await;

        let staged = scratch_root
            .path()
            .join(chat_id.to_string())
            .join(openwave_code_execution::SKILLS_DIR)
            .join("presentations")
            .join(openwave_code_execution::SKILL_MANIFEST_FILE);
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), manifest);

        // Idempotent: the first exec re-prepares the same tree.
        provider.stage_turn_workspace(chat_id).await;
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), manifest);

        // A skill the user drops in after configuration is picked up by the
        // next staging without a restart, and the catalog attributes it.
        let user_manifest = "---\nname: meeting-notes\ndescription: My way.\n---\nBody.\n";
        let user_skill = scratch_root
            .path()
            .join("user-skills")
            .join("meeting-notes");
        std::fs::create_dir_all(&user_skill).unwrap();
        std::fs::write(
            user_skill.join(openwave_code_execution::SKILL_MANIFEST_FILE),
            user_manifest,
        )
        .unwrap();
        provider.stage_turn_workspace(chat_id).await;
        let staged_user = scratch_root
            .path()
            .join(chat_id.to_string())
            .join(openwave_code_execution::SKILLS_DIR)
            .join("meeting-notes")
            .join(openwave_code_execution::SKILL_MANIFEST_FILE);
        assert_eq!(
            std::fs::read_to_string(&staged_user).unwrap(),
            user_manifest
        );
        assert_eq!(
            provider
                .skill_catalog()
                .iter()
                .map(|skill| (skill.name.as_str(), skill.origin))
                .collect::<Vec<_>>(),
            [
                ("meeting-notes", openwave_code_execution::SkillOrigin::User),
                (
                    "presentations",
                    openwave_code_execution::SkillOrigin::Builtin
                ),
            ]
        );

        // A skill-less configuration (headless embeddings) stages nothing.
        let bare =
            ConfiguredCodeExecutionProvider::new(store, Arc::new(NoSecrets), scratch_root.path());
        let bare_chat = ChatId::new();
        bare.stage_turn_workspace(bare_chat).await;
        assert!(!scratch_root.path().join(bare_chat.to_string()).exists());
    }

    /// Local exec is confined to the scratch directory but can create entries
    /// in it, including a symlink aimed at the host. Both preparation writes
    /// run on that directory before the next command, unsandboxed.    #[cfg(unix)]
    #[tokio::test]
    async fn preparation_does_not_write_through_a_planted_symlink() {
        let outside = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        for name in DOCUMENT_SCRIPT_FILES {
            std::fs::write(source.path().join(name), format!("helper:{name}")).unwrap();
        }
        let workspace = tempfile::tempdir().unwrap();

        let skill = openwave_code_execution::LoadedSkill {
            package: openwave_code_execution::SkillPackage {
                name: "pdf-documents".into(),
                description: "Produce PDFs.".into(),
                python_deps: Vec::new(),
                host_deps: Vec::new(),
                origin: openwave_code_execution::SkillOrigin::Builtin,
            },
            manifest: "---\nname: pdf-documents\n---\nbody".into(),
        };
        let skills = std::slice::from_ref(&skill);

        std::os::unix::fs::symlink(outside.path(), workspace.path().join("output")).unwrap();
        assert!(
            prepare_execution_directories(workspace.path(), true, Some(source.path()), skills)
                .await
                .is_err()
        );
        assert!(!outside.path().join(".openwave-directory").exists());

        std::fs::remove_file(workspace.path().join("output")).unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join(".openwave")).unwrap();
        assert!(
            prepare_execution_directories(workspace.path(), true, Some(source.path()), skills)
                .await
                .is_err()
        );
        assert!(!outside.path().join("exec-scripts").exists());
        assert!(!outside.path().join("skills").exists());
    }

    #[test]
    fn local_is_the_only_bounded_default() {
        let config = CodeExecutionConfig::default();
        assert_eq!(config.provider, Some(CodeExecutionProviderKind::Local));
        assert_eq!(config.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert!(config.validate().is_ok());
        assert!(CodeExecutionConfig {
            provider: Some(CodeExecutionProviderKind::Local),
            timeout_ms: MIN_TIMEOUT_MS - 1,
            egress: EgressConfig::Open,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn selection_contains_no_endpoint_or_credential_reference() {
        let json = serde_json::to_value(CodeExecutionConfig::default()).unwrap();
        assert_eq!(json["provider"], "local");
        assert!(json.get("endpoint").is_none());
        assert!(json.get("credential").is_none());
    }

    #[test]
    fn egress_defaults_to_open_and_compiles_no_policy() {
        let config = CodeExecutionConfig::default();
        assert_eq!(config.egress, EgressConfig::Open);
        // Open must leave the managed adapters on today's open-internet
        // creation: no policy is threaded into the create path.
        assert_eq!(config.egress.to_policy().unwrap(), None);

        // The egress config carries no secret or endpoint — only patterns.
        let json = serde_json::to_value(&config.egress).unwrap();
        assert_eq!(json, serde_json::json!({ "mode": "open" }));
    }

    #[test]
    fn egress_allowlist_compiles_to_a_deny_by_default_decision_policy() {
        let config = EgressConfig::Allowlist {
            domains: vec!["*.pypi.org".to_owned(), "crates.io".to_owned()],
            cidrs: vec!["140.82.112.0/20".to_owned()],
        };
        let Some(EgressPolicy::Allowlist(allowlist)) = config.to_policy().unwrap() else {
            panic!("a non-empty allowlist compiles to an allowlist policy");
        };
        assert_eq!(allowlist.domains().len(), 2);
        assert_eq!(allowlist.cidrs().len(), 1);

        // An empty allowlist is a deny-all policy, not open egress.
        let empty = EgressConfig::Allowlist {
            domains: vec![],
            cidrs: vec![],
        };
        let Some(EgressPolicy::Allowlist(empty_list)) = empty.to_policy().unwrap() else {
            panic!("an empty allowlist still compiles to a policy");
        };
        assert!(empty_list.is_empty());

        // A malformed grant fails closed at validation rather than widening.
        let bad = EgressConfig::Allowlist {
            domains: vec!["not a host".to_owned()],
            cidrs: vec![],
        };
        assert!(bad.to_policy().is_err());
        assert!(CodeExecutionConfig {
            provider: Some(CodeExecutionProviderKind::E2b),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            egress: bad,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn chat_network_policy_compiles_package_class_and_deny_all_for_managed_providers() {
        let off = network_egress_config(&NetworkPolicy::Off)
            .to_policy()
            .unwrap();
        let Some(EgressPolicy::Allowlist(off)) = off else {
            panic!("off must compile to an explicit deny-all policy");
        };
        assert!(off.is_empty());

        let packages = network_egress_config(&NetworkPolicy::PackageManagers)
            .to_policy()
            .unwrap();
        let Some(EgressPolicy::Allowlist(packages)) = packages else {
            panic!("package managers must compile to an allowlist");
        };
        assert_eq!(packages.domains().len(), PACKAGE_MANAGER_DOMAINS.len());
        assert!(packages
            .domains()
            .iter()
            .any(|domain| domain.to_string() == "pypi.org"));
        assert!(packages.cidrs().is_empty());

        assert_eq!(
            network_egress_config(&NetworkPolicy::Open)
                .to_policy()
                .unwrap(),
            None
        );
    }

    #[test]
    fn egress_enforcement_never_oversells_a_provider_past_its_model() {
        let status = egress_enforcement_status();
        let row = |provider| {
            status
                .iter()
                .find(|row| row.provider == provider)
                .unwrap_or_else(|| panic!("{provider} enforcement is disclosed"))
        };
        let e2b = row(CodeExecutionProviderKind::E2b);
        let daytona = row(CodeExecutionProviderKind::Daytona);

        // E2B is confirmed, but its own enforcement model says it is not a full
        // boundary — domain rules cover only HTTP/HTTPS and DNS stays open — so
        // the surface must report gaps, not a boundary. Reading it from the
        // model is what keeps the two from ever disagreeing.
        assert!(
            !E2BExecutionProvider::egress_enforcement().is_credential_boundary(),
            "the model itself does not treat E2B as a boundary"
        );
        assert_eq!(e2b.status, EgressEnforcementStatus::AppliedWithGaps);
        assert!(
            !e2b.gaps.is_empty(),
            "the ports/DNS holes that make E2B not-a-boundary must be surfaced"
        );

        // Daytona's per-sandbox policy is a strict, externally enforced
        // boundary — confirmed live in #888 — so the corrected model treats it
        // as a credential boundary with no phantom curated-service exceptions.
        assert!(
            DaytonaExecutionProvider::egress_enforcement().is_credential_boundary(),
            "the corrected Daytona model is a credential boundary"
        );
        assert!(
            daytona.gaps.is_empty(),
            "the phantom curated-service exceptions must be gone from the disclosure"
        );
        // But it stays honest about the one thing the host can't verify
        // statically: the per-sandbox override needs Daytona org tier 3+. So it
        // is a conditional boundary carrying that requirement inline, never an
        // unconditional green boundary.
        assert_eq!(
            daytona.status,
            EgressEnforcementStatus::ConditionalBoundary,
            "Daytona over-claims as an unconditional boundary"
        );
        assert_eq!(
            daytona.requirement.as_deref(),
            Some(DAYTONA_TIER_REQUIREMENT)
        );
        assert_ne!(
            daytona.status,
            EgressEnforcementStatus::Boundary,
            "the tier caveat must keep Daytona off the unconditional boundary status"
        );
    }

    #[test]
    fn resolve_glue_applies_the_configured_policy_to_the_managed_providers() {
        // The catastrophic-but-silent regression is the resolve path dropping
        // the policy — a configured allowlist reverting to open egress. These
        // assert the exact wiring resolve uses carries the policy through to the
        // provider; the adapter tests then prove that policy compiles into the
        // create body.
        let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
        let pool = RemoteSessionPool::default();
        let allowlist = EgressConfig::Allowlist {
            domains: vec!["*.pypi.org".to_owned()],
            cidrs: vec!["140.82.112.0/20".to_owned()],
        };
        let expected = allowlist.to_policy().unwrap().unwrap();

        let e2b = configured_e2b(
            E2BCredential::parse("test-e2b-key").unwrap(),
            timeout,
            pool.clone(),
            &allowlist,
        )
        .unwrap();
        assert_eq!(e2b.egress_policy(), Some(&expected));

        let daytona = configured_daytona(
            DaytonaCredential::parse("test-daytona-key").unwrap(),
            timeout,
            pool.clone(),
            &allowlist,
        )
        .unwrap();
        assert_eq!(daytona.egress_policy(), Some(&expected));

        // Open leaves both providers on today's open-internet creation: no
        // policy is threaded in.
        let open_e2b = configured_e2b(
            E2BCredential::parse("test-e2b-key").unwrap(),
            timeout,
            pool.clone(),
            &EgressConfig::Open,
        )
        .unwrap();
        assert_eq!(open_e2b.egress_policy(), None);
        let open_daytona = configured_daytona(
            DaytonaCredential::parse("test-daytona-key").unwrap(),
            timeout,
            pool,
            &EgressConfig::Open,
        )
        .unwrap();
        assert_eq!(open_daytona.egress_policy(), None);
    }

    #[tokio::test]
    async fn configuration_can_disable_and_reenable_local_execution() {
        let (store, _dir) = test_store().await;
        let host_config = openwave_core::Config::desktop(_dir.path());
        let secrets = NoSecrets;
        let disabled = update_config(
            &host_config,
            &store,
            &secrets,
            CodeExecutionConfigUpdate {
                provider: Some(None),
                timeout_ms: Some(MIN_TIMEOUT_MS),
                egress: None,
            },
        )
        .await;
        let disabled = match disabled {
            Ok(info) => info,
            Err(_) => panic!("valid disabled code-execution configuration was rejected"),
        };
        assert_eq!(disabled.provider, None);
        assert!(!disabled.available);

        let local = update_config(
            &host_config,
            &store,
            &secrets,
            CodeExecutionConfigUpdate {
                provider: Some(Some(CodeExecutionProviderKind::Local)),
                timeout_ms: Some(MAX_TIMEOUT_MS),
                egress: None,
            },
        )
        .await;
        let local = match local {
            Ok(info) => info,
            Err(_) => panic!("valid local code-execution configuration was rejected"),
        };
        assert_eq!(local.provider, Some(CodeExecutionProviderKind::Local));
        assert_eq!(local.timeout_ms, MAX_TIMEOUT_MS);
    }

    #[tokio::test]
    async fn workspace_capability_degrades_to_none_instead_of_failing() {
        let (store, dir) = test_store().await;
        let provider = ConfiguredCodeExecutionProvider::new(
            Arc::new(store),
            Arc::new(NoSecrets),
            dir.path().join("scratch"),
        );
        assert!(provider.workspace().await.unwrap().is_some());

        // Disabling execution and selecting a managed provider without a
        // credential must both report "no workspace", not an error.
        provider
            .store
            .set_setting(
                CODE_EXECUTION_SETTING,
                &serde_json::json!({ "provider": null, "timeout_ms": DEFAULT_TIMEOUT_MS }),
            )
            .await
            .unwrap();
        assert!(provider.workspace().await.unwrap().is_none());

        provider
            .store
            .set_setting(
                CODE_EXECUTION_SETTING,
                &serde_json::json!({ "provider": "e2b", "timeout_ms": DEFAULT_TIMEOUT_MS }),
            )
            .await
            .unwrap();
        assert!(provider.workspace().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn invalid_persisted_policy_fails_closed() {
        let (store, _dir) = test_store().await;
        store
            .set_setting(
                CODE_EXECUTION_SETTING,
                &serde_json::json!({
                    "provider": "local",
                    "timeout_ms": MAX_TIMEOUT_MS + 1,
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            read_config(&store).await.unwrap(),
            CodeExecutionConfig::disabled()
        );
    }
}
