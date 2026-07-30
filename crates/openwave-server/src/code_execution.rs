//! Host-owned code-execution provider selection and policy.
//!
//! The model cannot select a provider or timeout. The foreground `exec` tool
//! calls [`ConfiguredCodeExecutionProvider`], which reads the current host
//! setting at the last possible boundary and delegates to the selected adapter.
//! Local and managed adapters implement the same provider contract without
//! changing the tool schema or persisted tool-call arguments.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openwave_code_execution::{
    sync, CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind,
    CodeExecutionRequest, CodeExecutionResponse, DaytonaCredential, DaytonaExecutionProvider,
    E2BCredential, E2BExecutionProvider, ExecFolderAccess, ExecFolderGrant, ExecutionWorkspaceId,
    LocalExecutionProvider, PreviewScan, RemoteSessionPool, WorkspaceFilePath, WorkspaceLifecycle,
    WorkspaceListing, DAYTONA_CREDENTIAL_KEY, DOCUMENT_SCRIPTS_DIR, DOCUMENT_SCRIPT_FILES,
    E2B_CREDENTIAL_KEY,
};
use openwave_core::{Chat, ChatId, HostRootId, ProjectId, Result, SecretProvider, Store};
use openwave_egress::{
    CidrBlock, DomainPattern, EgressAllowlist, EgressEnforcement, EgressError, EgressPolicy,
};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;

const CODE_EXECUTION_SETTING: &str = "code_execution";
pub const DEFAULT_TIMEOUT_MS: u64 = 20_000;
pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;

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
/// sandbox denies network and outside-workspace writes. `None` explicitly
/// removes execution from service without changing the stable tool contract.
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
    config_info(store, secrets).await.map_err(Into::into)
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
    scratch_root: PathBuf,
    document_scripts_source: Option<PathBuf>,
    folder_grant_resolver: Option<Arc<dyn ExecFolderGrantResolver>>,
    remote_sessions: RemoteSessionPool,
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
            scratch_root: scratch_root.into(),
            document_scripts_source: None,
            folder_grant_resolver: None,
            remote_sessions: RemoteSessionPool::default(),
        }
    }

    /// Install a trusted bundled helper directory into every exec workspace.
    #[must_use]
    pub fn with_document_scripts(mut self, source: Option<PathBuf>) -> Self {
        self.document_scripts_source = source;
        self
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
    ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, CodeExecutionError> {
        let config = read_config(&*self.store).await.map_err(|_| {
            CodeExecutionError::Unavailable("configuration storage is unavailable".into())
        })?;
        if config.provider != Some(CodeExecutionProviderKind::Local) || !cfg!(target_os = "macos") {
            return Ok(Vec::new());
        }
        self.resolve_chat_folder_grants(chat).await
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

    /// Resolve the currently selected adapter at the last boundary before use.
    async fn resolve(
        &self,
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
            CodeExecutionProviderKind::Local => Box::new(
                LocalExecutionProvider::new(
                    &self.scratch_root,
                    Duration::from_millis(config.timeout_ms),
                )?
                .with_document_scripts(self.document_scripts_source.clone()),
            ),
            CodeExecutionProviderKind::E2b => {
                let credential = E2BCredential::load(&*self.secrets)
                    .await?
                    .ok_or(CodeExecutionError::NotConfigured)?;
                Box::new(configured_e2b(
                    credential,
                    Duration::from_millis(config.timeout_ms),
                    self.remote_sessions.clone(),
                    &config.egress,
                )?)
            }
            CodeExecutionProviderKind::Daytona => {
                let credential = DaytonaCredential::load(&*self.secrets)
                    .await?
                    .ok_or(CodeExecutionError::NotConfigured)?;
                Box::new(configured_daytona(
                    credential,
                    Duration::from_millis(config.timeout_ms),
                    self.remote_sessions.clone(),
                    &config.egress,
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
        let provider = match self.resolve().await {
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
        let (kind, provider) = self.resolve().await?;
        if kind == CodeExecutionProviderKind::Local && cfg!(target_os = "macos") {
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
                    CodeExecutionError::InvalidRequest(
                        "execution conversation does not exist".into(),
                    )
                })?;
            let grants = self
                .resolve_chat_folder_grants(&chat)
                .await?
                .into_iter()
                .map(|grant| {
                    ExecFolderGrant::new(
                        grant.path,
                        if grant.writable {
                            ExecFolderAccess::ReadWrite
                        } else {
                            ExecFolderAccess::ReadOnly
                        },
                    )
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            request = request.with_folder_grants(grants)?;
        }
        let host_dir = self.scratch_root.join(request.workspace_id.as_str());
        prepare_execution_directories(
            &host_dir,
            kind != CodeExecutionProviderKind::Local,
            self.document_scripts_source.as_deref(),
        )
        .await?;
        // A remote sandbox has its own filesystem, but the model is shown one
        // path vocabulary across the file tools and exec. Mirror the chat's
        // private scratch into the workspace before the command and back out
        // after it, so a file written by either side is visible to the other.
        // The local provider already runs inside scratch; mirroring there
        // would only copy the directory onto itself.
        let lifecycle = match kind {
            CodeExecutionProviderKind::Local => None,
            _ => provider.workspace_lifecycle(),
        };
        let Some(lifecycle) = lifecycle else {
            return provider.execute(request).await;
        };
        // A failed push fails the execution: running against files the model
        // believes are present would answer with misleading not-found errors.
        let mut notes = sync::push_host_dir(lifecycle, &request.workspace_id, &host_dir)
            .await?
            .notes;
        let mut response = provider.execute(request.clone()).await?;
        // A failed pull keeps the execution's output — the command did run —
        // and says the host copies are stale instead of failing the call.
        match sync::pull_into_host_dir(lifecycle, &request.workspace_id, &host_dir).await {
            Ok(pulled) => notes.extend(pulled.notes),
            Err(error) => notes.push(format!(
                "workspace files were not copied back to private scratch: {error}"
            )),
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

    // `workspace_lifecycle` stays `None` here on purpose: the capability of
    // this late-binding wrapper depends on the configuration read at call
    // time, which the synchronous trait flag cannot express. Host callers use
    // [`ConfiguredCodeExecutionProvider::workspace`] instead.
}

async fn prepare_execution_directories(
    host_dir: &std::path::Path,
    mirrored: bool,
    document_scripts_source: Option<&std::path::Path>,
) -> std::result::Result<(), CodeExecutionError> {
    for name in ["output", "preview", "documents"] {
        let directory = host_dir.join(name);
        tokio::fs::create_dir_all(&directory).await.map_err(|_| {
            CodeExecutionError::Sandbox(format!(
                "private workspace directory '{name}/' is unavailable"
            ))
        })?;
        if mirrored {
            // The sync protocol mirrors files rather than empty directories.
            // A hidden zero-byte marker makes the conventional directories
            // exist in managed workspaces without becoming a user artifact.
            tokio::fs::write(directory.join(".openwave-directory"), [])
                .await
                .map_err(|_| {
                    CodeExecutionError::Sandbox(format!(
                        "private workspace directory '{name}/' is unavailable"
                    ))
                })?;
        }
    }
    if let Some(source) = document_scripts_source {
        install_document_scripts(source, host_dir).await?;
    }
    Ok(())
}

async fn install_document_scripts(
    source: &std::path::Path,
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    let destination = host_dir.join(DOCUMENT_SCRIPTS_DIR);
    tokio::fs::create_dir_all(&destination).await.map_err(|_| {
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
        tokio::fs::write(destination.join(name), content)
            .await
            .map_err(|_| {
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
        AgentError, ChatRootAttachment, DbStore, PermissionMode, RootAttachmentOrigin,
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

    #[tokio::test]
    async fn exec_workspace_conventions_exist_before_a_command_runs() {
        let dir = tempfile::tempdir().unwrap();
        prepare_execution_directories(dir.path(), true, None)
            .await
            .unwrap();

        for name in ["output", "preview", "documents"] {
            assert!(dir.path().join(name).is_dir());
            assert!(dir.path().join(name).join(".openwave-directory").is_file());
        }
    }

    #[tokio::test]
    async fn bundled_document_helpers_are_installed_as_one_library() {
        let source = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        for name in DOCUMENT_SCRIPT_FILES {
            std::fs::write(source.path().join(name), format!("helper:{name}")).unwrap();
        }

        prepare_execution_directories(workspace.path(), false, Some(source.path()))
            .await
            .unwrap();

        for name in DOCUMENT_SCRIPT_FILES {
            let installed = workspace.path().join(DOCUMENT_SCRIPTS_DIR).join(name);
            assert_eq!(
                std::fs::read_to_string(installed).unwrap(),
                format!("helper:{name}")
            );
        }
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
        let secrets = NoSecrets;
        let disabled = update_config(
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
