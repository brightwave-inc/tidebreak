use std::collections::HashMap;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tidebreak_core::{ExecDegradation, SecretProvider};
use tidebreak_egress::{
    CidrBlock, EgressEnforcement, EgressPolicy, EnforcementException, ExceptionReach,
    ExceptionScope,
};

use crate::credential::SecretCredential;
use crate::http::{decode_bounded_json, download_bounded_file, multipart_file};
use crate::output::{Capture, StreamKind};
use crate::remote::{
    connect_remote_workspace, create_remote_workspace, destroy_remote_workspace, execute_remote,
    stage_remote_file, with_remote_session, RemoteSandboxAdapter, RemoteSession,
    RemoteSessionError, RemoteSessionPool, RemoteWorkspaceAdapter,
};
use crate::{
    ExecError, ExecProvider, ExecProviderKind, ExecRequest, ExecResponse, ExecutionWorkspaceId,
    StagedUpload, WorkspaceFileEntry, WorkspaceFilePath, WorkspaceLifecycle, WorkspaceListing,
    MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
};

const E2B_API_BASE: &str = "https://api.e2b.app";
const E2B_SANDBOX_BASE: &str = "https://sandbox.e2b.app";
/// The published Tidebreak documents template every sandbox is created from.
///
/// E2B provisions from account-registered *templates*, not arbitrary OCI refs,
/// so the image reaches E2B as a template published from the Tidebreak account.
/// Publishing makes it creatable from any E2B account — but only by this
/// opaque template *ID*. E2B resolves a custom template's human alias inside
/// the owning team alone; only E2B's own base templates, like
/// `code-interpreter-v1`, resolve by alias from any account (verified against
/// api.e2b.app, 2026-08-04). Pinning the alias here is what silently dropped
/// every account but Tidebreak's own to the fallback image.
///
/// The ID is what the published, version-suffixed alias — kept on its own
/// line so the publish workflow can rewrite both together —
/// `tidebreak-documents-v0-26-0`
/// resolves to. It is currently built from
/// `ghcr.io/brightwave-inc/tidebreak-sandbox-agent-documents:v0.26.0`
/// (`sha256:dd22da7a3c5b1f315e888da902e7a46ae034585e2ab5c09c0ae4588a69f158a2`),
/// the same ref recorded in `crates/tidebreak-sandbox-agent/e2b/e2b.Dockerfile`.
/// Publishing a new image version publishes a new alias with a new ID, and the
/// publish workflow's pin PR moves this constant with it — that directory's
/// README has the procedure.
const E2B_TEMPLATE: &str = "yarxjy39quzg6wm78u1a";

/// E2B's own public code-interpreter template, used only when the Tidebreak
/// template cannot be resolved. Degraded but working: document skills fall back
/// to installing their Python dependencies inside the sandbox at run time.
const E2B_FALLBACK_TEMPLATE: &str = "code-interpreter-v1";
const E2B_WORKSPACE_ROOT: &str = "/home/user";
const E2B_USER: &str = "user";
const E2B_ENVD_PORT: &str = "49983";
const E2B_SANDBOX_TTL_SECONDS: u64 = 300;
const E2B_TRANSPORT_GRACE: Duration = Duration::from_secs(10);
const MAX_MANAGEMENT_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CONNECT_FRAME_BYTES: usize = 256 * 1024;
const CONNECT_END_STREAM_FLAG: u8 = 0b0000_0010;
const CONNECT_COMPRESSED_FLAG: u8 = 0b0000_0001;

/// Fixed key for E2B credentials in Tidebreak's host secret store.
pub const E2B_CREDENTIAL_KEY: &str = "code_execution.e2b.api_key";

/// Non-serializable, redacted E2B API credential.
#[derive(Clone)]
pub struct E2BCredential(SecretCredential);

impl std::fmt::Debug for E2BCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("E2BCredential")
            .field(&"***")
            .finish()
    }
}

impl E2BCredential {
    pub fn parse(value: impl Into<String>) -> Result<Self, ExecError> {
        SecretCredential::parse("E2B", value).map(Self)
    }

    /// Resolve the credential without exposing it through serializable config.
    pub async fn load(secrets: &dyn SecretProvider) -> Result<Option<Self>, ExecError> {
        SecretCredential::load(secrets, E2B_CREDENTIAL_KEY, "E2B")
            .await
            .map(|credential| credential.map(Self))
    }

    fn as_str(&self) -> &str {
        self.0.expose()
    }

    fn fingerprint(&self) -> [u8; 32] {
        self.0.fingerprint()
    }
}

/// Direct-command adapter for E2B's managed sandbox service.
pub struct E2BExecutionProvider {
    credential: E2BCredential,
    timeout: Duration,
    pool: RemoteSessionPool,
    client: Client,
    endpoints: E2BEndpoints,
    egress: Option<EgressPolicy>,
    template: String,
    /// Whether `template` is the built-in default rather than a caller's
    /// override. Only the default may fall back.
    default_template: bool,
    /// Latched once E2B reports the default template unresolvable, so the
    /// remaining sandbox creations of this provider's life skip the doomed
    /// first attempt.
    template_unavailable: AtomicBool,
    /// Set when `template_unavailable` latches, and taken by the execution that
    /// observes it. The latch means one provider instance degrades once, so the
    /// report goes out once too.
    degradation_pending: AtomicBool,
}

#[derive(Clone)]
struct E2BEndpoints {
    api_base: String,
    sandbox_base: String,
}

impl Default for E2BEndpoints {
    fn default() -> Self {
        Self {
            api_base: E2B_API_BASE.into(),
            sandbox_base: E2B_SANDBOX_BASE.into(),
        }
    }
}

impl E2BExecutionProvider {
    pub fn new(credential: E2BCredential, timeout: Duration) -> Result<Self, ExecError> {
        Self::with_session_pool(credential, timeout, RemoteSessionPool::default())
    }

    pub fn with_session_pool(
        credential: E2BCredential,
        timeout: Duration,
        pool: RemoteSessionPool,
    ) -> Result<Self, ExecError> {
        Self::with_endpoints(credential, timeout, pool, E2BEndpoints::default())
    }

    fn with_endpoints(
        credential: E2BCredential,
        timeout: Duration,
        pool: RemoteSessionPool,
        endpoints: E2BEndpoints,
    ) -> Result<Self, ExecError> {
        if timeout.is_zero() {
            return Err(ExecError::InvalidRequest(
                "execution timeout must be positive".into(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Connect's deadline remains the command timeout. This outer bound
            // gives envd a short window to return the terminal trailer while
            // preventing a stalled HTTP response from holding the tool forever.
            .timeout(timeout.saturating_add(E2B_TRANSPORT_GRACE))
            .build()
            .map_err(|_| ExecError::Unavailable("could not configure E2B transport".into()))?;
        Ok(Self {
            credential,
            timeout,
            pool,
            client,
            endpoints,
            egress: None,
            template: E2B_TEMPLATE.into(),
            default_template: true,
            template_unavailable: AtomicBool::new(false),
            degradation_pending: AtomicBool::new(false),
        })
    }

    /// Create every sandbox from this E2B template instead of the published
    /// Tidebreak documents template.
    ///
    /// This is an escape hatch for an account that maintains its own template.
    /// Unlike the default it never falls back: a template the caller named and
    /// E2B cannot resolve is an error they need to see, not something to paper
    /// over with different sandbox contents.
    #[must_use]
    pub fn with_template(mut self, template: impl Into<String>) -> Self {
        self.template = template.into();
        self.default_template = false;
        self
    }

    /// Compile an egress policy into every sandbox this provider creates.
    ///
    /// Without a policy the provider keeps today's disclosed open-egress
    /// creation. With one, sandboxes are created deny-by-default and the
    /// allowlist becomes E2B's per-sandbox `allowOut` rules — subject to the
    /// enforcement holes declared by [`Self::egress_enforcement`].
    #[must_use]
    pub fn with_egress_policy(mut self, policy: EgressPolicy) -> Self {
        self.egress = Some(policy);
        self
    }

    /// The egress policy compiled into this provider's sandboxes, or `None` for
    /// today's open-internet creation. Exposed so the host wiring that selects
    /// and applies a policy can be verified without a live API — a dropped
    /// policy in that path reverts a configured allowlist to open egress, and
    /// this is what a test asserts against.
    #[doc(hidden)]
    #[must_use]
    pub fn egress_policy(&self) -> Option<&EgressPolicy> {
        self.egress.as_ref()
    }

    /// Host knowledge about E2B's per-sandbox network enforcement, declared
    /// as what it actually blocks. Vendor exceptions: DNS to `8.8.8.8` stays
    /// open regardless of policy, and domain-pattern rules are enforced only
    /// on ports 80 and 443, so a name denied only by a domain rule may stay
    /// reachable on other ports.
    #[must_use]
    pub fn egress_enforcement() -> EgressEnforcement {
        EgressEnforcement::external(vec![
            EnforcementException {
                scope: ExceptionScope::Address(
                    CidrBlock::parse("8.8.8.8/32").expect("static exception block parses"),
                ),
                reach: ExceptionReach::Narrow,
                purpose: "DNS resolution",
            },
            EnforcementException {
                scope: ExceptionScope::DomainRulePortLimit(vec![80, 443]),
                reach: ExceptionReach::GeneralPurpose,
                purpose: "domain filtering covers HTTP and HTTPS ports only",
            },
        ])
    }

    async fn create_sandbox(&self, workspace_id: &str) -> Result<RemoteSession, ExecError> {
        if !self.default_template || self.template_unavailable.load(Ordering::Relaxed) {
            let template = if self.default_template {
                E2B_FALLBACK_TEMPLATE
            } else {
                self.template.as_str()
            };
            return self
                .create_sandbox_from(workspace_id, template)
                .await
                .map_err(|failure| failure.into_error(template));
        }

        match self.create_sandbox_from(workspace_id, &self.template).await {
            Ok(session) => Ok(session),
            Err(CreateSandboxFailure::TemplateMissing) => {
                // The Tidebreak template is published from one account and used
                // by all of them, so it can be absent here for reasons the user
                // cannot fix: the publish has not happened yet on a build that
                // shipped ahead of it, or it was withdrawn. Rather than leave
                // code execution dead, run the same skills on E2B's public
                // template and pay the in-sandbox install.
                tracing::warn!(
                    "E2B could not resolve the Tidebreak template '{}'; falling back to '{E2B_FALLBACK_TEMPLATE}' \
                     for the rest of this session. Document skills will install their Python \
                     dependencies inside the sandbox at run time.",
                    self.template
                );
                self.template_unavailable.store(true, Ordering::Relaxed);
                self.degradation_pending.store(true, Ordering::Relaxed);
                self.create_sandbox_from(workspace_id, E2B_FALLBACK_TEMPLATE)
                    .await
                    .map_err(|failure| failure.into_error(E2B_FALLBACK_TEMPLATE))
            }
            Err(failure) => Err(failure.into_error(&self.template)),
        }
    }

    async fn create_sandbox_from(
        &self,
        workspace_id: &str,
        template: &str,
    ) -> Result<RemoteSession, CreateSandboxFailure> {
        let url = format!(
            "{}/sandboxes",
            self.endpoints.api_base.trim_end_matches('/')
        );
        let network = e2b_network_settings(self.egress.as_ref());
        let response = self
            .client
            .post(url)
            .header("X-API-Key", self.credential.as_str())
            .json(&CreateSandboxRequest {
                template_id: template,
                timeout: E2B_SANDBOX_TTL_SECONDS,
                secure: true,
                allow_internet_access: network.allow_internet_access,
                network: network.network,
                metadata: HashMap::from([("tidebreak_workspace_id", workspace_id)]),
            })
            .send()
            .await
            .map_err(|_| {
                CreateSandboxFailure::Other(ExecError::Unavailable(
                    "could not reach the E2B API".into(),
                ))
            })?;
        // Template resolution is the only thing creation looks up by name, and
        // E2B answers an unresolvable one — unknown alias, or a template the
        // key's team cannot see — with 404 and a `template '<id>' not found`
        // body. Every other failure (401/403 credential, 429 quota, 5xx) keeps
        // its own meaning.
        if response.status() == StatusCode::NOT_FOUND {
            return Err(CreateSandboxFailure::TemplateMissing);
        }
        decode_session(response)
            .await
            .map_err(CreateSandboxFailure::Other)
    }

    async fn connect_sandbox(&self, sandbox_id: &str) -> Result<Option<RemoteSession>, ExecError> {
        validate_sandbox_id(sandbox_id)?;
        let url = format!(
            "{}/sandboxes/{sandbox_id}/connect",
            self.endpoints.api_base.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .header("X-API-Key", self.credential.as_str())
            .json(&ConnectSandboxRequest {
                timeout: E2B_SANDBOX_TTL_SECONDS,
            })
            .send()
            .await
            .map_err(|_| ExecError::Unavailable("could not reach the E2B API".into()))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        decode_session(response).await.map(Some)
    }

    async fn run_e2b_command(
        &self,
        session: &RemoteSession,
        request: &ExecRequest,
    ) -> Result<ExecResponse, RemoteSessionError> {
        validate_sandbox_id(&session.sandbox_id)?;
        let access_token = session
            .access_token
            .as_deref()
            .ok_or_else(|| ExecError::Unavailable("E2B sandbox token is unavailable".into()))?;
        let started = Instant::now();
        let request_json = serde_json::to_vec(&StartRequest {
            process: ProcessConfig {
                cmd: &request.command,
                args: &request.arguments,
                envs: HashMap::new(),
                cwd: remote_cwd(&request.cwd)?,
            },
            stdin: false,
        })
        .map_err(|_| ExecError::InvalidRequest("E2B request is not serializable".into()))?;
        let body = connect_envelope(0, &request_json)?;
        let timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX);
        let url = format!(
            "{}/process.Process/Start",
            self.endpoints.sandbox_base.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/connect+json")
            .header("Connect-Protocol-Version", "1")
            .header("Connect-Timeout-Ms", timeout_ms)
            .header("E2b-Sandbox-Id", &session.sandbox_id)
            .header("E2b-Sandbox-Port", E2B_ENVD_PORT)
            .header("X-Access-Token", access_token)
            .body(body)
            .send()
            .await
            .map_err(|_| ExecError::AmbiguousExecution)?;
        if response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::BAD_GATEWAY
        {
            return Err(RemoteSessionError::Missing);
        }
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()).into());
        }

        decode_command_response(response, started)
            .await
            .map_err(RemoteSessionError::Provider)
    }

    fn envd_request(
        &self,
        builder: reqwest::RequestBuilder,
        session: &RemoteSession,
    ) -> Result<reqwest::RequestBuilder, ExecError> {
        let access_token = session
            .access_token
            .as_deref()
            .ok_or_else(|| ExecError::Unavailable("E2B sandbox token is unavailable".into()))?;
        Ok(builder
            .header("E2b-Sandbox-Id", &session.sandbox_id)
            .header("E2b-Sandbox-Port", E2B_ENVD_PORT)
            .header("X-Access-Token", access_token))
    }

    fn sandbox_url(&self, path: &str) -> String {
        format!(
            "{}{path}",
            self.endpoints.sandbox_base.trim_end_matches('/')
        )
    }
}

#[async_trait]
impl RemoteSandboxAdapter for E2BExecutionProvider {
    fn kind(&self) -> ExecProviderKind {
        ExecProviderKind::E2b
    }

    fn credential_fingerprint(&self) -> [u8; 32] {
        self.credential.fingerprint()
    }

    fn egress_fingerprint(&self) -> [u8; 32] {
        crate::remote::egress_policy_fingerprint(self.egress.as_ref())
    }

    async fn create_session(&self, workspace_id: &str) -> Result<RemoteSession, ExecError> {
        self.create_sandbox(workspace_id).await
    }

    async fn destroy_sandbox(&self, session: &RemoteSession) -> Result<(), ExecError> {
        validate_sandbox_id(&session.sandbox_id)?;
        let url = format!(
            "{}/sandboxes/{}",
            self.endpoints.api_base.trim_end_matches('/'),
            session.sandbox_id
        );
        let response = self
            .client
            .delete(url)
            .header("X-API-Key", self.credential.as_str())
            .send()
            .await
            .map_err(|_| ExecError::Unavailable("could not reach the E2B API".into()))?;
        if response.status() == StatusCode::NOT_FOUND || response.status().is_success() {
            return Ok(());
        }
        Err(provider_status_error(response.status()))
    }

    async fn reconnect_session(
        &self,
        session: &RemoteSession,
    ) -> Result<Option<RemoteSession>, ExecError> {
        self.connect_sandbox(&session.sandbox_id).await
    }

    async fn run_command(
        &self,
        session: &RemoteSession,
        request: &ExecRequest,
    ) -> Result<ExecResponse, RemoteSessionError> {
        self.run_e2b_command(session, request).await
    }
}

#[async_trait]
impl RemoteWorkspaceAdapter for E2BExecutionProvider {
    async fn upload_file(
        &self,
        session: &RemoteSession,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), RemoteSessionError> {
        validate_sandbox_id(&session.sandbox_id)?;
        let multipart = multipart_file(path.file_name(), content);
        let response = self
            .envd_request(self.client.post(self.sandbox_url("/files")), session)?
            .query(&[
                ("path", remote_file_path(Some(path)).as_str()),
                ("username", E2B_USER),
            ])
            .header("Content-Type", multipart.content_type)
            .body(multipart.body)
            .send()
            .await
            .map_err(|_| ExecError::Unavailable("could not reach the E2B sandbox".into()))?;
        if response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::BAD_GATEWAY
        {
            return Err(RemoteSessionError::Missing);
        }
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()).into());
        }
        Ok(())
    }

    async fn download_file(
        &self,
        session: &RemoteSession,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, RemoteSessionError> {
        validate_sandbox_id(&session.sandbox_id)?;
        let response = self
            .envd_request(self.client.get(self.sandbox_url("/files")), session)?
            .query(&[
                ("path", remote_file_path(Some(path)).as_str()),
                ("username", E2B_USER),
            ])
            .send()
            .await
            .map_err(|_| ExecError::Unavailable("could not reach the E2B sandbox".into()))?;
        // The session was reconciled immediately before this call, so a 404
        // here is the file, while a routing failure surfaces as 502.
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ExecError::WorkspaceFileNotFound.into());
        }
        if response.status() == StatusCode::BAD_GATEWAY {
            return Err(RemoteSessionError::Missing);
        }
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()).into());
        }
        download_bounded_file(response, "E2B", MAX_WORKSPACE_FILE_BYTES)
            .await
            .map_err(RemoteSessionError::Provider)
    }

    async fn list_directory(
        &self,
        session: &RemoteSession,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, RemoteSessionError> {
        validate_sandbox_id(&session.sandbox_id)?;
        let response = self
            .envd_request(
                self.client
                    .post(self.sandbox_url("/filesystem.Filesystem/ListDir")),
                session,
            )?
            .header("Connect-Protocol-Version", "1")
            .json(&ListDirRequest {
                path: remote_file_path(path),
                depth: 1,
            })
            .send()
            .await
            .map_err(|_| ExecError::Unavailable("could not reach the E2B sandbox".into()))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ExecError::WorkspaceFileNotFound.into());
        }
        if response.status() == StatusCode::BAD_GATEWAY {
            return Err(RemoteSessionError::Missing);
        }
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()).into());
        }
        let body =
            decode_bounded_json::<ListDirResponse>(response, "E2B", MAX_MANAGEMENT_RESPONSE_BYTES)
                .await
                .map_err(RemoteSessionError::Provider)?;
        let mut entries = Vec::new();
        for entry in body.entries {
            if entry.name.is_empty() {
                continue;
            }
            let relative = entry
                .path
                .as_deref()
                .and_then(|absolute| {
                    absolute
                        .strip_prefix(E2B_WORKSPACE_ROOT)
                        .map(|rest| rest.trim_start_matches('/').to_owned())
                })
                .filter(|relative| !relative.is_empty())
                .unwrap_or_else(|| match path {
                    None => entry.name.clone(),
                    Some(path) => format!("{}/{}", path.as_str(), entry.name),
                });
            entries.push(WorkspaceFileEntry {
                path: relative,
                directory: entry.kind.as_deref() == Some("FILE_TYPE_DIRECTORY"),
                size_bytes: entry.size.as_ref().and_then(json_u64),
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let truncated = entries.len() > MAX_WORKSPACE_LIST_ENTRIES;
        entries.truncate(MAX_WORKSPACE_LIST_ENTRIES);
        Ok(WorkspaceListing { entries, truncated })
    }
}

#[async_trait]
impl WorkspaceLifecycle for E2BExecutionProvider {
    async fn create_workspace(&self, workspace: &ExecutionWorkspaceId) -> Result<(), ExecError> {
        create_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn connect_workspace(&self, workspace: &ExecutionWorkspaceId) -> Result<bool, ExecError> {
        connect_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn destroy_workspace(&self, workspace: &ExecutionWorkspaceId) -> Result<(), ExecError> {
        destroy_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), ExecError> {
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(ExecError::WorkspaceFileTooLarge);
        }
        with_remote_session(
            self,
            &self.pool,
            workspace.as_str(),
            |adapter, session| async move { adapter.upload_file(&session, path, content).await },
        )
        .await
    }

    async fn stage_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<StagedUpload, ExecError> {
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(ExecError::WorkspaceFileTooLarge);
        }
        stage_remote_file(self, &self.pool, workspace.as_str(), path, content).await
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, ExecError> {
        with_remote_session(
            self,
            &self.pool,
            workspace.as_str(),
            |adapter, session| async move { adapter.download_file(&session, path).await },
        )
        .await
    }

    async fn list_workspace_files(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, ExecError> {
        with_remote_session(
            self,
            &self.pool,
            workspace.as_str(),
            |adapter, session| async move { adapter.list_directory(&session, path).await },
        )
        .await
    }
}

#[async_trait]
impl ExecProvider for E2BExecutionProvider {
    async fn execute(&self, request: ExecRequest) -> Result<ExecResponse, ExecError> {
        let mut response = execute_remote(self, &self.pool, request).await?;
        if self.degradation_pending.swap(false, Ordering::Relaxed) {
            response.degraded = Some(ExecDegradation::SandboxImageUnavailable);
        }
        Ok(response)
    }

    fn workspace_lifecycle(&self) -> Option<&dyn WorkspaceLifecycle> {
        Some(self)
    }
}

/// Why a single sandbox-creation attempt failed. Split out from
/// [`ExecError`] because only one shape — E2B failing to resolve the
/// template — may be retried against a different template.
enum CreateSandboxFailure {
    TemplateMissing,
    Other(ExecError),
}

impl CreateSandboxFailure {
    fn into_error(self, template: &str) -> ExecError {
        match self {
            Self::TemplateMissing => ExecError::Unavailable(format!(
                "E2B could not find the sandbox template '{template}'"
            )),
            Self::Other(error) => error,
        }
    }
}

#[derive(Serialize)]
struct CreateSandboxRequest<'a> {
    #[serde(rename = "templateID")]
    template_id: &'a str,
    timeout: u64,
    secure: bool,
    allow_internet_access: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<E2BNetworkConfig>,
    metadata: HashMap<&'static str, &'a str>,
}

#[derive(Serialize)]
struct E2BNetworkConfig {
    #[serde(rename = "allowOut")]
    allow_out: Vec<String>,
}

struct E2BNetworkSettings {
    allow_internet_access: bool,
    network: Option<E2BNetworkConfig>,
}

/// Compile the host policy into E2B's creation-time network controls. No
/// policy keeps today's disclosed open-egress sandbox; any policy creates the
/// sandbox deny-by-default, with allowlist entries (wildcard domains and CIDR
/// blocks) as `allowOut` holes through the blocked default.
fn e2b_network_settings(policy: Option<&EgressPolicy>) -> E2BNetworkSettings {
    let Some(policy) = policy else {
        return E2BNetworkSettings {
            allow_internet_access: true,
            network: None,
        };
    };
    let network = match policy {
        EgressPolicy::BlockAll => None,
        EgressPolicy::Allowlist(allowlist) => {
            let allow_out: Vec<String> = allowlist
                .domains()
                .iter()
                .map(ToString::to_string)
                .chain(allowlist.cidrs().iter().map(ToString::to_string))
                .collect();
            (!allow_out.is_empty()).then_some(E2BNetworkConfig { allow_out })
        }
    };
    E2BNetworkSettings {
        allow_internet_access: false,
        network,
    }
}

#[derive(Serialize)]
struct ConnectSandboxRequest {
    timeout: u64,
}

#[derive(Deserialize)]
struct SandboxResponse {
    #[serde(rename = "sandboxID")]
    sandbox_id: String,
    #[serde(rename = "envdAccessToken")]
    access_token: Option<String>,
}

async fn decode_session(response: Response) -> Result<RemoteSession, ExecError> {
    if !response.status().is_success() {
        return Err(provider_status_error(response.status()));
    }
    let body =
        decode_bounded_json::<SandboxResponse>(response, "E2B", MAX_MANAGEMENT_RESPONSE_BYTES)
            .await?;
    validate_sandbox_id(&body.sandbox_id)?;
    let access_token = body
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            ExecError::Unavailable("E2B did not return a secure sandbox token".into())
        })?;
    Ok(RemoteSession {
        sandbox_id: body.sandbox_id,
        endpoint: None,
        access_token: Some(access_token),
    })
}

fn provider_status_error(status: StatusCode) -> ExecError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            ExecError::Unavailable("E2B credential was rejected".into())
        }
        StatusCode::TOO_MANY_REQUESTS => ExecError::Unavailable("E2B rate limit exceeded".into()),
        _ => ExecError::Unavailable(format!(
            "E2B request failed with status {}",
            status.as_u16()
        )),
    }
}

fn validate_sandbox_id(value: &str) -> Result<(), ExecError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ExecError::Unavailable(
            "E2B returned an invalid sandbox identity".into(),
        ));
    }
    Ok(())
}

/// Map a validated workspace-relative path onto the sandbox user's home, the
/// same root the command contract uses for its working directory.
fn remote_file_path(path: Option<&WorkspaceFilePath>) -> String {
    match path {
        None => E2B_WORKSPACE_ROOT.to_owned(),
        Some(path) => format!("{E2B_WORKSPACE_ROOT}/{}", path.as_str()),
    }
}

/// Connect's JSON mapping encodes 64-bit sizes as strings; older envd builds
/// omit the field entirely.
fn json_u64(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

#[derive(Serialize)]
struct ListDirRequest {
    path: String,
    depth: u32,
}

#[derive(Deserialize)]
struct ListDirResponse {
    #[serde(default)]
    entries: Vec<ListDirEntry>,
}

#[derive(Deserialize)]
struct ListDirEntry {
    #[serde(default)]
    name: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    path: Option<String>,
    size: Option<serde_json::Value>,
}

fn remote_cwd(cwd: &str) -> Result<String, ExecError> {
    let mut remote = E2B_WORKSPACE_ROOT.to_owned();
    for component in Path::new(cwd).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| ExecError::InvalidRequest("invalid working directory".into()))?;
                remote.push('/');
                remote.push_str(part);
            }
            _ => {
                return Err(ExecError::InvalidRequest(
                    "invalid working directory".into(),
                ));
            }
        }
    }
    Ok(remote)
}

#[derive(Serialize)]
struct StartRequest<'a> {
    process: ProcessConfig<'a>,
    stdin: bool,
}

#[derive(Serialize)]
struct ProcessConfig<'a> {
    cmd: &'a str,
    args: &'a [String],
    envs: HashMap<&'static str, &'static str>,
    cwd: String,
}

fn connect_envelope(flags: u8, payload: &[u8]) -> Result<Vec<u8>, ExecError> {
    let length = u32::try_from(payload.len())
        .map_err(|_| ExecError::InvalidRequest("E2B request exceeds the protocol bound".into()))?;
    let mut encoded = Vec::with_capacity(payload.len() + 5);
    encoded.push(flags);
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

async fn decode_command_response(
    response: Response,
    started: Instant,
) -> Result<ExecResponse, ExecError> {
    let mut decoder = EnvelopeDecoder::default();
    let mut stream = response.bytes_stream();
    let mut capture = Capture::default();
    let mut process_end = None;
    let mut end_stream = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ExecError::AmbiguousExecution)?;
        for envelope in decoder.decode(&chunk)? {
            if envelope.flags & CONNECT_COMPRESSED_FLAG != 0 {
                return Err(ExecError::Unavailable(
                    "E2B returned unsupported compressed output".into(),
                ));
            }
            if envelope.flags & CONNECT_END_STREAM_FLAG != 0 {
                if end_stream {
                    return Err(ExecError::Unavailable(
                        "E2B returned duplicate stream trailers".into(),
                    ));
                }
                end_stream = true;
                let end: ConnectEndStream =
                    serde_json::from_slice(&envelope.payload).map_err(|_| {
                        ExecError::Unavailable("E2B returned invalid stream trailers".into())
                    })?;
                if let Some(error) = end.error {
                    if error.code == "deadline_exceeded" {
                        return Ok(command_response(started, capture, None, true));
                    }
                    return Err(ExecError::Unavailable(format!(
                        "E2B command stream failed: {}",
                        error.code
                    )));
                }
                continue;
            }
            if end_stream {
                return Err(ExecError::Unavailable(
                    "E2B returned output after stream trailers".into(),
                ));
            }
            let event: StartResponse = serde_json::from_slice(&envelope.payload).map_err(|_| {
                ExecError::Unavailable("E2B returned an invalid command event".into())
            })?;
            let Some(event) = event.event else {
                continue;
            };
            if let Some(data) = event.data {
                if let Some(stdout) = data.stdout {
                    capture.append_base64(&stdout, StreamKind::Stdout, "E2B")?;
                }
                if let Some(stderr) = data.stderr {
                    capture.append_base64(&stderr, StreamKind::Stderr, "E2B")?;
                }
            }
            if let Some(end) = event.end {
                if process_end.is_some() {
                    return Err(ExecError::Unavailable(
                        "E2B returned duplicate process completion".into(),
                    ));
                }
                if let Some(error) = end.error.as_deref().filter(|error| !error.is_empty()) {
                    capture.append(error.as_bytes(), StreamKind::Stderr);
                }
                process_end = Some(end);
            }
        }
    }

    if decoder.has_incomplete_frame() || !end_stream {
        return Err(ExecError::AmbiguousExecution);
    }
    let end = process_end.ok_or(ExecError::AmbiguousExecution)?;
    let exit_code = end.exited.then_some(end.exit_code.unwrap_or_default());
    Ok(command_response(started, capture, exit_code, false))
}

fn command_response(
    started: Instant,
    capture: Capture,
    exit_code: Option<i32>,
    timed_out: bool,
) -> ExecResponse {
    capture.response(ExecProviderKind::E2b, started, exit_code, timed_out)
}

#[derive(Default)]
struct EnvelopeDecoder {
    buffer: Vec<u8>,
}

struct Envelope {
    flags: u8,
    payload: Vec<u8>,
}

impl EnvelopeDecoder {
    fn decode(&mut self, chunk: &[u8]) -> Result<Vec<Envelope>, ExecError> {
        if chunk.len() > MAX_CONNECT_FRAME_BYTES.saturating_mul(2) {
            return Err(ExecError::Unavailable(
                "E2B returned an oversized stream chunk".into(),
            ));
        }
        self.buffer.extend_from_slice(chunk);
        let mut offset = 0;
        let mut envelopes = Vec::new();
        loop {
            if self.buffer.len().saturating_sub(offset) < 5 {
                break;
            }
            let flags = self.buffer[offset];
            let length = u32::from_be_bytes([
                self.buffer[offset + 1],
                self.buffer[offset + 2],
                self.buffer[offset + 3],
                self.buffer[offset + 4],
            ]) as usize;
            if length > MAX_CONNECT_FRAME_BYTES {
                return Err(ExecError::Unavailable(
                    "E2B returned an oversized stream frame".into(),
                ));
            }
            let end = offset
                .checked_add(5)
                .and_then(|start| start.checked_add(length))
                .ok_or_else(|| {
                    ExecError::Unavailable("E2B returned an invalid stream frame".into())
                })?;
            if self.buffer.len() < end {
                break;
            }
            envelopes.push(Envelope {
                flags,
                payload: self.buffer[offset + 5..end].to_vec(),
            });
            offset = end;
        }
        if offset > 0 {
            self.buffer.drain(..offset);
        }
        if self.buffer.len() > MAX_CONNECT_FRAME_BYTES + 5 {
            return Err(ExecError::Unavailable(
                "E2B returned an oversized incomplete frame".into(),
            ));
        }
        Ok(envelopes)
    }

    fn has_incomplete_frame(&self) -> bool {
        !self.buffer.is_empty()
    }
}

#[derive(Deserialize)]
struct StartResponse {
    event: Option<ProcessEvent>,
}

#[derive(Deserialize)]
struct ProcessEvent {
    data: Option<ProcessData>,
    end: Option<ProcessEnd>,
}

#[derive(Deserialize)]
struct ProcessData {
    stdout: Option<String>,
    stderr: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessEnd {
    exit_code: Option<i32>,
    #[serde(default)]
    exited: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ConnectEndStream {
    error: Option<ConnectStreamError>,
}

#[derive(Deserialize)]
struct ConnectStreamError {
    code: String,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::body::Bytes;
    use axum::extract::{Path as AxumPath, Query, State};
    use axum::http::{header, HeaderMap};
    use axum::routing::{delete, post};
    use axum::{Json, Router};
    use base64::Engine as _;
    use serde_json::{json, Value};

    use super::*;
    use crate::{ExecutionId, ExecutionWorkspaceId};

    #[derive(Clone, Copy)]
    enum ProcessMode {
        Complete,
        Timeout,
    }

    struct MockState {
        mode: ProcessMode,
        /// Template id the mock refuses to resolve, answering the 404 E2B
        /// returns for an alias its team cannot see.
        missing_template: Option<String>,
        creates: AtomicUsize,
        connects: AtomicUsize,
        starts: AtomicUsize,
        deletes: AtomicUsize,
        uploads: AtomicUsize,
        create_body: StdMutex<Option<Value>>,
        create_templates: StdMutex<Vec<String>>,
        start_bodies: StdMutex<Vec<Vec<u8>>>,
        api_keys: StdMutex<Vec<String>>,
        access_tokens: StdMutex<Vec<String>>,
        files: StdMutex<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl MockState {
        fn new(mode: ProcessMode) -> Self {
            Self {
                mode,
                missing_template: None,
                creates: AtomicUsize::new(0),
                connects: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                deletes: AtomicUsize::new(0),
                uploads: AtomicUsize::new(0),
                create_body: StdMutex::new(None),
                create_templates: StdMutex::new(Vec::new()),
                start_bodies: StdMutex::new(Vec::new()),
                api_keys: StdMutex::new(Vec::new()),
                access_tokens: StdMutex::new(Vec::new()),
                files: StdMutex::new(std::collections::HashMap::new()),
            }
        }
    }

    async fn mock_server(
        mode: ProcessMode,
    ) -> (Arc<MockState>, String, tokio::task::JoinHandle<()>) {
        serve(MockState::new(mode)).await
    }

    /// A mock whose account cannot resolve `missing_template` — E2B's answer
    /// when the alias is unknown or not published to this team.
    async fn mock_server_without_template(
        missing_template: &str,
    ) -> (Arc<MockState>, String, tokio::task::JoinHandle<()>) {
        let mut state = MockState::new(ProcessMode::Complete);
        state.missing_template = Some(missing_template.to_owned());
        serve(state).await
    }

    async fn serve(state: MockState) -> (Arc<MockState>, String, tokio::task::JoinHandle<()>) {
        let state = Arc::new(state);
        let app = Router::new()
            .route("/sandboxes", post(mock_create))
            .route("/sandboxes/{sandbox_id}", delete(mock_delete))
            .route("/sandboxes/{sandbox_id}/connect", post(mock_connect))
            .route("/process.Process/Start", post(mock_start))
            .route("/files", post(mock_upload).get(mock_download))
            .route("/filesystem.Filesystem/ListDir", post(mock_list))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (state, base, server)
    }

    fn provider_at(
        base: &str,
        pool: RemoteSessionPool,
        egress: Option<EgressPolicy>,
    ) -> E2BExecutionProvider {
        let provider = E2BExecutionProvider::with_endpoints(
            E2BCredential::parse("test-e2b-key").unwrap(),
            Duration::from_secs(2),
            pool,
            E2BEndpoints {
                api_base: base.to_owned(),
                sandbox_base: base.to_owned(),
            },
        )
        .unwrap();
        match egress {
            Some(policy) => provider.with_egress_policy(policy),
            None => provider,
        }
    }

    async fn mock_provider(
        mode: ProcessMode,
        egress: Option<EgressPolicy>,
    ) -> (
        E2BExecutionProvider,
        Arc<MockState>,
        tokio::task::JoinHandle<()>,
    ) {
        let (state, base, server) = mock_server(mode).await;
        let provider = provider_at(&base, RemoteSessionPool::default(), egress);
        (provider, state, server)
    }

    async fn mock_create(
        State(state): State<Arc<MockState>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
        state.creates.fetch_add(1, Ordering::SeqCst);
        state
            .api_keys
            .lock()
            .unwrap()
            .push(header_value(&headers, "x-api-key"));
        let template = body["templateID"].as_str().unwrap().to_owned();
        state
            .create_templates
            .lock()
            .unwrap()
            .push(template.clone());
        if state.missing_template.as_deref() == Some(template.as_str()) {
            // The body E2B's API returns for an unresolvable template.
            return Err((
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({
                    "code": 404,
                    "message": format!("template '{template}' not found"),
                })),
            ));
        }
        *state.create_body.lock().unwrap() = Some(body);
        Ok(Json(json!({
            "sandboxID": "sandbox-123",
            "envdVersion": "0.3.0",
            "envdAccessToken": "access-123"
        })))
    }

    async fn mock_connect(
        State(state): State<Arc<MockState>>,
        AxumPath(sandbox_id): AxumPath<String>,
        headers: HeaderMap,
    ) -> Json<Value> {
        assert_eq!(sandbox_id, "sandbox-123");
        state.connects.fetch_add(1, Ordering::SeqCst);
        state
            .api_keys
            .lock()
            .unwrap()
            .push(header_value(&headers, "x-api-key"));
        Json(json!({
            "sandboxID": "sandbox-123",
            "envdVersion": "0.3.0",
            "envdAccessToken": "access-123"
        }))
    }

    async fn mock_start(
        State(state): State<Arc<MockState>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> ([(header::HeaderName, &'static str); 1], Vec<u8>) {
        state.starts.fetch_add(1, Ordering::SeqCst);
        assert_eq!(header_value(&headers, "e2b-sandbox-id"), "sandbox-123");
        assert_eq!(header_value(&headers, "e2b-sandbox-port"), "49983");
        assert_eq!(header_value(&headers, "connect-protocol-version"), "1");
        state
            .access_tokens
            .lock()
            .unwrap()
            .push(header_value(&headers, "x-access-token"));
        state.start_bodies.lock().unwrap().push(body.to_vec());

        let mut response = Vec::new();
        response.extend(frame(0, &json!({"event": {"start": {"pid": 7}}})));
        response.extend(frame(
            0,
            &json!({
                "event": {
                    "data": {
                        "stdout": base64::engine::general_purpose::STANDARD.encode("ok\n")
                    }
                }
            }),
        ));
        match state.mode {
            ProcessMode::Complete => {
                response.extend(frame(
                    0,
                    &json!({
                        "event": {
                            "end": {
                                "exitCode": 0,
                                "exited": true,
                                "status": "exited"
                            }
                        }
                    }),
                ));
                response.extend(frame(CONNECT_END_STREAM_FLAG, &json!({})));
            }
            ProcessMode::Timeout => {
                response.extend(frame(
                    CONNECT_END_STREAM_FLAG,
                    &json!({
                        "error": {
                            "code": "deadline_exceeded",
                            "message": "command timed out"
                        }
                    }),
                ));
            }
        }
        (
            [(header::CONTENT_TYPE, "application/connect+json")],
            response,
        )
    }

    async fn mock_delete(
        State(state): State<Arc<MockState>>,
        AxumPath(sandbox_id): AxumPath<String>,
        headers: HeaderMap,
    ) -> axum::http::StatusCode {
        assert_eq!(sandbox_id, "sandbox-123");
        assert_eq!(header_value(&headers, "x-api-key"), "test-e2b-key");
        state.deletes.fetch_add(1, Ordering::SeqCst);
        axum::http::StatusCode::NO_CONTENT
    }

    async fn mock_upload(
        State(state): State<Arc<MockState>>,
        Query(query): Query<std::collections::HashMap<String, String>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> axum::http::StatusCode {
        assert_eq!(query["username"], "user");
        assert_eq!(header_value(&headers, "x-access-token"), "access-123");
        state.uploads.fetch_add(1, Ordering::SeqCst);
        let content = multipart_content(&headers, &body);
        state
            .files
            .lock()
            .unwrap()
            .insert(query["path"].clone(), content);
        axum::http::StatusCode::OK
    }

    async fn mock_download(
        State(state): State<Arc<MockState>>,
        Query(query): Query<std::collections::HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Result<Vec<u8>, axum::http::StatusCode> {
        assert_eq!(query["username"], "user");
        assert_eq!(header_value(&headers, "x-access-token"), "access-123");
        state
            .files
            .lock()
            .unwrap()
            .get(&query["path"])
            .cloned()
            .ok_or(axum::http::StatusCode::NOT_FOUND)
    }

    async fn mock_list(
        State(state): State<Arc<MockState>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        assert_eq!(header_value(&headers, "connect-protocol-version"), "1");
        assert_eq!(body["path"], E2B_WORKSPACE_ROOT);
        let files = state.files.lock().unwrap();
        let entries: Vec<Value> = files
            .iter()
            .map(|(path, content)| {
                json!({
                    "name": path.rsplit('/').next().unwrap(),
                    "type": "FILE_TYPE_FILE",
                    "path": path,
                    // Connect's JSON mapping renders uint64 as a string.
                    "size": content.len().to_string(),
                })
            })
            .collect();
        Json(json!({ "entries": entries }))
    }

    fn multipart_content(headers: &HeaderMap, body: &[u8]) -> Vec<u8> {
        let content_type = header_value(headers, "content-type");
        let boundary = content_type.split("boundary=").nth(1).unwrap().to_owned();
        let start = body.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let tail = format!("\r\n--{boundary}--\r\n");
        assert!(body.ends_with(tail.as_bytes()));
        body[start..body.len() - tail.len()].to_vec()
    }

    fn header_value(headers: &HeaderMap, name: &str) -> String {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_owned()
    }

    fn frame(flags: u8, value: &Value) -> Vec<u8> {
        connect_envelope(flags, &serde_json::to_vec(value).unwrap()).unwrap()
    }

    fn request(execution: &str, workspace: &str, argument: &str) -> ExecRequest {
        ExecRequest::new(
            ExecutionId::parse(execution).unwrap(),
            ExecutionWorkspaceId::parse(workspace).unwrap(),
            "/bin/echo",
            vec![argument.into()],
            ".",
        )
        .unwrap()
    }

    fn decode_start_request(body: &[u8]) -> Value {
        assert_eq!(body[0], 0);
        let length = u32::from_be_bytes(body[1..5].try_into().unwrap()) as usize;
        assert_eq!(length, body.len() - 5);
        serde_json::from_slice(&body[5..]).unwrap()
    }

    #[tokio::test]
    async fn e2b_reuses_the_chat_sandbox_and_replays_an_exact_execution() {
        let (provider, state, server) = mock_provider(ProcessMode::Complete, None).await;
        let first_request = request("execution-1", "workspace-1", "first");
        let first = provider.execute(first_request.clone()).await.unwrap();
        assert_eq!(first.provider, ExecProviderKind::E2b);
        assert_eq!(first.exit_code, Some(0));
        assert_eq!(first.stdout, "ok\n");

        let replay = provider.execute(first_request.clone()).await.unwrap();
        assert_eq!(replay, first);
        assert_eq!(state.starts.load(Ordering::SeqCst), 1);

        let conflict = ExecRequest::new(
            first_request.execution_id.clone(),
            first_request.workspace_id.clone(),
            "/bin/echo",
            vec!["different".into()],
            ".",
        )
        .unwrap();
        assert!(matches!(
            provider.execute(conflict).await,
            Err(ExecError::IdentityConflict)
        ));

        provider
            .execute(request("execution-2", "workspace-1", "second"))
            .await
            .unwrap();
        assert_eq!(state.creates.load(Ordering::SeqCst), 1);
        assert_eq!(state.connects.load(Ordering::SeqCst), 1);
        assert_eq!(state.starts.load(Ordering::SeqCst), 2);
        assert_eq!(
            state.api_keys.lock().unwrap().as_slice(),
            ["test-e2b-key", "test-e2b-key"]
        );
        assert_eq!(
            state.access_tokens.lock().unwrap().as_slice(),
            ["access-123", "access-123"]
        );

        let create = state.create_body.lock().unwrap().clone().unwrap();
        // Spelled out rather than compared to the constant: reverting the
        // default to E2B's public template silently costs every document run
        // an in-sandbox dependency install, and moving it back to the human
        // alias breaks every account but the one that published the template.
        assert_eq!(create["templateID"], "yarxjy39quzg6wm78u1a");
        assert_eq!(create["metadata"]["tidebreak_workspace_id"], "workspace-1");
        assert_eq!(create["secure"], true);
        assert_eq!(create["allow_internet_access"], true);
        assert!(create.get("network").is_none());

        let bodies = state.start_bodies.lock().unwrap();
        let first_start = decode_start_request(&bodies[0]);
        assert_eq!(first_start["process"]["cmd"], "/bin/echo");
        assert_eq!(first_start["process"]["args"], json!(["first"]));
        assert_eq!(first_start["process"]["cwd"], E2B_WORKSPACE_ROOT);
        assert_eq!(first_start["stdin"], false);
        server.abort();
    }

    #[tokio::test]
    async fn e2b_workspace_lifecycle_round_trips_files_over_the_session_api() {
        let (provider, state, server) = mock_provider(ProcessMode::Complete, None).await;
        let workspace = ExecutionWorkspaceId::parse("workspace-files").unwrap();

        // No pooled handle yet: connect reports unreachable without failing.
        assert!(!provider.connect_workspace(&workspace).await.unwrap());
        provider.create_workspace(&workspace).await.unwrap();
        assert_eq!(state.creates.load(Ordering::SeqCst), 1);
        assert!(provider.connect_workspace(&workspace).await.unwrap());

        let path = WorkspaceFilePath::parse("data/report.bin").unwrap();
        let content = b"\x00tidebreak\xff".to_vec();
        provider
            .put_workspace_file(&workspace, &path, &content)
            .await
            .unwrap();
        assert_eq!(
            state
                .files
                .lock()
                .unwrap()
                .get("/home/user/data/report.bin"),
            Some(&content)
        );
        assert_eq!(
            provider
                .get_workspace_file(&workspace, &path)
                .await
                .unwrap(),
            content
        );
        assert!(matches!(
            provider
                .get_workspace_file(&workspace, &WorkspaceFilePath::parse("missing").unwrap())
                .await,
            Err(ExecError::WorkspaceFileNotFound)
        ));

        let listing = provider
            .list_workspace_files(&workspace, None)
            .await
            .unwrap();
        assert!(!listing.truncated);
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].path, "data/report.bin");
        assert!(!listing.entries[0].directory);
        assert_eq!(listing.entries[0].size_bytes, Some(content.len() as u64));

        provider.destroy_workspace(&workspace).await.unwrap();
        assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
        // The handle is released, so the workspace reports unreachable and a
        // second destroy is a no-op rather than another provider call.
        assert!(!provider.connect_workspace(&workspace).await.unwrap());
        provider.destroy_workspace(&workspace).await.unwrap();
        assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
        server.abort();
    }

    /// A reused sandbox session skips re-uploading content it already holds,
    /// re-uploads changed content, and forgets everything once the sandbox is
    /// destroyed — a successor sandbox must be staged from scratch.
    #[tokio::test]
    async fn e2b_staging_skips_content_the_live_sandbox_already_has() {
        let (provider, state, server) = mock_provider(ProcessMode::Complete, None).await;
        let workspace = ExecutionWorkspaceId::parse("workspace-stage").unwrap();
        let path = WorkspaceFilePath::parse("build_deck.py").unwrap();

        let staged = |content: &'static [u8]| {
            let provider = &provider;
            let workspace = &workspace;
            let path = &path;
            async move {
                provider
                    .stage_workspace_file(workspace, path, content)
                    .await
                    .unwrap()
            }
        };

        assert_eq!(staged(b"v1").await, crate::StagedUpload::Uploaded);
        assert_eq!(staged(b"v1").await, crate::StagedUpload::AlreadyCurrent);
        assert_eq!(state.uploads.load(Ordering::SeqCst), 1);

        assert_eq!(staged(b"v2").await, crate::StagedUpload::Uploaded);
        assert_eq!(state.uploads.load(Ordering::SeqCst), 2);

        provider.destroy_workspace(&workspace).await.unwrap();
        assert_eq!(staged(b"v2").await, crate::StagedUpload::Uploaded);
        assert_eq!(state.uploads.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn e2b_compiles_the_egress_policy_into_the_creation_request() {
        use tidebreak_egress::{CidrBlock, DomainPattern, EgressAllowlist};

        let policy = EgressPolicy::Allowlist(EgressAllowlist::new(
            vec![DomainPattern::parse("*.pypi.org").unwrap()],
            vec![CidrBlock::parse("140.82.112.0/20").unwrap()],
        ));
        let (provider, state, server) = mock_provider(ProcessMode::Complete, Some(policy)).await;
        provider
            .execute(request("execution-net", "workspace-net", "hello"))
            .await
            .unwrap();
        let create = state.create_body.lock().unwrap().clone().unwrap();
        assert_eq!(create["allow_internet_access"], false);
        assert_eq!(
            create["network"]["allowOut"],
            json!(["*.pypi.org", "140.82.112.0/20"])
        );

        // Block-all needs no allow rules: the blocked default is the policy.
        let block_all = e2b_network_settings(Some(&EgressPolicy::BlockAll));
        assert!(!block_all.allow_internet_access);
        assert!(block_all.network.is_none());
        server.abort();
    }

    /// Reproduction of a bug we hit: a chat starts with network off, the E2B
    /// sandbox is created deny-by-default, and after the user opens the chat's
    /// network policy every pip install still fails DNS because the pooled
    /// sandbox — whose egress is fixed at creation — keeps being reused. A
    /// policy change must destroy the stale sandbox and create one under the
    /// new policy; an unchanged policy must keep reusing the live sandbox.
    #[tokio::test]
    async fn e2b_replaces_the_pooled_sandbox_when_the_egress_policy_changes() {
        use tidebreak_egress::EgressAllowlist;

        let (state, base, server) = mock_server(ProcessMode::Complete).await;
        let pool = RemoteSessionPool::default();

        // Chat network policy off: deny-all sandbox.
        let blocked = provider_at(
            &base,
            pool.clone(),
            Some(EgressPolicy::Allowlist(EgressAllowlist::default())),
        );
        blocked
            .execute(request("execution-1", "workspace-policy", "first"))
            .await
            .unwrap();
        assert_eq!(state.creates.load(Ordering::SeqCst), 1);
        assert_eq!(state.deletes.load(Ordering::SeqCst), 0);

        // The user opens the policy; resolve() builds a fresh provider over the
        // same pool. The stale deny-all sandbox is destroyed and replaced by an
        // open-internet one instead of being reused.
        let open = provider_at(&base, pool.clone(), None);
        open.execute(request("execution-2", "workspace-policy", "second"))
            .await
            .unwrap();
        assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
        assert_eq!(state.creates.load(Ordering::SeqCst), 2);
        let create = state.create_body.lock().unwrap().clone().unwrap();
        assert_eq!(create["allow_internet_access"], true);

        // Unchanged policy: the live sandbox is reconnected, not replaced.
        let same = provider_at(&base, pool, None);
        same.execute(request("execution-3", "workspace-policy", "third"))
            .await
            .unwrap();
        assert_eq!(state.creates.load(Ordering::SeqCst), 2);
        assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
        server.abort();
    }

    /// The Tidebreak template is published from one account and consumed by
    /// every account, so a client can outrun the publish or see it withdrawn.
    /// Creation then falls back to E2B's public template — once, with the
    /// decision latched so later sandboxes skip the doomed first attempt.
    #[tokio::test]
    async fn e2b_falls_back_to_the_public_template_when_tidebreak_s_is_unresolvable() {
        let (state, base, server) = mock_server_without_template(E2B_TEMPLATE).await;
        let provider = provider_at(&base, RemoteSessionPool::default(), None);

        let first = provider
            .execute(request("execution-1", "workspace-a", "first"))
            .await
            .unwrap();
        let second = provider
            .execute(request("execution-2", "workspace-b", "second"))
            .await
            .unwrap();

        assert_eq!(
            state.create_templates.lock().unwrap().as_slice(),
            [E2B_TEMPLATE, E2B_FALLBACK_TEMPLATE, E2B_FALLBACK_TEMPLATE]
        );
        // The run that discovered the missing template reports the degraded
        // setup; the ones after it inherit the latch and report nothing.
        assert_eq!(
            first.degraded,
            Some(ExecDegradation::SandboxImageUnavailable)
        );
        assert_eq!(second.degraded, None);
        server.abort();
    }

    /// A template the caller named is theirs to fix: creation surfaces the
    /// failure instead of quietly running on different sandbox contents.
    #[tokio::test]
    async fn e2b_does_not_fall_back_from_an_explicit_template_override() {
        let (state, base, server) = mock_server_without_template("acme-custom").await;
        let provider =
            provider_at(&base, RemoteSessionPool::default(), None).with_template("acme-custom");

        let error = provider
            .execute(request("execution-1", "workspace-a", "first"))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ExecError::Unavailable(message) if message.contains("acme-custom")),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            state.create_templates.lock().unwrap().as_slice(),
            ["acme-custom"]
        );
        server.abort();
    }

    #[tokio::test]
    async fn e2b_projects_connect_deadlines_to_the_bounded_timeout_result() {
        let (provider, state, server) = mock_provider(ProcessMode::Timeout, None).await;
        let response = provider
            .execute(request("execution-timeout", "workspace-timeout", "slow"))
            .await
            .unwrap();
        assert!(response.timed_out);
        assert_eq!(response.exit_code, None);
        assert_eq!(response.stdout, "ok\n");
        assert!(!response.output_truncated);
        assert_eq!(state.starts.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
