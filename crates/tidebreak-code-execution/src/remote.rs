use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use tidebreak_egress::EgressPolicy;
use tokio::sync::Mutex;

use crate::receipt::{request_fingerprint, BeginExecution, ExecutionReceipt};
use crate::{
    ExecError, ExecProviderKind, ExecRequest, ExecResponse, StagedUpload, WorkspaceFilePath,
    WorkspaceListing,
};

/// How long a receipt left in `Running` keeps reporting the execution as
/// ambiguous before the pool treats it as abandoned.
///
/// A receipt is left running when the host's future is dropped mid-execution —
/// a cancelled or interrupted turn — and the host then genuinely does not know
/// whether the remote command completed. Reporting that as ambiguous is the
/// point, because the command may still be running server-side and the tool
/// call id is reused by a resumed or replayed turn. What was wrong is that the
/// state never cleared: nothing short of a process restart let the same call
/// be attempted again. This window is well past any remote command's own
/// lifetime, so once it elapses the earlier attempt is over one way or the
/// other, and the safe recovery is to run the command again rather than to
/// report a failure for an outcome that may have been a success.
const ABANDONED_RUNNING_AFTER: Duration = Duration::from_secs(30 * 60);

/// How long a settled receipt stays available to replay its outcome. Past it
/// the entry is dropped, which is the same state a restarted process is in.
///
/// Losing a settled receipt — to this window or to the ceiling below — narrows
/// two guarantees, and a reader should not treat a receipt as authoritative
/// forever. A replayed tool call whose receipt is gone runs the command again
/// instead of returning the recorded response, and the fingerprint check that
/// answers `IdentityConflict` when the same execution id comes back with
/// different arguments has nothing left to compare against. Both are already
/// true across a restart, and six hours is well past the length of a session,
/// so the window is chosen to make this rare rather than to make it impossible.
const SETTLED_RECEIPT_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Soft ceiling on retained receipts. Settled receipts hold their whole
/// captured response, so the map is trimmed oldest-first once it is reached;
/// a receipt still inside its running window is never evicted, since dropping
/// it would discard exactly the ambiguity it exists to report.
const MAX_RECEIPTS: usize = 512;

/// Shared process-local sessions and idempotency receipts for managed sandboxes.
#[derive(Clone, Default)]
pub struct RemoteSessionPool {
    state: Arc<Mutex<PoolState>>,
}

#[derive(Default)]
struct PoolState {
    sessions: HashMap<SessionKey, Arc<Mutex<Option<PooledSession>>>>,
    receipts: HashMap<ReceiptKey, ReceiptEntry>,
    /// Content digests of files already staged into each workspace's live
    /// sandbox, bound to the exact sandbox instance they were uploaded to. A
    /// recreated sandbox never inherits digests from its predecessor.
    staged: HashMap<SessionKey, StagedDigests>,
}

/// One receipt and when it was recorded, so the pool can bound how long it is
/// kept and how long an unfinished execution stays ambiguous.
struct ReceiptEntry {
    receipt: ExecutionReceipt,
    recorded_at: Instant,
}

impl ReceiptEntry {
    fn new(receipt: ExecutionReceipt, now: Instant) -> Self {
        Self {
            receipt,
            recorded_at: now,
        }
    }

    /// Whether this entry still says anything. A running receipt past the
    /// abandonment window describes an execution nobody is waiting on and no
    /// remote command still serves; a settled one past its TTL is cache.
    fn is_live(&self, now: Instant) -> bool {
        let age = now.saturating_duration_since(self.recorded_at);
        match self.receipt {
            ExecutionReceipt::Running { .. } => age < ABANDONED_RUNNING_AFTER,
            _ => age < SETTLED_RECEIPT_TTL,
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.receipt, ExecutionReceipt::Running { .. })
    }
}

impl PoolState {
    /// Drop receipts that have stopped meaning anything, then trim the map back
    /// under its ceiling by discarding the oldest settled entries.
    fn prune_receipts(&mut self, now: Instant) {
        self.receipts.retain(|_, entry| entry.is_live(now));
        if self.receipts.len() < MAX_RECEIPTS {
            return;
        }
        let mut settled: Vec<(ReceiptKey, Instant)> = self
            .receipts
            .iter()
            .filter(|(_, entry)| !entry.is_running())
            .map(|(key, entry)| (key.clone(), entry.recorded_at))
            .collect();
        settled.sort_by_key(|(_, recorded_at)| *recorded_at);
        for (key, _) in settled {
            if self.receipts.len() < MAX_RECEIPTS {
                break;
            }
            self.receipts.remove(&key);
        }
        // If every remaining receipt is inside its running window the map is
        // allowed past the ceiling, because none of them may lose its fail-safe.
        // That is not a bound on concurrency: a receipt is inserted before any
        // per-workspace serialization is taken, and an abandoned one stays until
        // it ages out, so a host issuing distinct execution ids and dropping each
        // future can accumulate entries for the length of that window. They are
        // small and the window ends, which is why nothing here caps them harder.
    }

    /// Drop pooled session slots nobody holds and that carry no live sandbox,
    /// along with the staged ledgers that belonged to them. Without this the
    /// map keeps one entry per workspace the process has ever touched.
    fn prune_sessions(&mut self) {
        let sessions = &mut self.sessions;
        let staged = &mut self.staged;
        sessions.retain(|key, slot| {
            // Another task holding a clone is mid-operation on this slot.
            if Arc::strong_count(slot) > 1 {
                return true;
            }
            let Ok(guard) = slot.try_lock() else {
                return true;
            };
            if guard.is_some() {
                return true;
            }
            drop(guard);
            staged.remove(key);
            false
        });
    }
}

struct StagedDigests {
    sandbox_id: String,
    digests: HashMap<String, [u8; 32]>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    provider: ExecProviderKind,
    credential: [u8; 32],
    workspace_id: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ReceiptKey {
    provider: ExecProviderKind,
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
    Provider(ExecError),
}

impl From<ExecError> for RemoteSessionError {
    fn from(error: ExecError) -> Self {
        Self::Provider(error)
    }
}

#[async_trait]
pub(crate) trait RemoteSandboxAdapter: Send + Sync {
    fn kind(&self) -> ExecProviderKind;
    fn credential_fingerprint(&self) -> [u8; 32];

    /// Fingerprint of the egress policy this adapter compiles into sandbox
    /// creation. A pooled sandbox created under a different fingerprint is
    /// stale — its network controls no longer reflect the effective policy —
    /// and must be replaced rather than reused.
    fn egress_fingerprint(&self) -> [u8; 32];

    async fn create_session(&self, workspace_id: &str) -> Result<RemoteSession, ExecError>;

    /// Destroy the remote sandbox. A sandbox that is already gone is success.
    async fn destroy_sandbox(&self, session: &RemoteSession) -> Result<(), ExecError>;

    async fn reconnect_session(
        &self,
        session: &RemoteSession,
    ) -> Result<Option<RemoteSession>, ExecError>;

    async fn run_command(
        &self,
        session: &RemoteSession,
        request: &ExecRequest,
    ) -> Result<ExecResponse, RemoteSessionError>;
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
        provider: ExecProviderKind,
        request: &ExecRequest,
    ) -> Result<BeginExecution, ExecError> {
        let fingerprint = request_fingerprint(request)?;
        let key = ReceiptKey {
            provider,
            execution_id: request.execution_id.as_str().to_owned(),
        };
        let mut state = self.state.lock().await;
        state.prune_receipts(Instant::now());
        match state.receipts.get(&key) {
            None => {
                state.receipts.insert(
                    key,
                    ReceiptEntry::new(ExecutionReceipt::running(fingerprint), Instant::now()),
                );
                Ok(BeginExecution::Started)
            }
            Some(entry) => entry.receipt.replay(&fingerprint, ExecError::Unavailable),
        }
    }

    async fn finish_execution(
        &self,
        provider: ExecProviderKind,
        request: &ExecRequest,
        outcome: &Result<ExecResponse, ExecError>,
    ) -> Result<(), ExecError> {
        let key = ReceiptKey {
            provider,
            execution_id: request.execution_id.as_str().to_owned(),
        };
        let receipt = ExecutionReceipt::from_outcome(request_fingerprint(request)?, outcome);
        self.state
            .lock()
            .await
            .receipts
            .insert(key, ReceiptEntry::new(receipt, Instant::now()));
        Ok(())
    }

    async fn session(
        &self,
        provider: ExecProviderKind,
        credential: [u8; 32],
        workspace_id: &str,
    ) -> Arc<Mutex<Option<PooledSession>>> {
        let key = SessionKey {
            provider,
            credential,
            workspace_id: workspace_id.to_owned(),
        };
        let mut state = self.state.lock().await;
        state.prune_sessions();
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

    /// Release the pooled slot for a destroyed sandbox, so the map does not
    /// keep one entry per workspace for the life of the process.
    ///
    /// The slot is only removed when this caller is its last holder besides the
    /// map: another task already waiting on the same slot must keep working
    /// against the entry it took, or the workspace would end up with two. Such
    /// a slot is empty by the time this runs, so [`PoolState::prune_sessions`]
    /// reclaims it once the other holder is done. That reasoning covers only
    /// slots this function leaves behind — a slot still carrying a sandbox is
    /// never swept.
    async fn forget_session(&self, key: &SessionKey) {
        let mut state = self.state.lock().await;
        state.staged.remove(key);
        let releasable = state
            .sessions
            .get(key)
            .is_some_and(|slot| Arc::strong_count(slot) <= 2);
        if releasable {
            state.sessions.remove(key);
        }
    }

    #[cfg(test)]
    async fn receipt_count(&self) -> usize {
        self.state.lock().await.receipts.len()
    }

    #[cfg(test)]
    async fn session_count(&self) -> usize {
        self.state.lock().await.sessions.len()
    }

    /// Backdate every receipt, standing in for the passage of time.
    #[cfg(test)]
    async fn age_receipts(&self, by: Duration) {
        for entry in self.state.lock().await.receipts.values_mut() {
            entry.recorded_at = entry
                .recorded_at
                .checked_sub(by)
                .expect("test clock stays inside Instant's range");
        }
    }
}

pub(crate) async fn execute_remote(
    adapter: &dyn RemoteSandboxAdapter,
    pool: &RemoteSessionPool,
    request: ExecRequest,
) -> Result<ExecResponse, ExecError> {
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
    request: &ExecRequest,
) -> Result<ExecResponse, ExecError> {
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
) -> Result<T, ExecError>
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
            Err(ExecError::Unavailable(format!(
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
) -> Result<StagedUpload, ExecError>
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
) -> Result<RemoteSession, ExecError>
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
) -> Result<(), ExecError>
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
) -> Result<bool, ExecError>
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
///
/// Either way the staged ledger goes: it describes content in a sandbox this
/// call has tried to tear down, and the cost of forgetting it is re-uploading
/// a file, while the cost of keeping it is skipping an upload the sandbox no
/// longer has — or retaining it for a workspace nothing will ask about again.
pub(crate) async fn destroy_remote_workspace<A>(
    adapter: &A,
    pool: &RemoteSessionPool,
    workspace_id: &str,
) -> Result<(), ExecError>
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
        pool.forget_session(&key).await;
        return Ok(());
    };
    match adapter.destroy_sandbox(&pooled.session).await {
        Ok(()) => {
            // The slot is empty and the sandbox is gone, so the entry itself
            // can go; a retryable failure keeps it, since the retry needs the
            // handle this pooled session carries.
            drop(slot);
            pool.forget_session(&key).await;
            Ok(())
        }
        Err(error) => {
            // The handle goes back because the destroy is retryable and the
            // retry needs it. That keeps the map entry too: a slot carrying a
            // sandbox is never swept, so a teardown that fails and is never
            // retried holds its entry for the life of the process. One small
            // entry per abandoned workspace is the accepted cost of not
            // stranding a live sandbox nothing can address any more.
            *slot = Some(pooled);
            pool.clear_staged(&key).await;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::{ExecutionId, ExecutionWorkspaceId};

    /// A sandbox adapter that can be told to hang, standing in for a remote
    /// command whose result the host never gets to see.
    #[derive(Default)]
    struct FakeAdapter {
        hang: AtomicBool,
        runs: AtomicUsize,
        destroys: AtomicUsize,
    }

    #[async_trait]
    impl RemoteSandboxAdapter for FakeAdapter {
        fn kind(&self) -> ExecProviderKind {
            ExecProviderKind::E2b
        }

        fn credential_fingerprint(&self) -> [u8; 32] {
            [7; 32]
        }

        fn egress_fingerprint(&self) -> [u8; 32] {
            [9; 32]
        }

        async fn create_session(&self, _workspace_id: &str) -> Result<RemoteSession, ExecError> {
            Ok(RemoteSession {
                sandbox_id: "sandbox-1".to_owned(),
                endpoint: None,
                access_token: None,
            })
        }

        async fn destroy_sandbox(&self, _session: &RemoteSession) -> Result<(), ExecError> {
            self.destroys.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn reconnect_session(
            &self,
            session: &RemoteSession,
        ) -> Result<Option<RemoteSession>, ExecError> {
            Ok(Some(session.clone()))
        }

        async fn run_command(
            &self,
            _session: &RemoteSession,
            _request: &ExecRequest,
        ) -> Result<ExecResponse, RemoteSessionError> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            if self.hang.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(ExecResponse {
                provider: ExecProviderKind::E2b,
                exit_code: Some(0),
                stdout: "done".to_owned(),
                stderr: String::new(),
                timed_out: false,
                output_truncated: false,
                duration_ms: 1,
                sync_notes: Vec::new(),
                degraded: None,
            })
        }
    }

    #[async_trait]
    impl RemoteWorkspaceAdapter for FakeAdapter {
        async fn upload_file(
            &self,
            _session: &RemoteSession,
            _path: &WorkspaceFilePath,
            _content: &[u8],
        ) -> Result<(), RemoteSessionError> {
            Ok(())
        }

        async fn download_file(
            &self,
            _session: &RemoteSession,
            _path: &WorkspaceFilePath,
        ) -> Result<Vec<u8>, RemoteSessionError> {
            Ok(Vec::new())
        }

        async fn list_directory(
            &self,
            _session: &RemoteSession,
            _path: Option<&WorkspaceFilePath>,
        ) -> Result<WorkspaceListing, RemoteSessionError> {
            Ok(WorkspaceListing {
                entries: Vec::new(),
                truncated: false,
            })
        }
    }

    fn request(execution: &str, workspace: &str) -> ExecRequest {
        ExecRequest::new(
            ExecutionId::parse(execution).unwrap(),
            ExecutionWorkspaceId::parse(workspace).unwrap(),
            "/bin/echo",
            vec!["hello".to_owned()],
            ".",
        )
        .unwrap()
    }

    /// A turn cancelled mid-execution drops the future between the running
    /// receipt and its outcome. The host genuinely does not know whether the
    /// remote command ran, so the next attempt at the same tool call id is
    /// ambiguous — and stayed ambiguous forever, which is what made a resumed
    /// turn unrunnable until the process restarted. The ambiguity now expires.
    #[tokio::test]
    async fn an_abandoned_execution_stops_being_ambiguous_once_it_ages_out() {
        let adapter = FakeAdapter {
            hang: AtomicBool::new(true),
            ..FakeAdapter::default()
        };
        let pool = RemoteSessionPool::default();

        let cancelled = tokio::time::timeout(
            Duration::from_millis(50),
            execute_remote(&adapter, &pool, request("execution-1", "workspace-1")),
        )
        .await;
        assert!(cancelled.is_err(), "the execution future is dropped");

        adapter.hang.store(false, Ordering::SeqCst);
        let replayed = execute_remote(&adapter, &pool, request("execution-1", "workspace-1")).await;
        assert!(
            matches!(replayed, Err(ExecError::AmbiguousExecution)),
            "an execution that may still be running stays ambiguous"
        );

        pool.age_receipts(ABANDONED_RUNNING_AFTER).await;
        let recovered = execute_remote(&adapter, &pool, request("execution-1", "workspace-1"))
            .await
            .expect("the abandoned execution is retried rather than failed");
        assert_eq!(recovered.stdout, "done");
    }

    /// The pool lives for the life of the process, so a destroyed workspace
    /// that keeps its map entries is a leak measured in chats.
    #[tokio::test]
    async fn destroying_a_workspace_releases_its_pool_entries() {
        let adapter = FakeAdapter::default();
        let pool = RemoteSessionPool::default();
        let path = WorkspaceFilePath::parse("input.txt").unwrap();

        stage_remote_file(&adapter, &pool, "workspace-1", &path, b"bytes")
            .await
            .unwrap();
        execute_remote(&adapter, &pool, request("execution-1", "workspace-1"))
            .await
            .unwrap();
        assert_eq!(pool.session_count().await, 1);

        destroy_remote_workspace(&adapter, &pool, "workspace-1")
            .await
            .unwrap();
        assert_eq!(adapter.destroys.load(Ordering::SeqCst), 1);
        assert_eq!(pool.session_count().await, 0);
        assert!(pool.state.lock().await.staged.is_empty());

        // The receipt outlives the sandbox on purpose — it is what makes a
        // replayed tool call idempotent — but not the process.
        assert_eq!(pool.receipt_count().await, 1);
        pool.age_receipts(SETTLED_RECEIPT_TTL).await;
        execute_remote(&adapter, &pool, request("execution-2", "workspace-2"))
            .await
            .unwrap();
        assert_eq!(pool.receipt_count().await, 1);
    }
}
