use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::receipt::{request_fingerprint, BeginExecution, ExecutionReceipt};
use crate::{
    CodeExecutionError, CodeExecutionProviderKind, CodeExecutionRequest, CodeExecutionResponse,
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
    let provider = adapter.kind();
    let session = pool
        .session(
            provider,
            adapter.credential_fingerprint(),
            request.workspace_id.as_str(),
        )
        .await;
    // Commands for one chat are serialized so workspace mutations have a
    // deterministic order and session replacement cannot race execution.
    let mut session = session.lock().await;
    let active = match session.as_ref() {
        Some(existing) => match adapter.reconnect_session(existing).await? {
            Some(connected) => connected,
            None => {
                adapter
                    .create_session(request.workspace_id.as_str())
                    .await?
            }
        },
        None => {
            adapter
                .create_session(request.workspace_id.as_str())
                .await?
        }
    };
    *session = Some(active.clone());

    match adapter.run_command(&active, request).await {
        Ok(response) => Ok(response),
        Err(RemoteSessionError::Missing) => {
            *session = None;
            Err(CodeExecutionError::Unavailable(format!(
                "{} sandbox is no longer available",
                provider
            )))
        }
        Err(RemoteSessionError::Provider(error)) => Err(error),
    }
}
