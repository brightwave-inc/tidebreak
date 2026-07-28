use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use openwave_core::SecretProvider;
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};

use crate::credential::SecretCredential;
use crate::http::decode_bounded_json;
use crate::output::{Capture, StreamKind};
use crate::remote::{
    execute_remote, RemoteSandboxAdapter, RemoteSession, RemoteSessionError, RemoteSessionPool,
};
use crate::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse,
};

const DAYTONA_API_BASE: &str = "https://app.daytona.io/api";
const DAYTONA_IDLE_MINUTES: u32 = 5;
const DAYTONA_START_TIMEOUT: Duration = Duration::from_secs(60);
const DAYTONA_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DAYTONA_TRANSPORT_GRACE: Duration = Duration::from_secs(10);
const MAX_DAYTONA_RESPONSE_BYTES: usize = 1024 * 1024;

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
        })
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
                network_block_all: false,
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
        let url = toolbox_execute_url(
            endpoint,
            &session.sandbox_id,
            self.endpoints.allow_insecure_toolbox,
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
impl CodeExecutionProvider for DaytonaExecutionProvider {
    async fn execute(
        &self,
        request: CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError> {
        execute_remote(self, &self.pool, request).await
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSandboxRequest<'a> {
    labels: HashMap<&'static str, &'a str>,
    network_block_all: bool,
    auto_stop_interval: u32,
    auto_delete_interval: u32,
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

fn toolbox_execute_url(
    endpoint: &str,
    sandbox_id: &str,
    allow_insecure: bool,
) -> Result<Url, CodeExecutionError> {
    validate_sandbox_id(sandbox_id)?;
    let mut url = validate_toolbox_base(endpoint, allow_insecure)?;
    let path = format!(
        "{}/{sandbox_id}/process/execute",
        url.path().trim_end_matches('/')
    );
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
    use std::sync::{Arc, Mutex, OnceLock};

    use axum::extract::{Path, State};
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
    }

    async fn spawn_mock() -> (String, MockState, tokio::task::JoinHandle<()>) {
        let state = MockState::default();
        let app = Router::new()
            .route("/api/sandbox", post(create_sandbox))
            .route("/api/sandbox/{id}", get(get_sandbox))
            .route("/toolbox/{id}/process/execute", post(execute))
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
