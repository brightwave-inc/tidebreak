use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::receipt::{request_fingerprint, BeginExecution, ExecutionReceipt};
use crate::{
    CodeExecutionError, CodeExecutionProviderKind, CodeExecutionRequest, CodeExecutionResponse,
    WorkspaceFilePath, WorkspaceListing,
};

/// Shared process-local sessions and idempotency receipts for managed sandboxes.
#[derive(Clone, Default)]
pub struct RemoteSessionPool {
    state: Arc<Mutex<PoolState>>,
}

#[derive(Default)]
struct PoolState {
    sessions: HashMap<SessionKey, Arc<Mutex<Option<RemoteSession>>>>,
    receipts: HashMap<ReceiptKey, ExecutionReceipt>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    provider: CodeExecutionProviderKind,
    credential: [u8; 32],
    workspace_id: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ReceiptKey {
    provider: CodeExecutionProviderKind,
    execution_id: String,
}

#[derive(Clone)]
pub(crate) struct RemoteSession {
    pub(crate) sandbox_id: String,
    pub(crate) endpoint: Option<String>,
    pub(crate) access_token: Option<String>,
}

pub(crate) enum RemoteSessionError {
    Missing,
    Provider(CodeExecutionError),
}

impl From<CodeExecutionError> for RemoteSessionError {
    fn from(error: CodeExecutionError) -> Self {
        Self::Provider(error)
    }
}

#[async_trait]
pub(crate) trait RemoteSandboxAdapter: Send + Sync {
    fn kind(&self) -> CodeExecutionProviderKind;
    fn credential_fingerprint(&self) -> [u8; 32];

    async fn create_session(&self, workspace_id: &str)
        -> Result<RemoteSession, CodeExecutionError>;

    async fn reconnect_session(
        &self,
        session: &RemoteSession,
    ) -> Result<Option<RemoteSession>, CodeExecutionError>;

    async fn run_command(
        &self,
        session: &RemoteSession,
        request: &CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, RemoteSessionError>;
}

/// Vendor file and teardown transports behind the shared workspace lifecycle.
#[async_trait]
pub(crate) trait RemoteWorkspaceAdapter: RemoteSandboxAdapter {
    async fn upload_file(
        &self,
        session: &RemoteSession,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), RemoteSessionError>;

    async fn download_file(
        &self,
        session: &RemoteSession,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, RemoteSessionError>;

    async fn list_directory(
        &self,
        session: &RemoteSession,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, RemoteSessionError>;

    /// Destroy the remote sandbox. A sandbox that is already gone is success.
    async fn destroy_sandbox(&self, session: &RemoteSession) -> Result<(), CodeExecutionError>;
}

impl RemoteSessionPool {
    async fn begin_execution(
        &self,
        provider: CodeExecutionProviderKind,
        request: &CodeExecutionRequest,
    ) -> Result<BeginExecution, CodeExecutionError> {
        let fingerprint = request_fingerprint(request)?;
        let key = ReceiptKey {
            provider,
            execution_id: request.execution_id.as_str().to_owned(),
        };
        let mut state = self.state.lock().await;
        match state.receipts.get(&key) {
            None => {
                state
                    .receipts
                    .insert(key, ExecutionReceipt::running(fingerprint));
                Ok(BeginExecution::Started)
            }
            Some(receipt) => receipt.replay(&fingerprint, CodeExecutionError::Unavailable),
        }
    }

    async fn finish_execution(
        &self,
        provider: CodeExecutionProviderKind,
        request: &CodeExecutionRequest,
        outcome: &Result<CodeExecutionResponse, CodeExecutionError>,
    ) -> Result<(), CodeExecutionError> {
        let key = ReceiptKey {
            provider,
            execution_id: request.execution_id.as_str().to_owned(),
        };
        let receipt = ExecutionReceipt::from_outcome(request_fingerprint(request)?, outcome);
        self.state.lock().await.receipts.insert(key, receipt);
        Ok(())
    }

    async fn session(
        &self,
        provider: CodeExecutionProviderKind,
        credential: [u8; 32],
        workspace_id: &str,
    ) -> Arc<Mutex<Option<RemoteSession>>> {
        let key = SessionKey {
            provider,
            credential,
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

pub(crate) async fn execute_remote(
    adapter: &dyn RemoteSandboxAdapter,
    pool: &RemoteSessionPool,
    request: CodeExecutionRequest,
) -> Result<CodeExecutionResponse, CodeExecutionError> {
    request.validate()?;
    let provider = adapter.kind();
    match pool.begin_execution(provider, &request).await? {
        BeginExecution::Cached(response) => return Ok(response),
        BeginExecution::Started => {}
    }
    let outcome = execute_uncached(adapter, pool, &request).await;
    pool.finish_execution(provider, &request, &outcome).await?;
    outcome
}

async fn execute_uncached(
    adapter: &dyn RemoteSandboxAdapter,
    pool: &RemoteSessionPool,
    request: &CodeExecutionRequest,
) -> Result<CodeExecutionResponse, CodeExecutionError> {
    with_remote_session(
        adapter,
        pool,
        request.workspace_id.as_str(),
        |adapter, session| async move { adapter.run_command(&session, request).await },
    )
    .await
}

/// Run one operation against the chat's connected remote session, creating or
/// reconnecting the sandbox first. Operations for one chat are serialized so
/// workspace mutations have a deterministic order and session replacement
/// cannot race execution.
pub(crate) async fn with_remote_session<'a, A, T, F, Fut>(
    adapter: &'a A,
    pool: &RemoteSessionPool,
    workspace_id: &str,
    operation: F,
) -> Result<T, CodeExecutionError>
where
    A: RemoteSandboxAdapter + ?Sized,
    F: FnOnce(&'a A, RemoteSession) -> Fut,
    Fut: std::future::Future<Output = Result<T, RemoteSessionError>>,
{
    let slot = pool
        .session(
            adapter.kind(),
            adapter.credential_fingerprint(),
            workspace_id,
        )
        .await;
    let mut slot = slot.lock().await;
    let active = connected_session(adapter, &mut slot, workspace_id).await?;
    match operation(adapter, active).await {
        Ok(value) => Ok(value),
        Err(RemoteSessionError::Missing) => {
            *slot = None;
            Err(CodeExecutionError::Unavailable(format!(
                "{} sandbox is no longer available",
                adapter.kind()
            )))
        }
        Err(RemoteSessionError::Provider(error)) => Err(error),
    }
}

async fn connected_session<A>(
    adapter: &A,
    slot: &mut Option<RemoteSession>,
    workspace_id: &str,
) -> Result<RemoteSession, CodeExecutionError>
where
    A: RemoteSandboxAdapter + ?Sized,
{
    let active = match slot.as_ref() {
        Some(existing) => match adapter.reconnect_session(existing).await? {
            Some(connected) => connected,
            None => adapter.create_session(workspace_id).await?,
        },
        None => adapter.create_session(workspace_id).await?,
    };
    *slot = Some(active.clone());
    Ok(active)
}

/// Ensure the chat's durable remote workspace exists.
pub(crate) async fn create_remote_workspace<A>(
    adapter: &A,
    pool: &RemoteSessionPool,
    workspace_id: &str,
) -> Result<(), CodeExecutionError>
where
    A: RemoteSandboxAdapter + ?Sized,
{
    let slot = pool
        .session(
            adapter.kind(),
            adapter.credential_fingerprint(),
            workspace_id,
        )
        .await;
    let mut slot = slot.lock().await;
    connected_session(adapter, &mut slot, workspace_id)
        .await
        .map(|_| ())
}

/// Connect to an existing remote workspace without creating one. The host can
/// only reach sandboxes it has a pooled handle for; a workspace with no handle
/// reports unreachable rather than provisioning a new sandbox.
pub(crate) async fn connect_remote_workspace<A>(
    adapter: &A,
    pool: &RemoteSessionPool,
    workspace_id: &str,
) -> Result<bool, CodeExecutionError>
where
    A: RemoteSandboxAdapter + ?Sized,
{
    let slot = pool
        .session(
            adapter.kind(),
            adapter.credential_fingerprint(),
            workspace_id,
        )
        .await;
    let mut slot = slot.lock().await;
    let Some(existing) = slot.as_ref() else {
        return Ok(false);
    };
    match adapter.reconnect_session(existing).await? {
        Some(connected) => {
            *slot = Some(connected);
            Ok(true)
        }
        None => {
            *slot = None;
            Ok(false)
        }
    }
}

/// Destroy the chat's remote sandbox if the host holds a handle to one. The
/// handle is released only after the provider acknowledges the destroy, so a
/// failed teardown stays retryable.
pub(crate) async fn destroy_remote_workspace<A>(
    adapter: &A,
    pool: &RemoteSessionPool,
    workspace_id: &str,
) -> Result<(), CodeExecutionError>
where
    A: RemoteWorkspaceAdapter + ?Sized,
{
    let slot = pool
        .session(
            adapter.kind(),
            adapter.credential_fingerprint(),
            workspace_id,
        )
        .await;
    let mut slot = slot.lock().await;
    let Some(session) = slot.take() else {
        return Ok(());
    };
    match adapter.destroy_sandbox(&session).await {
        Ok(()) => Ok(()),
        Err(error) => {
            *slot = Some(session);
            Err(error)
        }
    }
}
