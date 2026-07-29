use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use openwave_core::SecretProvider;
use openwave_egress::{
    EgressEnforcement, EgressPolicy, EnforcementException, ExceptionReach, ExceptionScope,
};
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};

use crate::credential::SecretCredential;
use crate::http::{decode_bounded_json, download_bounded_file, multipart_file};
use crate::output::{Capture, StreamKind};
use crate::remote::{
    connect_remote_workspace, create_remote_workspace, destroy_remote_workspace, execute_remote,
    with_remote_session, RemoteSandboxAdapter, RemoteSession, RemoteSessionError,
    RemoteSessionPool, RemoteWorkspaceAdapter,
};
use crate::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse, ExecutionWorkspaceId, WorkspaceFileEntry, WorkspaceFilePath,
    WorkspaceLifecycle, WorkspaceListing, MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
};

const DAYTONA_API_BASE: &str = "https://app.daytona.io/api";
const DAYTONA_IDLE_MINUTES: u32 = 5;
const DAYTONA_START_TIMEOUT: Duration = Duration::from_secs(60);
const DAYTONA_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DAYTONA_TRANSPORT_GRACE: Duration = Duration::from_secs(10);
const MAX_DAYTONA_RESPONSE_BYTES: usize = 1024 * 1024;
/// Daytona accepts at most this many CIDR entries per sandbox allowlist.
const DAYTONA_MAX_NETWORK_ALLOW_ENTRIES: usize = 10;
/// Daytona accepts at most this many domain entries per sandbox allowlist.
const DAYTONA_MAX_DOMAIN_ALLOW_ENTRIES: usize = 20;

/// Fixed key for Daytona credentials in OpenWave's host secret store.
pub const DAYTONA_CREDENTIAL_KEY: &str = "code_execution.daytona.api_key";

/// Non-serializable, redacted Daytona API credential.
#[derive(Clone)]
pub struct DaytonaCredential(SecretCredential);

impl std::fmt::Debug for DaytonaCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("DaytonaCredential")
            .field(&"***")
            .finish()
    }
}

impl DaytonaCredential {
    pub fn parse(value: impl Into<String>) -> Result<Self, CodeExecutionError> {
        SecretCredential::parse("Daytona", value).map(Self)
    }

    /// Resolve the credential without exposing it through serializable config.
    pub async fn load(secrets: &dyn SecretProvider) -> Result<Option<Self>, CodeExecutionError> {
        SecretCredential::load(secrets, DAYTONA_CREDENTIAL_KEY, "Daytona")
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

/// Direct-command adapter for Daytona's managed sandbox service.
pub struct DaytonaExecutionProvider {
    credential: DaytonaCredential,
    timeout: Duration,
    pool: RemoteSessionPool,
    client: Client,
    endpoints: DaytonaEndpoints,
    egress: Option<EgressPolicy>,
}

#[derive(Clone)]
struct DaytonaEndpoints {
    api_base: String,
    allow_insecure_toolbox: bool,
}

impl Default for DaytonaEndpoints {
    fn default() -> Self {
        Self {
            api_base: DAYTONA_API_BASE.into(),
            allow_insecure_toolbox: false,
        }
    }
}

impl DaytonaExecutionProvider {
    pub fn new(
        credential: DaytonaCredential,
        timeout: Duration,
    ) -> Result<Self, CodeExecutionError> {
        Self::with_session_pool(credential, timeout, RemoteSessionPool::default())
    }

    pub fn with_session_pool(
        credential: DaytonaCredential,
        timeout: Duration,
        pool: RemoteSessionPool,
    ) -> Result<Self, CodeExecutionError> {
        Self::with_endpoints(credential, timeout, pool, DaytonaEndpoints::default())
    }

    fn with_endpoints(
        credential: DaytonaCredential,
        timeout: Duration,
        pool: RemoteSessionPool,
        endpoints: DaytonaEndpoints,
    ) -> Result<Self, CodeExecutionError> {
        if timeout.is_zero() {
            return Err(CodeExecutionError::InvalidRequest(
                "execution timeout must be positive".into(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout.saturating_add(DAYTONA_TRANSPORT_GRACE))
            .build()
            .map_err(|_| {
                CodeExecutionError::Unavailable("could not configure Daytona transport".into())
            })?;
        Ok(Self {
            credential,
            timeout,
            pool,
            client,
            endpoints,
            egress: None,
        })
    }

    /// Compile an egress policy into every sandbox this provider creates.
    ///
    /// Without a policy the provider keeps today's disclosed open-egress
    /// creation. With one, sandboxes are created with Daytona's block-all
    /// switch or its per-sandbox allowlists — subject to the vendor exception
    /// list declared by [`Self::egress_enforcement`], and to Daytona's plan
    /// gating on per-sandbox network overrides. Fails when the allowlist
    /// exceeds Daytona's entry limits, before any sandbox is created.
    pub fn with_egress_policy(mut self, policy: EgressPolicy) -> Result<Self, CodeExecutionError> {
        if let EgressPolicy::Allowlist(allowlist) = &policy {
            if allowlist.cidrs().len() > DAYTONA_MAX_NETWORK_ALLOW_ENTRIES {
                return Err(CodeExecutionError::InvalidRequest(format!(
                    "Daytona accepts at most {DAYTONA_MAX_NETWORK_ALLOW_ENTRIES} egress address blocks"
                )));
            }
            if allowlist.domains().len() > DAYTONA_MAX_DOMAIN_ALLOW_ENTRIES {
                return Err(CodeExecutionError::InvalidRequest(format!(
                    "Daytona accepts at most {DAYTONA_MAX_DOMAIN_ALLOW_ENTRIES} egress domain patterns"
                )));
            }
        }
        self.egress = Some(policy);
        Ok(self)
    }

    /// The egress policy compiled into this provider's sandboxes, or `None` for
    /// today's open-internet creation. Exposed so the host wiring that selects
    /// and applies a policy can be verified without a live API.
    #[doc(hidden)]
    #[must_use]
    pub fn egress_policy(&self) -> Option<&EgressPolicy> {
        self.egress.as_ref()
    }

    /// Host knowledge about Daytona's per-sandbox network enforcement,
    /// declared as what it actually blocks. Daytona keeps a vendor-curated
    /// "essential services" list reachable regardless of policy — package
    /// registries, public git hosting, container registries, and AI APIs —
    /// each a general-purpose destination, so this surface does not qualify
    /// as a boundary for third-party-credential-bearing work.
    #[must_use]
    pub fn egress_enforcement() -> EgressEnforcement {
        let curated = |purpose: &'static str| EnforcementException {
            scope: ExceptionScope::VendorCurated,
            reach: ExceptionReach::GeneralPurpose,
            purpose,
        };
        EgressEnforcement::external(vec![
            curated("package registries"),
            curated("git hosting"),
            curated("container registries"),
            curated("AI APIs"),
        ])
    }

    async fn create_sandbox(
        &self,
        workspace_id: &str,
    ) -> Result<RemoteSession, CodeExecutionError> {
        let response = self
            .client
            .post(self.api_url("/sandbox"))
            .bearer_auth(self.credential.as_str())
            .json(&CreateSandboxRequest {
                labels: HashMap::from([
                    ("openwave_workspace_id", workspace_id),
                    ("code-toolbox-language", "python"),
                ]),
                network: daytona_network_settings(self.egress.as_ref()),
                auto_stop_interval: DAYTONA_IDLE_MINUTES,
                // Delete once the idle stop happens. A later command creates a
                // fresh chat workspace instead of leaving stopped resources.
                auto_delete_interval: 0,
            })
            .send()
            .await
            .map_err(|_| CodeExecutionError::Unavailable("could not reach Daytona".into()))?;
        let sandbox = self.decode_sandbox(response).await?;
        self.wait_until_started(sandbox, true)
            .await?
            .ok_or_else(|| {
                CodeExecutionError::Unavailable(
                    "Daytona sandbox disappeared while it was starting".into(),
                )
            })
    }

    async fn reconnect_sandbox(
        &self,
        sandbox_id: &str,
    ) -> Result<Option<RemoteSession>, CodeExecutionError> {
        validate_sandbox_id(sandbox_id)?;
        let response = self
            .client
            .get(self.api_url(&format!("/sandbox/{sandbox_id}")))
            .bearer_auth(self.credential.as_str())
            .send()
            .await
            .map_err(|_| CodeExecutionError::Unavailable("could not reach Daytona".into()))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let sandbox = self.decode_sandbox(response).await?;
        self.wait_until_started(sandbox, true).await
    }

    async fn wait_until_started(
        &self,
        mut sandbox: DaytonaSandbox,
        start_if_stopped: bool,
    ) -> Result<Option<RemoteSession>, CodeExecutionError> {
        let deadline = Instant::now() + DAYTONA_START_TIMEOUT;
        let mut requested_start = false;
        loop {
            match sandbox.state.as_deref() {
                Some("started") => {
                    return sandbox
                        .into_session(self.endpoints.allow_insecure_toolbox)
                        .map(Some);
                }
                Some("destroyed" | "destroying") => return Ok(None),
                Some("error" | "build_failed") => {
                    return Err(CodeExecutionError::Unavailable(
                        "Daytona sandbox failed to start".into(),
                    ));
                }
                Some("stopped" | "paused" | "archived") if start_if_stopped && !requested_start => {
                    let response = self
                        .client
                        .post(self.api_url(&format!("/sandbox/{}/start", sandbox.id)))
                        .bearer_auth(self.credential.as_str())
                        .send()
                        .await
                        .map_err(|_| {
                            CodeExecutionError::Unavailable("could not reach Daytona".into())
                        })?;
                    if response.status() == StatusCode::NOT_FOUND {
                        return Ok(None);
                    }
                    sandbox = self.decode_sandbox(response).await?;
                    requested_start = true;
                    continue;
                }
                Some(
                    "creating" | "restoring" | "starting" | "pending_build" | "building_snapshot"
                    | "pulling_snapshot" | "resuming",
                ) => {}
                Some("stopped" | "paused" | "archived") => {
                    return Err(CodeExecutionError::Unavailable(
                        "Daytona sandbox did not start".into(),
                    ));
                }
                _ => {
                    return Err(CodeExecutionError::Unavailable(
                        "Daytona returned an unknown sandbox state".into(),
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Err(CodeExecutionError::Unavailable(
                    "Daytona sandbox did not start before its deadline".into(),
                ));
            }
            tokio::time::sleep(DAYTONA_POLL_INTERVAL).await;
            let response = self
                .client
                .get(self.api_url(&format!("/sandbox/{}", sandbox.id)))
                .bearer_auth(self.credential.as_str())
                .send()
                .await
                .map_err(|_| CodeExecutionError::Unavailable("could not reach Daytona".into()))?;
            if response.status() == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            sandbox = self.decode_sandbox(response).await?;
        }
    }

    async fn run_daytona_command(
        &self,
        session: &RemoteSession,
        request: &CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, RemoteSessionError> {
        validate_sandbox_id(&session.sandbox_id)?;
        let endpoint = session.endpoint.as_deref().ok_or_else(|| {
            CodeExecutionError::Unavailable("Daytona toolbox endpoint is unavailable".into())
        })?;
        let url = toolbox_url(
            endpoint,
            &session.sandbox_id,
            self.endpoints.allow_insecure_toolbox,
            "process/execute",
        )?;
        let started = Instant::now();
        let response = self
            .client
            .post(url)
            .bearer_auth(self.credential.as_str())
            .json(&ExecuteRequest {
                command: direct_command(request),
                cwd: &request.cwd,
                timeout: timeout_seconds(self.timeout),
            })
            .send()
            .await
            .map_err(|_| CodeExecutionError::AmbiguousExecution)?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::GONE | StatusCode::BAD_GATEWAY
        ) {
            return Err(RemoteSessionError::Missing);
        }
        if response.status() == StatusCode::REQUEST_TIMEOUT {
            return Ok(Capture::default().response(
                CodeExecutionProviderKind::Daytona,
                started,
                None,
                true,
            ));
        }
        if !response.status().is_success() {
            let status = response.status();
            let error = decode_bounded_json::<DaytonaErrorResponse>(
                response,
                "Daytona",
                MAX_DAYTONA_RESPONSE_BYTES,
            )
            .await
            .ok();
            if error
                .and_then(|body| body.code)
                .is_some_and(|code| code == "PROCESS_EXECUTION_TIMEOUT")
            {
                return Ok(Capture::default().response(
                    CodeExecutionProviderKind::Daytona,
                    started,
                    None,
                    true,
                ));
            }
            return Err(provider_status_error(status).into());
        }
        let body =
            decode_bounded_json::<ExecuteResponse>(response, "Daytona", MAX_DAYTONA_RESPONSE_BYTES)
                .await?;
        let mut capture = Capture::default();
        capture.append(body.result.as_bytes(), StreamKind::Stdout);
        Ok(capture.response(
            CodeExecutionProviderKind::Daytona,
            started,
            body.exit_code,
            false,
        ))
    }

    fn toolbox_file_url(
        &self,
        session: &RemoteSession,
        suffix: &str,
    ) -> Result<Url, CodeExecutionError> {
        let endpoint = session.endpoint.as_deref().ok_or_else(|| {
            CodeExecutionError::Unavailable("Daytona toolbox endpoint is unavailable".into())
        })?;
        toolbox_url(
            endpoint,
            &session.sandbox_id,
            self.endpoints.allow_insecure_toolbox,
            suffix,
        )
    }

    async fn decode_sandbox(
        &self,
        response: Response,
    ) -> Result<DaytonaSandbox, CodeExecutionError> {
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()));
        }
        let sandbox: DaytonaSandbox =
            decode_bounded_json(response, "Daytona", MAX_DAYTONA_RESPONSE_BYTES).await?;
        validate_sandbox_id(&sandbox.id)?;
        Ok(sandbox)
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{}", self.endpoints.api_base.trim_end_matches('/'), path)
    }
}

#[async_trait]
impl RemoteSandboxAdapter for DaytonaExecutionProvider {
    fn kind(&self) -> CodeExecutionProviderKind {
        CodeExecutionProviderKind::Daytona
    }

    fn credential_fingerprint(&self) -> [u8; 32] {
        self.credential.fingerprint()
    }

    async fn create_session(
        &self,
        workspace_id: &str,
    ) -> Result<RemoteSession, CodeExecutionError> {
        self.create_sandbox(workspace_id).await
    }

    async fn reconnect_session(
        &self,
        session: &RemoteSession,
    ) -> Result<Option<RemoteSession>, CodeExecutionError> {
        self.reconnect_sandbox(&session.sandbox_id).await
    }

    async fn run_command(
        &self,
        session: &RemoteSession,
        request: &CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, RemoteSessionError> {
        self.run_daytona_command(session, request).await
    }
}

#[async_trait]
impl RemoteWorkspaceAdapter for DaytonaExecutionProvider {
    async fn upload_file(
        &self,
        session: &RemoteSession,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), RemoteSessionError> {
        let url = self.toolbox_file_url(session, "files/upload")?;
        let multipart = multipart_file(path.file_name(), content);
        let response = self
            .client
            .post(url)
            .bearer_auth(self.credential.as_str())
            .query(&[("path", path.as_str())])
            .header("Content-Type", multipart.content_type)
            .body(multipart.body)
            .send()
            .await
            .map_err(|_| CodeExecutionError::Unavailable("could not reach Daytona".into()))?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::GONE | StatusCode::BAD_GATEWAY
        ) {
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
        let url = self.toolbox_file_url(session, "files/download")?;
        let response = self
            .client
            .get(url)
            .bearer_auth(self.credential.as_str())
            .query(&[("path", path.as_str())])
            .send()
            .await
            .map_err(|_| CodeExecutionError::Unavailable("could not reach Daytona".into()))?;
        // The session was reconciled immediately before this call, so a 404
        // here is the file rather than the sandbox.
        if response.status() == StatusCode::NOT_FOUND {
            return Err(CodeExecutionError::WorkspaceFileNotFound.into());
        }
        if matches!(
            response.status(),
            StatusCode::GONE | StatusCode::BAD_GATEWAY
        ) {
            return Err(RemoteSessionError::Missing);
        }
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()).into());
        }
        download_bounded_file(response, "Daytona", MAX_WORKSPACE_FILE_BYTES)
            .await
            .map_err(RemoteSessionError::Provider)
    }

    async fn list_directory(
        &self,
        session: &RemoteSession,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, RemoteSessionError> {
        let url = self.toolbox_file_url(session, "files")?;
        let listed = path.map_or(".", WorkspaceFilePath::as_str);
        let response = self
            .client
            .get(url)
            .bearer_auth(self.credential.as_str())
            .query(&[("path", listed)])
            .send()
            .await
            .map_err(|_| CodeExecutionError::Unavailable("could not reach Daytona".into()))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(CodeExecutionError::WorkspaceFileNotFound.into());
        }
        if matches!(
            response.status(),
            StatusCode::GONE | StatusCode::BAD_GATEWAY
        ) {
            return Err(RemoteSessionError::Missing);
        }
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()).into());
        }
        let files = decode_bounded_json::<Vec<DaytonaFileInfo>>(
            response,
            "Daytona",
            MAX_DAYTONA_RESPONSE_BYTES,
        )
        .await
        .map_err(RemoteSessionError::Provider)?;
        let mut entries = Vec::new();
        for file in files {
            if file.name.is_empty() {
                continue;
            }
            let relative = match path {
                None => file.name,
                Some(path) => format!("{}/{}", path.as_str(), file.name),
            };
            entries.push(WorkspaceFileEntry {
                path: relative,
                directory: file.is_dir,
                size_bytes: (!file.is_dir).then_some(file.size).flatten(),
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let truncated = entries.len() > MAX_WORKSPACE_LIST_ENTRIES;
        entries.truncate(MAX_WORKSPACE_LIST_ENTRIES);
        Ok(WorkspaceListing { entries, truncated })
    }

    async fn destroy_sandbox(&self, session: &RemoteSession) -> Result<(), CodeExecutionError> {
        validate_sandbox_id(&session.sandbox_id)?;
        let response = self
            .client
            .delete(self.api_url(&format!("/sandbox/{}", session.sandbox_id)))
            .bearer_auth(self.credential.as_str())
            .send()
            .await
            .map_err(|_| CodeExecutionError::Unavailable("could not reach Daytona".into()))?;
        if response.status() == StatusCode::NOT_FOUND || response.status().is_success() {
            return Ok(());
        }
        Err(provider_status_error(response.status()))
    }
}

#[async_trait]
impl WorkspaceLifecycle for DaytonaExecutionProvider {
    async fn create_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError> {
        create_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn connect_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<bool, CodeExecutionError> {
        connect_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn destroy_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError> {
        destroy_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), CodeExecutionError> {
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(CodeExecutionError::WorkspaceFileTooLarge);
        }
        with_remote_session(
            self,
            &self.pool,
            workspace.as_str(),
            |adapter, session| async move { adapter.upload_file(&session, path, content).await },
        )
        .await
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, CodeExecutionError> {
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
    ) -> Result<WorkspaceListing, CodeExecutionError> {
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
impl CodeExecutionProvider for DaytonaExecutionProvider {
    async fn execute(
        &self,
        request: CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError> {
        execute_remote(self, &self.pool, request).await
    }

    fn workspace_lifecycle(&self) -> Option<&dyn WorkspaceLifecycle> {
        Some(self)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSandboxRequest<'a> {
    labels: HashMap<&'static str, &'a str>,
    #[serde(flatten)]
    network: DaytonaNetworkSettings,
    auto_stop_interval: u32,
    auto_delete_interval: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DaytonaNetworkSettings {
    network_block_all: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_allow_list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain_allow_list: Option<String>,
}

/// Compile the host policy into Daytona's creation-time network controls. No
/// policy keeps today's disclosed open-egress sandbox. Block-all and the
/// empty allowlist use the block-all switch; a non-empty allowlist becomes
/// Daytona's comma-separated CIDR and wildcard-domain allowlists, which deny
/// every other destination the vendor's exception list does not keep open.
///
/// A non-empty allowlist emits *both* the CIDR and the domain field, present
/// but empty when that axis has no entries, rather than omitting the empty
/// one. This mirrors E2B's explicit `allow_internet_access: false` baseline:
/// a domain-only policy must still deny raw-IP egress, and an omitted CIDR
/// field could read as "no restriction on that axis" and leave IP egress
/// fully open — defeating deny-by-default. Present-but-empty is intended to
/// mean "allow nothing on this axis."
///
/// Vendor semantics of an empty-but-present allowlist need live confirmation
/// against Daytona before this surface is trusted as an enforcement boundary;
/// the default (no policy → open egress) is not changed here.
fn daytona_network_settings(policy: Option<&EgressPolicy>) -> DaytonaNetworkSettings {
    let open = DaytonaNetworkSettings {
        network_block_all: false,
        network_allow_list: None,
        domain_allow_list: None,
    };
    match policy {
        None => open,
        Some(EgressPolicy::BlockAll) => DaytonaNetworkSettings {
            network_block_all: true,
            ..open
        },
        Some(EgressPolicy::Allowlist(allowlist)) => {
            if allowlist.is_empty() {
                return DaytonaNetworkSettings {
                    network_block_all: true,
                    ..open
                };
            }
            DaytonaNetworkSettings {
                network_block_all: false,
                network_allow_list: Some(comma_joined(allowlist.cidrs())),
                domain_allow_list: Some(comma_joined(allowlist.domains())),
            }
        }
    }
}

fn comma_joined<T: ToString>(entries: &[T]) -> String {
    entries
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaytonaSandbox {
    id: String,
    state: Option<String>,
    toolbox_proxy_url: String,
}

impl DaytonaSandbox {
    fn into_session(self, allow_insecure: bool) -> Result<RemoteSession, CodeExecutionError> {
        validate_toolbox_base(&self.toolbox_proxy_url, allow_insecure)?;
        Ok(RemoteSession {
            sandbox_id: self.id,
            endpoint: Some(self.toolbox_proxy_url),
            access_token: None,
        })
    }
}

#[derive(Serialize)]
struct ExecuteRequest<'a> {
    command: String,
    cwd: &'a str,
    timeout: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteResponse {
    exit_code: Option<i32>,
    result: String,
}

#[derive(Deserialize)]
struct DaytonaErrorResponse {
    code: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaytonaFileInfo {
    #[serde(default)]
    name: String,
    #[serde(default)]
    is_dir: bool,
    #[serde(default)]
    size: Option<u64>,
}

/// Preserve the provider-neutral direct argv contract over Daytona's shell-text
/// transport. Quoting every element keeps metacharacters as argument data; the
/// shell is only the protocol bridge and is replaced by the requested process.
fn direct_command(request: &CodeExecutionRequest) -> String {
    std::iter::once(request.command.as_str())
        .chain(request.arguments.iter().map(String::as_str))
        .map(shell_quote)
        .fold(String::from("exec"), |mut command, argument| {
            command.push(' ');
            command.push_str(&argument);
            command
        })
}

fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for (index, part) in value.split('\'').enumerate() {
        if index > 0 {
            quoted.push_str("'\"'\"'");
        }
        quoted.push_str(part);
    }
    quoted.push('\'');
    quoted
}

fn timeout_seconds(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_millis().saturating_add(999) / 1_000)
        .unwrap_or(u32::MAX)
        .max(1)
}

fn toolbox_url(
    endpoint: &str,
    sandbox_id: &str,
    allow_insecure: bool,
    suffix: &str,
) -> Result<Url, CodeExecutionError> {
    validate_sandbox_id(sandbox_id)?;
    let mut url = validate_toolbox_base(endpoint, allow_insecure)?;
    let path = format!("{}/{sandbox_id}/{suffix}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

fn validate_toolbox_base(endpoint: &str, allow_insecure: bool) -> Result<Url, CodeExecutionError> {
    let url = Url::parse(endpoint).map_err(|_| {
        CodeExecutionError::Unavailable("Daytona returned an invalid toolbox endpoint".into())
    })?;
    let host = url.host_str().unwrap_or_default();
    let cloud_endpoint =
        url.scheme() == "https" && (host == "daytona.io" || host.ends_with(".daytona.io"));
    let test_endpoint = allow_insecure
        && url.scheme() == "http"
        && matches!(host, "127.0.0.1" | "localhost" | "::1");
    if (!cloud_endpoint && !test_endpoint)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CodeExecutionError::Unavailable(
            "Daytona returned an invalid toolbox endpoint".into(),
        ));
    }
    Ok(url)
}

fn validate_sandbox_id(value: &str) -> Result<(), CodeExecutionError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CodeExecutionError::Unavailable(
            "Daytona returned an invalid sandbox identity".into(),
        ));
    }
    Ok(())
}

fn provider_status_error(status: StatusCode) -> CodeExecutionError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            CodeExecutionError::Unavailable("Daytona credential was rejected".into())
        }
        StatusCode::TOO_MANY_REQUESTS => {
            CodeExecutionError::Unavailable("Daytona rate limit exceeded".into())
        }
        _ => CodeExecutionError::Unavailable(format!(
            "Daytona request failed with status {}",
            status.as_u16()
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use axum::body::Bytes;
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::{json, Value};

    use super::*;
    use crate::{ExecutionId, ExecutionWorkspaceId};

    #[derive(Clone, Default)]
    struct MockState {
        base: Arc<OnceLock<String>>,
        requests: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
        files: Arc<Mutex<StdHashMap<String, Vec<u8>>>>,
    }

    async fn spawn_mock() -> (String, MockState, tokio::task::JoinHandle<()>) {
        let state = MockState::default();
        let app = Router::new()
            .route("/api/sandbox/{id}", get(get_sandbox).delete(delete_sandbox))
            .route("/api/sandbox", post(create_sandbox))
            .route("/toolbox/{id}/process/execute", post(execute))
            .route("/toolbox/{id}/files", get(list_files))
            .route("/toolbox/{id}/files/upload", post(upload_file))
            .route("/toolbox/{id}/files/download", get(download_file))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        state
            .base
            .set(format!("http://{address}"))
            .expect("mock base is set once");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), state, server)
    }

    async fn create_sandbox(
        State(state): State<MockState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        state
            .requests
            .lock()
            .unwrap()
            .push(("create".into(), headers, body));
        Json(json!({
            "id": "sandbox-123",
            "state": "started",
            "toolboxProxyUrl": format!("{}/toolbox", state.base.get().unwrap()),
        }))
    }

    async fn get_sandbox(
        State(state): State<MockState>,
        Path(id): Path<String>,
        headers: HeaderMap,
    ) -> Json<Value> {
        state
            .requests
            .lock()
            .unwrap()
            .push(("get".into(), headers, json!({"id": id})));
        Json(json!({
            "id": "sandbox-123",
            "state": "started",
            "toolboxProxyUrl": format!("{}/toolbox", state.base.get().unwrap()),
        }))
    }

    async fn execute(
        State(state): State<MockState>,
        Path(id): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let timed_out = body["command"] == "exec 'slow'";
        state.requests.lock().unwrap().push((
            "execute".into(),
            headers,
            json!({"id": id, "request": body}),
        ));
        if timed_out {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "code": "PROCESS_EXECUTION_TIMEOUT",
                    "message": "command exceeded its timeout",
                })),
            )
        } else {
            (
                StatusCode::OK,
                Json(json!({"exitCode": 0, "result": "ok\n"})),
            )
        }
    }

    async fn delete_sandbox(
        State(state): State<MockState>,
        Path(id): Path<String>,
        headers: HeaderMap,
    ) -> StatusCode {
        state
            .requests
            .lock()
            .unwrap()
            .push(("delete".into(), headers, json!({"id": id})));
        StatusCode::OK
    }

    async fn upload_file(
        State(state): State<MockState>,
        Path(id): Path<String>,
        Query(query): Query<StdHashMap<String, String>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        assert_eq!(id, "sandbox-123");
        let content = multipart_content(&headers, &body);
        state.requests.lock().unwrap().push((
            "upload".into(),
            headers,
            json!({"path": query["path"]}),
        ));
        state
            .files
            .lock()
            .unwrap()
            .insert(query["path"].clone(), content);
        StatusCode::OK
    }

    async fn download_file(
        State(state): State<MockState>,
        Path(id): Path<String>,
        Query(query): Query<StdHashMap<String, String>>,
    ) -> Result<Vec<u8>, StatusCode> {
        assert_eq!(id, "sandbox-123");
        state
            .files
            .lock()
            .unwrap()
            .get(&query["path"])
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)
    }

    async fn list_files(
        State(state): State<MockState>,
        Path(id): Path<String>,
        Query(query): Query<StdHashMap<String, String>>,
    ) -> Json<Value> {
        assert_eq!(id, "sandbox-123");
        assert_eq!(query["path"], ".");
        let files = state.files.lock().unwrap();
        let entries: Vec<Value> = files
            .iter()
            .map(|(path, content)| {
                json!({
                    "name": path.rsplit('/').next().unwrap(),
                    "isDir": false,
                    "size": content.len(),
                    "modTime": "2026-07-28T00:00:00Z",
                })
            })
            .collect();
        Json(json!(entries))
    }

    fn multipart_content(headers: &HeaderMap, body: &[u8]) -> Vec<u8> {
        let content_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap();
        let boundary = content_type.split("boundary=").nth(1).unwrap().to_owned();
        let start = body.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let tail = format!("\r\n--{boundary}--\r\n");
        assert!(body.ends_with(tail.as_bytes()));
        body[start..body.len() - tail.len()].to_vec()
    }

    fn request(execution_id: &str) -> CodeExecutionRequest {
        CodeExecutionRequest::new(
            ExecutionId::parse(execution_id).unwrap(),
            ExecutionWorkspaceId::parse("chat-123").unwrap(),
            "printf",
            vec![
                "%s".into(),
                "a; touch /tmp/not-openwave".into(),
                "single'quote".into(),
            ],
            ".",
        )
        .unwrap()
    }

    fn timeout_request() -> CodeExecutionRequest {
        CodeExecutionRequest::new(
            ExecutionId::parse("call-timeout").unwrap(),
            ExecutionWorkspaceId::parse("chat-timeout").unwrap(),
            "slow",
            Vec::new(),
            ".",
        )
        .unwrap()
    }

    #[test]
    fn direct_argv_is_quoted_before_crossing_daytonas_shell_api() {
        assert_eq!(
            direct_command(&request("call-123")),
            "exec 'printf' '%s' 'a; touch /tmp/not-openwave' 'single'\"'\"'quote'"
        );
    }

    #[tokio::test]
    async fn daytona_reuses_the_chat_sandbox_and_replays_an_exact_execution() {
        let (base, state, server) = spawn_mock().await;
        let provider = DaytonaExecutionProvider::with_endpoints(
            DaytonaCredential::parse("test-daytona-key").unwrap(),
            Duration::from_secs(5),
            RemoteSessionPool::default(),
            DaytonaEndpoints {
                api_base: format!("{base}/api"),
                allow_insecure_toolbox: true,
            },
        )
        .unwrap();

        let first = provider.execute(request("call-123")).await.unwrap();
        let second = provider.execute(request("call-456")).await.unwrap();
        let replay = provider.execute(request("call-456")).await.unwrap();
        assert_eq!(first.provider, CodeExecutionProviderKind::Daytona);
        assert_eq!(first.stdout, "ok\n");
        assert_eq!(second, replay);

        let requests = state.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(path, _, _)| path == "create")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(path, _, _)| path == "execute")
                .count(),
            2
        );
        assert_eq!(
            requests.iter().filter(|(path, _, _)| path == "get").count(),
            1
        );
        for (_, headers, _) in requests
            .iter()
            .filter(|(path, _, _)| matches!(path.as_str(), "create" | "get" | "execute"))
        {
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer test-daytona-key")
            );
        }
        let create_body = requests
            .iter()
            .find(|(path, _, _)| path == "create")
            .map(|(_, _, body)| body)
            .unwrap();
        assert_eq!(create_body["networkBlockAll"], false);
        assert!(create_body.get("networkAllowList").is_none());
        assert!(create_body.get("domainAllowList").is_none());
        assert_eq!(create_body["autoStopInterval"], DAYTONA_IDLE_MINUTES);
        assert_eq!(create_body["autoDeleteInterval"], 0);
        assert_eq!(create_body["labels"]["openwave_workspace_id"], "chat-123");
        let execute_body = requests
            .iter()
            .find(|(path, _, _)| path == "execute")
            .map(|(_, _, body)| &body["request"])
            .unwrap();
        assert_eq!(
            execute_body["command"],
            "exec 'printf' '%s' 'a; touch /tmp/not-openwave' 'single'\"'\"'quote'"
        );
        assert_eq!(execute_body["cwd"], ".");
        assert_eq!(execute_body["timeout"], 5);
        server.abort();
    }

    #[test]
    fn daytona_compiles_the_egress_policy_into_creation_network_fields() {
        use openwave_egress::{CidrBlock, DomainPattern, EgressAllowlist};

        let allowlist = |domains: &[&str], cidrs: &[&str]| {
            EgressPolicy::Allowlist(EgressAllowlist::new(
                domains
                    .iter()
                    .map(|pattern| DomainPattern::parse(pattern).unwrap())
                    .collect(),
                cidrs
                    .iter()
                    .map(|block| CidrBlock::parse(block).unwrap())
                    .collect(),
            ))
        };

        // The wire shape Daytona receives, pinned through the same serde
        // struct the creation request embeds.
        let policy = allowlist(&["*.pypi.org", "crates.io"], &["140.82.112.0/20"]);
        let body = serde_json::to_value(daytona_network_settings(Some(&policy))).unwrap();
        assert_eq!(body["networkBlockAll"], false);
        assert_eq!(body["networkAllowList"], "140.82.112.0/20");
        assert_eq!(body["domainAllowList"], "*.pypi.org,crates.io");

        // A domain-only policy still expresses a deny-all baseline for raw-IP
        // egress: the CIDR field is present but empty, never omitted, so an
        // absent axis can't read as "no IP restriction".
        let domain_only = allowlist(&["*.pypi.org"], &[]);
        let body = serde_json::to_value(daytona_network_settings(Some(&domain_only))).unwrap();
        assert_eq!(body["networkBlockAll"], false);
        assert_eq!(body["networkAllowList"], "");
        assert_eq!(body["domainAllowList"], "*.pypi.org");

        // Symmetrically, a CIDR-only policy still denies every domain.
        let cidr_only = allowlist(&[], &["140.82.112.0/20"]);
        let body = serde_json::to_value(daytona_network_settings(Some(&cidr_only))).unwrap();
        assert_eq!(body["networkAllowList"], "140.82.112.0/20");
        assert_eq!(body["domainAllowList"], "");

        // Block-all and the empty allowlist both fail closed on the switch.
        for policy in [EgressPolicy::BlockAll, allowlist(&[], &[])] {
            let body = serde_json::to_value(daytona_network_settings(Some(&policy))).unwrap();
            assert_eq!(body, json!({ "networkBlockAll": true }));
        }

        // Vendor entry limits fail before any sandbox is created.
        let provider = || {
            DaytonaExecutionProvider::new(
                DaytonaCredential::parse("test-daytona-key").unwrap(),
                Duration::from_secs(5),
            )
            .unwrap()
        };
        let cidrs: Vec<String> = (0..11).map(|index| format!("10.0.{index}.0/24")).collect();
        let cidr_refs: Vec<&str> = cidrs.iter().map(String::as_str).collect();
        assert!(matches!(
            provider().with_egress_policy(allowlist(&[], &cidr_refs)),
            Err(CodeExecutionError::InvalidRequest(_))
        ));
        let domains: Vec<String> = (0..21)
            .map(|index| format!("host{index}.example.com"))
            .collect();
        let domain_refs: Vec<&str> = domains.iter().map(String::as_str).collect();
        assert!(matches!(
            provider().with_egress_policy(allowlist(&domain_refs, &[])),
            Err(CodeExecutionError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn daytona_workspace_lifecycle_round_trips_files_over_the_toolbox_api() {
        let (base, state, server) = spawn_mock().await;
        let provider = DaytonaExecutionProvider::with_endpoints(
            DaytonaCredential::parse("test-daytona-key").unwrap(),
            Duration::from_secs(5),
            RemoteSessionPool::default(),
            DaytonaEndpoints {
                api_base: format!("{base}/api"),
                allow_insecure_toolbox: true,
            },
        )
        .unwrap();
        let workspace = ExecutionWorkspaceId::parse("chat-files").unwrap();

        assert!(!provider.connect_workspace(&workspace).await.unwrap());
        provider.create_workspace(&workspace).await.unwrap();
        assert!(provider.connect_workspace(&workspace).await.unwrap());

        let path = WorkspaceFilePath::parse("data/report.bin").unwrap();
        let content = b"\x00daytona\xff".to_vec();
        provider
            .put_workspace_file(&workspace, &path, &content)
            .await
            .unwrap();
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
            Err(CodeExecutionError::WorkspaceFileNotFound)
        ));

        let listing = provider
            .list_workspace_files(&workspace, None)
            .await
            .unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].path, "report.bin");
        assert_eq!(listing.entries[0].size_bytes, Some(content.len() as u64));

        provider.destroy_workspace(&workspace).await.unwrap();
        assert!(!provider.connect_workspace(&workspace).await.unwrap());

        let requests = state.requests.lock().unwrap();
        let upload = requests
            .iter()
            .find(|(kind, _, _)| kind == "upload")
            .map(|(_, headers, body)| (headers.clone(), body.clone()))
            .unwrap();
        assert_eq!(upload.1["path"], "data/report.bin");
        assert_eq!(
            upload
                .0
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-daytona-key")
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(kind, _, _)| kind == "delete")
                .count(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn daytona_normalizes_the_toolbox_timeout_error() {
        let (base, _state, server) = spawn_mock().await;
        let provider = DaytonaExecutionProvider::with_endpoints(
            DaytonaCredential::parse("test-daytona-key").unwrap(),
            Duration::from_secs(5),
            RemoteSessionPool::default(),
            DaytonaEndpoints {
                api_base: format!("{base}/api"),
                allow_insecure_toolbox: true,
            },
        )
        .unwrap();

        let response = provider.execute(timeout_request()).await.unwrap();
        assert!(response.timed_out);
        assert_eq!(response.exit_code, None);
        server.abort();
    }
}
