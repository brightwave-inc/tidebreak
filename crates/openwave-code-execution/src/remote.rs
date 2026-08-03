use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use openwave_egress::EgressPolicy;
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;

use crate::receipt::{request_fingerprint, BeginExecution, ExecutionReceipt};
use crate::{
    CodeExecutionError, CodeExecutionProviderKind, CodeExecutionRequest, CodeExecutionResponse,
    StagedUpload, WorkspaceFilePath, WorkspaceListing,
};

/// Shared process-local sessions and idempotency receipts for managed sandboxes.
#[derive(Clone, Default)]
pub struct RemoteSessionPool {
    state: Arc<Mutex<PoolState>>,
}

#[derive(Default)]
struct PoolState {
    sessions: HashMap<SessionKey, Arc<Mutex<Option<PooledSession>>>>,
    receipts: HashMap<ReceiptKey, ExecutionReceipt>,
    /// Content digests of files already staged into each workspace's live
    /// sandbox, bound to the exact sandbox instance they were uploaded to. A
    /// recreated sandbox never inherits digests from its predecessor.
    staged: HashMap<SessionKey, StagedDigests>,
}

struct StagedDigests {
    sandbox_id: String,
    digests: HashMap<String, [u8; 32]>,
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

/// A pooled session bound to the egress policy its sandbox was created under.
///
/// Managed sandboxes compile egress into creation-time network controls, so a
/// live sandbox does not follow later policy edits. Recording the creation
/// policy's fingerprint here lets the pool notice the mismatch and replace the
/// sandbox instead of silently reusing one with stale egress.
struct PooledSession {
    session: RemoteSession,
    egress: [u8; 32],
}

/// Stable identity of the egress policy a provider compiles into sandbox
/// creation. `None` is today's open-internet creation.
pub(crate) fn egress_policy_fingerprint(policy: Option<&EgressPolicy>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    match policy {
        None => hasher.update(b"open"),
        Some(EgressPolicy::BlockAll) => hasher.update(b"block-all"),
        Some(EgressPolicy::Allowlist(allowlist)) => {
            hasher.update(b"allowlist");
            for domain in allowlist.domains() {
                hasher.update(b"\nd:");
                hasher.update(domain.to_string().as_bytes());
            }
            for cidr in allowlist.cidrs() {
                hasher.update(b"\nc:");
                hasher.update(cidr.to_string().as_bytes());
            }
        }
    }
    hasher.finalize().into()
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

    /// Fingerprint of the egress policy this adapter compiles into sandbox
    /// creation. A pooled sandbox created under a different fingerprint is
    /// stale — its network controls no longer reflect the effective policy —
    /// and must be replaced rather than reused.
    fn egress_fingerprint(&self) -> [u8; 32];

    async fn create_session(&self, workspace_id: &str)
        -> Result<RemoteSession, CodeExecutionError>;

    /// Destroy the remote sandbox. A sandbox that is already gone is success.
    async fn destroy_sandbox(&self, session: &RemoteSession) -> Result<(), CodeExecutionError>;

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
    ) -> Arc<Mutex<Option<PooledSession>>> {
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

    /// Whether the live sandbox `sandbox_id` already holds `path` with exactly
    /// this content, according to what previous stagings recorded.
    async fn staged_is_current(
        &self,
        key: &SessionKey,
        sandbox_id: &str,
        path: &str,
        digest: &[u8; 32],
    ) -> bool {
        let state = self.state.lock().await;
        state.staged.get(key).is_some_and(|staged| {
            staged.sandbox_id == sandbox_id && staged.digests.get(path) == Some(digest)
        })
    }

    /// Record that `path` with `digest` was just uploaded to `sandbox_id`.
    /// Digests recorded against a different sandbox instance are discarded:
    /// their staged state belongs to a sandbox that no longer serves this key.
    async fn record_staged(
        &self,
        key: &SessionKey,
        sandbox_id: &str,
        path: String,
        digest: [u8; 32],
    ) {
        let mut state = self.state.lock().await;
        let staged = state
            .staged
            .entry(key.clone())
            .or_insert_with(|| StagedDigests {
                sandbox_id: sandbox_id.to_owned(),
                digests: HashMap::new(),
            });
        if staged.sandbox_id != sandbox_id {
            staged.sandbox_id = sandbox_id.to_owned();
            staged.digests.clear();
        }
        staged.digests.insert(path, digest);
    }

    /// Forget everything staged for this key. Called when the sandbox is
    /// destroyed, so a successor sandbox starts from an empty ledger.
    async fn clear_staged(&self, key: &SessionKey) {
        self.state.lock().await.staged.remove(key);
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

/// Stage one file into the chat's remote sandbox, skipping the upload when
/// the pool remembers the live sandbox already holds identical content.
///
/// The digest check and the record both happen inside the per-chat session
/// lock, so the sandbox instance the ledger is bound to is exactly the one the
/// upload targeted.
pub(crate) async fn stage_remote_file<A>(
    adapter: &A,
    pool: &RemoteSessionPool,
    workspace_id: &str,
    path: &WorkspaceFilePath,
    content: &[u8],
) -> Result<StagedUpload, CodeExecutionError>
where
    A: RemoteWorkspaceAdapter + ?Sized,
{
    let key = SessionKey {
        provider: adapter.kind(),
        credential: adapter.credential_fingerprint(),
        workspace_id: workspace_id.to_owned(),
    };
    let digest: [u8; 32] = Sha256::digest(content).into();
    with_remote_session(adapter, pool, workspace_id, |adapter, session| async move {
        if pool
            .staged_is_current(&key, &session.sandbox_id, path.as_str(), &digest)
            .await
        {
            return Ok(StagedUpload::AlreadyCurrent);
        }
        adapter.upload_file(&session, path, content).await?;
        pool.record_staged(&key, &session.sandbox_id, path.as_str().to_owned(), digest)
            .await;
        Ok(StagedUpload::Uploaded)
    })
    .await
}

async fn connected_session<A>(
    adapter: &A,
    slot: &mut Option<PooledSession>,
    workspace_id: &str,
) -> Result<RemoteSession, CodeExecutionError>
where
    A: RemoteSandboxAdapter + ?Sized,
{
    let egress = adapter.egress_fingerprint();
    // A sandbox compiled under a different egress policy is stale: reusing it
    // would keep enforcing the old policy (a chat whose network was off keeps
    // failing DNS after the user opens it, and the reverse silently widens
    // egress). Replace it. The destroy is best-effort — a sandbox the destroy
    // could not reach expires on its own TTL.
    if let Some(pooled) = slot.as_ref() {
        if pooled.egress != egress {
            let _ = adapter.destroy_sandbox(&pooled.session).await;
            *slot = None;
        }
    }
    let active = match slot.as_ref() {
        Some(pooled) => match adapter.reconnect_session(&pooled.session).await? {
            Some(connected) => connected,
            None => adapter.create_session(workspace_id).await?,
        },
        None => adapter.create_session(workspace_id).await?,
    };
    *slot = Some(PooledSession {
        session: active.clone(),
        egress,
    });
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
    let egress = existing.egress;
    match adapter.reconnect_session(&existing.session).await? {
        Some(connected) => {
            // Reconnecting does not change the sandbox's creation-time egress,
            // so the recorded fingerprint carries over; a mismatch with the
            // effective policy is resolved by the next operation.
            *slot = Some(PooledSession {
                session: connected,
                egress,
            });
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
    let key = SessionKey {
        provider: adapter.kind(),
        credential: adapter.credential_fingerprint(),
        workspace_id: workspace_id.to_owned(),
    };
    let slot = pool
        .session(
            adapter.kind(),
            adapter.credential_fingerprint(),
            workspace_id,
        )
        .await;
    let mut slot = slot.lock().await;
    let Some(pooled) = slot.take() else {
        return Ok(());
    };
    match adapter.destroy_sandbox(&pooled.session).await {
        Ok(()) => {
            pool.clear_staged(&key).await;
            Ok(())
        }
        Err(error) => {
            *slot = Some(pooled);
            Err(error)
        }
    }
}
