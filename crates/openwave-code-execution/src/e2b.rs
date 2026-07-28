use std::collections::HashMap;
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt as _;
use openwave_core::SecretProvider;
use reqwest::{Client, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse, MAX_CAPTURE_BYTES,
};

const E2B_API_BASE: &str = "https://api.e2b.app";
const E2B_SANDBOX_BASE: &str = "https://sandbox.e2b.app";
const E2B_TEMPLATE: &str = "code-interpreter-v1";
const E2B_WORKSPACE_ROOT: &str = "/home/user";
const E2B_ENVD_PORT: &str = "49983";
const E2B_SANDBOX_TTL_SECONDS: u64 = 300;
const E2B_TRANSPORT_GRACE: Duration = Duration::from_secs(10);
const MAX_MANAGEMENT_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CONNECT_FRAME_BYTES: usize = 256 * 1024;
const CONNECT_END_STREAM_FLAG: u8 = 0b0000_0010;
const CONNECT_COMPRESSED_FLAG: u8 = 0b0000_0001;

/// Fixed key for E2B credentials in OpenWave's host secret store.
pub const E2B_CREDENTIAL_KEY: &str = "code_execution.e2b.api_key";

/// Non-serializable, redacted E2B API credential.
#[derive(Clone)]
pub struct E2BCredential(String);

impl std::fmt::Debug for E2BCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("E2BCredential")
            .field(&"***")
            .finish()
    }
}

impl E2BCredential {
    pub fn parse(value: impl Into<String>) -> Result<Self, CodeExecutionError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CodeExecutionError::InvalidRequest(
                "E2B API key must not be empty".into(),
            ));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Resolve the credential without exposing it through serializable config.
    pub async fn load(secrets: &dyn SecretProvider) -> Result<Option<Self>, CodeExecutionError> {
        let value = secrets.get_secret(E2B_CREDENTIAL_KEY).await.map_err(|_| {
            CodeExecutionError::Unavailable("E2B credential storage is unavailable".into())
        })?;
        value
            .filter(|value| !value.trim().is_empty())
            .map(Self::parse)
            .transpose()
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }
}

/// Shared process-local E2B sessions and idempotency receipts.
///
/// A configured host keeps one pool for its lifetime, while individual adapter
/// values can still be rebuilt when timeout policy or credentials change.
#[derive(Clone, Default)]
pub struct E2BSessionPool {
    state: Arc<Mutex<PoolState>>,
}

#[derive(Default)]
struct PoolState {
    sessions: HashMap<SessionKey, Arc<Mutex<Option<E2BSession>>>>,
    receipts: HashMap<String, ExecutionReceipt>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    credential: [u8; 32],
    workspace_id: String,
}

#[derive(Clone)]
struct E2BSession {
    sandbox_id: String,
    access_token: String,
}

#[derive(Clone)]
enum ExecutionReceipt {
    Running {
        fingerprint: String,
    },
    Completed {
        fingerprint: String,
        response: CodeExecutionResponse,
    },
    Failed {
        fingerprint: String,
        message: String,
    },
}

enum BeginExecution {
    Started,
    Cached(CodeExecutionResponse),
}

impl E2BSessionPool {
    async fn begin_execution(
        &self,
        request: &CodeExecutionRequest,
    ) -> Result<BeginExecution, CodeExecutionError> {
        let fingerprint = request_fingerprint(request)?;
        let mut state = self.state.lock().await;
        match state.receipts.get(request.execution_id.as_str()) {
            None => {
                state.receipts.insert(
                    request.execution_id.as_str().to_owned(),
                    ExecutionReceipt::Running { fingerprint },
                );
                Ok(BeginExecution::Started)
            }
            Some(ExecutionReceipt::Running {
                fingerprint: existing,
            }) => {
                ensure_same_fingerprint(existing, &fingerprint)?;
                Err(CodeExecutionError::AmbiguousExecution)
            }
            Some(ExecutionReceipt::Completed {
                fingerprint: existing,
                response,
            }) => {
                ensure_same_fingerprint(existing, &fingerprint)?;
                Ok(BeginExecution::Cached(response.clone()))
            }
            Some(ExecutionReceipt::Failed {
                fingerprint: existing,
                message,
            }) => {
                ensure_same_fingerprint(existing, &fingerprint)?;
                Err(CodeExecutionError::Unavailable(message.clone()))
            }
        }
    }

    async fn finish_execution(
        &self,
        request: &CodeExecutionRequest,
        outcome: &Result<CodeExecutionResponse, CodeExecutionError>,
    ) -> Result<(), CodeExecutionError> {
        let fingerprint = request_fingerprint(request)?;
        let receipt = match outcome {
            Ok(response) => ExecutionReceipt::Completed {
                fingerprint,
                response: response.clone(),
            },
            Err(error) => ExecutionReceipt::Failed {
                fingerprint,
                message: error.to_string(),
            },
        };
        self.state
            .lock()
            .await
            .receipts
            .insert(request.execution_id.as_str().to_owned(), receipt);
        Ok(())
    }

    async fn session(
        &self,
        credential: &E2BCredential,
        workspace_id: &str,
    ) -> Arc<Mutex<Option<E2BSession>>> {
        let key = SessionKey {
            credential: credential.fingerprint(),
            workspace_id: workspace_id.to_owned(),
        };
        let mut state = self.state.lock().await;
        state
            .sessions
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    }
}

/// Direct-command adapter for E2B's managed sandbox service.
pub struct E2BExecutionProvider {
    credential: E2BCredential,
    timeout: Duration,
    pool: E2BSessionPool,
    client: Client,
    endpoints: E2BEndpoints,
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
    pub fn new(credential: E2BCredential, timeout: Duration) -> Result<Self, CodeExecutionError> {
        Self::with_session_pool(credential, timeout, E2BSessionPool::default())
    }

    pub fn with_session_pool(
        credential: E2BCredential,
        timeout: Duration,
        pool: E2BSessionPool,
    ) -> Result<Self, CodeExecutionError> {
        Self::with_endpoints(credential, timeout, pool, E2BEndpoints::default())
    }

    fn with_endpoints(
        credential: E2BCredential,
        timeout: Duration,
        pool: E2BSessionPool,
        endpoints: E2BEndpoints,
    ) -> Result<Self, CodeExecutionError> {
        if timeout.is_zero() {
            return Err(CodeExecutionError::InvalidRequest(
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
            .map_err(|_| {
                CodeExecutionError::Unavailable("could not configure E2B transport".into())
            })?;
        Ok(Self {
            credential,
            timeout,
            pool,
            client,
            endpoints,
        })
    }

    async fn execute_uncached(
        &self,
        request: &CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError> {
        let session = self
            .pool
            .session(&self.credential, request.workspace_id.as_str())
            .await;
        // Commands for one chat are serialized so workspace mutations have a
        // deterministic order and session replacement cannot race execution.
        let mut session = session.lock().await;
        let active = match session.as_ref() {
            Some(existing) => match self.connect_sandbox(&existing.sandbox_id).await {
                Ok(connected) => connected,
                Err(ManagementError::NotFound) => {
                    self.create_sandbox(request.workspace_id.as_str()).await?
                }
                Err(ManagementError::Provider(error)) => return Err(error),
            },
            None => self.create_sandbox(request.workspace_id.as_str()).await?,
        };
        *session = Some(active.clone());

        let result = self.run_command(&active, request).await;
        if matches!(
            result,
            Err(CodeExecutionError::Unavailable(ref message))
                if message == "E2B sandbox is no longer available"
        ) {
            *session = None;
        }
        result
    }

    async fn create_sandbox(&self, workspace_id: &str) -> Result<E2BSession, CodeExecutionError> {
        let url = format!(
            "{}/sandboxes",
            self.endpoints.api_base.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .header("X-API-Key", self.credential.as_str())
            .json(&CreateSandboxRequest {
                template_id: E2B_TEMPLATE,
                timeout: E2B_SANDBOX_TTL_SECONDS,
                secure: true,
                allow_internet_access: true,
                metadata: HashMap::from([("openwave_workspace_id", workspace_id)]),
            })
            .send()
            .await
            .map_err(|_| CodeExecutionError::Unavailable("could not reach the E2B API".into()))?;
        decode_session(response).await
    }

    async fn connect_sandbox(&self, sandbox_id: &str) -> Result<E2BSession, ManagementError> {
        validate_sandbox_id(sandbox_id).map_err(ManagementError::Provider)?;
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
            .map_err(|_| {
                ManagementError::Provider(CodeExecutionError::Unavailable(
                    "could not reach the E2B API".into(),
                ))
            })?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ManagementError::NotFound);
        }
        decode_session(response)
            .await
            .map_err(ManagementError::Provider)
    }

    async fn run_command(
        &self,
        session: &E2BSession,
        request: &CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError> {
        validate_sandbox_id(&session.sandbox_id)?;
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
        .map_err(|_| {
            CodeExecutionError::InvalidRequest("E2B request is not serializable".into())
        })?;
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
            .header("X-Access-Token", &session.access_token)
            .body(body)
            .send()
            .await
            .map_err(|_| CodeExecutionError::AmbiguousExecution)?;
        if response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::BAD_GATEWAY
        {
            return Err(CodeExecutionError::Unavailable(
                "E2B sandbox is no longer available".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(provider_status_error(response.status()));
        }

        decode_command_response(response, started).await
    }
}

#[async_trait]
impl CodeExecutionProvider for E2BExecutionProvider {
    async fn execute(
        &self,
        request: CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError> {
        request.validate()?;
        match self.pool.begin_execution(&request).await? {
            BeginExecution::Cached(response) => return Ok(response),
            BeginExecution::Started => {}
        }
        let outcome = self.execute_uncached(&request).await;
        self.pool.finish_execution(&request, &outcome).await?;
        outcome
    }
}

enum ManagementError {
    NotFound,
    Provider(CodeExecutionError),
}

#[derive(Serialize)]
struct CreateSandboxRequest<'a> {
    #[serde(rename = "templateID")]
    template_id: &'static str,
    timeout: u64,
    secure: bool,
    allow_internet_access: bool,
    metadata: HashMap<&'static str, &'a str>,
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

async fn decode_session(response: Response) -> Result<E2BSession, CodeExecutionError> {
    if !response.status().is_success() {
        return Err(provider_status_error(response.status()));
    }
    let body = decode_bounded_json::<SandboxResponse>(response).await?;
    validate_sandbox_id(&body.sandbox_id)?;
    let access_token = body
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            CodeExecutionError::Unavailable("E2B did not return a secure sandbox token".into())
        })?;
    Ok(E2BSession {
        sandbox_id: body.sandbox_id,
        access_token,
    })
}

async fn decode_bounded_json<T: DeserializeOwned>(
    response: Response,
) -> Result<T, CodeExecutionError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            CodeExecutionError::Unavailable("E2B returned an incomplete response".into())
        })?;
        if bytes.len().saturating_add(chunk.len()) > MAX_MANAGEMENT_RESPONSE_BYTES {
            return Err(CodeExecutionError::Unavailable(
                "E2B returned an oversized response".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| CodeExecutionError::Unavailable("E2B returned an invalid response".into()))
}

fn provider_status_error(status: StatusCode) -> CodeExecutionError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            CodeExecutionError::Unavailable("E2B credential was rejected".into())
        }
        StatusCode::TOO_MANY_REQUESTS => {
            CodeExecutionError::Unavailable("E2B rate limit exceeded".into())
        }
        _ => CodeExecutionError::Unavailable(format!(
            "E2B request failed with status {}",
            status.as_u16()
        )),
    }
}

fn validate_sandbox_id(value: &str) -> Result<(), CodeExecutionError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CodeExecutionError::Unavailable(
            "E2B returned an invalid sandbox identity".into(),
        ));
    }
    Ok(())
}

fn remote_cwd(cwd: &str) -> Result<String, CodeExecutionError> {
    let mut remote = E2B_WORKSPACE_ROOT.to_owned();
    for component in Path::new(cwd).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    CodeExecutionError::InvalidRequest("invalid working directory".into())
                })?;
                remote.push('/');
                remote.push_str(part);
            }
            _ => {
                return Err(CodeExecutionError::InvalidRequest(
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

fn connect_envelope(flags: u8, payload: &[u8]) -> Result<Vec<u8>, CodeExecutionError> {
    let length = u32::try_from(payload.len()).map_err(|_| {
        CodeExecutionError::InvalidRequest("E2B request exceeds the protocol bound".into())
    })?;
    let mut encoded = Vec::with_capacity(payload.len() + 5);
    encoded.push(flags);
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

async fn decode_command_response(
    response: Response,
    started: Instant,
) -> Result<CodeExecutionResponse, CodeExecutionError> {
    let mut decoder = EnvelopeDecoder::default();
    let mut stream = response.bytes_stream();
    let mut capture = Capture::default();
    let mut process_end = None;
    let mut end_stream = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| CodeExecutionError::AmbiguousExecution)?;
        for envelope in decoder.decode(&chunk)? {
            if envelope.flags & CONNECT_COMPRESSED_FLAG != 0 {
                return Err(CodeExecutionError::Unavailable(
                    "E2B returned unsupported compressed output".into(),
                ));
            }
            if envelope.flags & CONNECT_END_STREAM_FLAG != 0 {
                if end_stream {
                    return Err(CodeExecutionError::Unavailable(
                        "E2B returned duplicate stream trailers".into(),
                    ));
                }
                end_stream = true;
                let end: ConnectEndStream =
                    serde_json::from_slice(&envelope.payload).map_err(|_| {
                        CodeExecutionError::Unavailable(
                            "E2B returned invalid stream trailers".into(),
                        )
                    })?;
                if let Some(error) = end.error {
                    if error.code == "deadline_exceeded" {
                        return Ok(command_response(started, capture, None, true));
                    }
                    return Err(CodeExecutionError::Unavailable(format!(
                        "E2B command stream failed: {}",
                        error.code
                    )));
                }
                continue;
            }
            if end_stream {
                return Err(CodeExecutionError::Unavailable(
                    "E2B returned output after stream trailers".into(),
                ));
            }
            let event: StartResponse = serde_json::from_slice(&envelope.payload).map_err(|_| {
                CodeExecutionError::Unavailable("E2B returned an invalid command event".into())
            })?;
            let Some(event) = event.event else {
                continue;
            };
            if let Some(data) = event.data {
                if let Some(stdout) = data.stdout {
                    capture.append_base64(&stdout, StreamKind::Stdout)?;
                }
                if let Some(stderr) = data.stderr {
                    capture.append_base64(&stderr, StreamKind::Stderr)?;
                }
            }
            if let Some(end) = event.end {
                if process_end.is_some() {
                    return Err(CodeExecutionError::Unavailable(
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
        return Err(CodeExecutionError::AmbiguousExecution);
    }
    let end = process_end.ok_or(CodeExecutionError::AmbiguousExecution)?;
    let exit_code = end.exited.then_some(end.exit_code.unwrap_or_default());
    Ok(command_response(started, capture, exit_code, false))
}

fn command_response(
    started: Instant,
    capture: Capture,
    exit_code: Option<i32>,
    timed_out: bool,
) -> CodeExecutionResponse {
    CodeExecutionResponse {
        provider: CodeExecutionProviderKind::E2b,
        exit_code,
        stdout: String::from_utf8_lossy(&capture.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&capture.stderr).into_owned(),
        timed_out,
        output_truncated: capture.truncated,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
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
    fn decode(&mut self, chunk: &[u8]) -> Result<Vec<Envelope>, CodeExecutionError> {
        if chunk.len() > MAX_CONNECT_FRAME_BYTES.saturating_mul(2) {
            return Err(CodeExecutionError::Unavailable(
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
                return Err(CodeExecutionError::Unavailable(
                    "E2B returned an oversized stream frame".into(),
                ));
            }
            let end = offset
                .checked_add(5)
                .and_then(|start| start.checked_add(length))
                .ok_or_else(|| {
                    CodeExecutionError::Unavailable("E2B returned an invalid stream frame".into())
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
            return Err(CodeExecutionError::Unavailable(
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

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Default)]
struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    total: usize,
    truncated: bool,
}

impl Capture {
    fn append_base64(&mut self, value: &str, kind: StreamKind) -> Result<(), CodeExecutionError> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|_| {
                CodeExecutionError::Unavailable("E2B returned invalid command output".into())
            })?;
        self.append(&decoded, kind);
        Ok(())
    }

    fn append(&mut self, value: &[u8], kind: StreamKind) {
        let available = MAX_CAPTURE_BYTES.saturating_sub(self.total);
        let kept = available.min(value.len());
        let target = match kind {
            StreamKind::Stdout => &mut self.stdout,
            StreamKind::Stderr => &mut self.stderr,
        };
        target.extend_from_slice(&value[..kept]);
        self.total += kept;
        self.truncated |= kept < value.len();
    }
}

fn request_fingerprint(request: &CodeExecutionRequest) -> Result<String, CodeExecutionError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|_| CodeExecutionError::InvalidRequest("request is not serializable".into()))?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ensure_same_fingerprint(existing: &str, expected: &str) -> Result<(), CodeExecutionError> {
    if existing == expected {
        Ok(())
    } else {
        Err(CodeExecutionError::IdentityConflict)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use axum::body::Bytes;
    use axum::extract::{Path as AxumPath, State};
    use axum::http::{header, HeaderMap};
    use axum::routing::post;
    use axum::{Json, Router};
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
        creates: AtomicUsize,
        connects: AtomicUsize,
        starts: AtomicUsize,
        create_body: StdMutex<Option<Value>>,
        start_bodies: StdMutex<Vec<Vec<u8>>>,
        api_keys: StdMutex<Vec<String>>,
        access_tokens: StdMutex<Vec<String>>,
    }

    impl MockState {
        fn new(mode: ProcessMode) -> Self {
            Self {
                mode,
                creates: AtomicUsize::new(0),
                connects: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                create_body: StdMutex::new(None),
                start_bodies: StdMutex::new(Vec::new()),
                api_keys: StdMutex::new(Vec::new()),
                access_tokens: StdMutex::new(Vec::new()),
            }
        }
    }

    async fn mock_provider(
        mode: ProcessMode,
    ) -> (
        E2BExecutionProvider,
        Arc<MockState>,
        tokio::task::JoinHandle<()>,
    ) {
        let state = Arc::new(MockState::new(mode));
        let app = Router::new()
            .route("/sandboxes", post(mock_create))
            .route("/sandboxes/{sandbox_id}/connect", post(mock_connect))
            .route("/process.Process/Start", post(mock_start))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = E2BExecutionProvider::with_endpoints(
            E2BCredential::parse("test-e2b-key").unwrap(),
            Duration::from_secs(2),
            E2BSessionPool::default(),
            E2BEndpoints {
                api_base: base.clone(),
                sandbox_base: base,
            },
        )
        .unwrap();
        (provider, state, server)
    }

    async fn mock_create(
        State(state): State<Arc<MockState>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        state.creates.fetch_add(1, Ordering::SeqCst);
        state
            .api_keys
            .lock()
            .unwrap()
            .push(header_value(&headers, "x-api-key"));
        *state.create_body.lock().unwrap() = Some(body);
        Json(json!({
            "sandboxID": "sandbox-123",
            "envdVersion": "0.3.0",
            "envdAccessToken": "access-123"
        }))
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

    fn request(execution: &str, workspace: &str, argument: &str) -> CodeExecutionRequest {
        CodeExecutionRequest::new(
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
        let (provider, state, server) = mock_provider(ProcessMode::Complete).await;
        let first_request = request("execution-1", "workspace-1", "first");
        let first = provider.execute(first_request.clone()).await.unwrap();
        assert_eq!(first.provider, CodeExecutionProviderKind::E2b);
        assert_eq!(first.exit_code, Some(0));
        assert_eq!(first.stdout, "ok\n");

        let replay = provider.execute(first_request.clone()).await.unwrap();
        assert_eq!(replay, first);
        assert_eq!(state.starts.load(Ordering::SeqCst), 1);

        let conflict = CodeExecutionRequest::new(
            first_request.execution_id.clone(),
            first_request.workspace_id.clone(),
            "/bin/echo",
            vec!["different".into()],
            ".",
        )
        .unwrap();
        assert!(matches!(
            provider.execute(conflict).await,
            Err(CodeExecutionError::IdentityConflict)
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
        assert_eq!(create["templateID"], E2B_TEMPLATE);
        assert_eq!(create["metadata"]["openwave_workspace_id"], "workspace-1");
        assert_eq!(create["secure"], true);
        assert_eq!(create["allow_internet_access"], true);

        let bodies = state.start_bodies.lock().unwrap();
        let first_start = decode_start_request(&bodies[0]);
        assert_eq!(first_start["process"]["cmd"], "/bin/echo");
        assert_eq!(first_start["process"]["args"], json!(["first"]));
        assert_eq!(first_start["process"]["cwd"], E2B_WORKSPACE_ROOT);
        assert_eq!(first_start["stdin"], false);
        server.abort();
    }

    #[tokio::test]
    async fn e2b_projects_connect_deadlines_to_the_bounded_timeout_result() {
        let (provider, state, server) = mock_provider(ProcessMode::Timeout).await;
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
