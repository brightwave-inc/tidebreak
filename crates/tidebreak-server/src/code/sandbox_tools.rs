//! Authenticated, durable native-tool execution for one managed sandbox incarnation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_core::code::supervisor_tools::{
    validate_request, SupervisorArtifact, SupervisorToolResult, TOOLS,
};
use tidebreak_core::code::SupervisorToolRequest;
use tidebreak_core::db::code::{
    claim_native_tool_request, complete_native_tool_request, enqueue_native_tool_request,
    list_native_tool_requests, mark_native_tool_request_delivered, NativeToolClaim,
    NativeToolReceipt, NativeToolStatus,
};
use tidebreak_core::{
    AgentError, CallId, CodeIncarnationId, ExecutionLocation, OwnerId, PermissionMode, SessionId,
    SessionLifecycle, ToolCtx, ToolOutput,
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use super::runtime::CodeRuntime;

const MAX_WORKERS: usize = 16;
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(120);
const UNCERTAIN: &str = "This native call started, but its outcome was not recorded. It will not run again automatically. Inspect the existing child sessions or conversation requests before taking further action.";

/// Holds no strong runtime reference between calls, so attachment cannot form a cycle.
pub struct SandboxToolExecutor {
    runtime: Weak<CodeRuntime>,
    workers: Arc<Mutex<HashMap<CallId, tokio::task::AbortHandle>>>,
    slots: Arc<Semaphore>,
    service_lock: AsyncMutex<()>,
    stopped: AtomicBool,
}

impl SandboxToolExecutor {
    pub fn new(runtime: Weak<CodeRuntime>) -> Self {
        Self {
            runtime,
            workers: Arc::default(),
            slots: Arc::new(Semaphore::new(MAX_WORKERS)),
            service_lock: AsyncMutex::new(()),
            stopped: AtomicBool::new(false),
        }
    }

    fn runtime(&self) -> Result<Arc<CodeRuntime>, AgentError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(AgentError::config("native tool executor has stopped"));
        }
        self.runtime
            .upgrade()
            .ok_or_else(|| AgentError::config("native tool runtime is unavailable"))
    }

    async fn delivery_receipt(
        &self,
        owner: &OwnerId,
        session: SessionId,
        incarnation: CodeIncarnationId,
        request_id: &str,
    ) -> Result<(Arc<CodeRuntime>, NativeToolReceipt), AgentError> {
        let runtime = self.runtime()?;
        require_session(&runtime, owner, session).await?;
        let receipt = list_native_tool_requests(&runtime.db, owner, session, incarnation)
            .await?
            .into_iter()
            .find(|receipt| receipt.request_id == request_id)
            .filter(|receipt| receipt.status == NativeToolStatus::Completed)
            .ok_or_else(|| {
                AgentError::AccessDenied(
                    "native tool result is absent or no longer deliverable".into(),
                )
            })?;
        Ok((runtime, receipt))
    }

    fn start_worker(
        &self,
        owner: OwnerId,
        session: SessionId,
        incarnation: CodeIncarnationId,
        receipt: NativeToolReceipt,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) {
        let guard = WorkerGuard {
            workers: self.workers.clone(),
            call: receipt.call_id,
        };
        let weak = self.runtime.clone();
        let (start, ready) = tokio::sync::oneshot::channel();
        let call_id = receipt.call_id;
        let job = tokio::spawn(async move {
            let _permit = permit;
            let _guard = guard;
            if ready.await.is_err() {
                return;
            }
            let Some(runtime) = weak.upgrade() else {
                return;
            };
            let request = SupervisorToolRequest {
                request_id: receipt.request_id.clone(),
                tool: receipt.tool.clone(),
                arguments: receipt.arguments.clone(),
            };
            let output = if let Some(remaining) =
                execution_remaining(receipt.claimed_at, chrono::Utc::now())
            {
                tokio::select! {
                    result = tokio::time::timeout(remaining,
                        runtime.execute_sandbox_tool(&owner, session, incarnation, receipt.call_id, &request)
                    ) => result.ok(),
                    () = authority_lost(&runtime, &owner, session, incarnation, receipt.call_id) => None,
                }
            } else {
                None
            };
            let result = match output {
                Some(Ok(result)) => result,
                Some(Err(error)) => SupervisorToolResult::failed(
                    receipt.request_id.clone(),
                    &bounded_error(&error.to_string()),
                ),
                None => SupervisorToolResult::failed(receipt.request_id.clone(), UNCERTAIN),
            };
            let result = match result.validate() {
                Ok(()) => result,
                Err(error) => SupervisorToolResult::failed(receipt.request_id.clone(), &error),
            };
            let stored = match serde_json::to_value(&result) {
                Ok(stored) => stored,
                Err(error) => {
                    tracing::warn!(request_id = %receipt.request_id, %error, "could not encode native tool result");
                    return;
                }
            };
            // Completion rechecks the receipt's exact grant and incarnation.
            // If this write fails, the next pump reports an uncertain outcome.
            if let Err(error) =
                complete_native_tool_request(&runtime.db, &owner, &receipt, &stored).await
            {
                tracing::warn!(request_id = %receipt.request_id, %error, "could not persist native tool result");
            }
        });
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stopped.load(Ordering::Acquire) {
            job.abort();
        } else {
            workers.insert(call_id, job.abort_handle());
            let _ = start.send(());
        }
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let handles: Vec<_> = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for handle in handles {
            handle.abort();
        }
    }
}

impl Drop for SandboxToolExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

struct WorkerGuard {
    workers: Arc<Mutex<HashMap<CallId, tokio::task::AbortHandle>>>,
    call: CallId,
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.call);
    }
}

#[async_trait]
impl super::remote::driver::HostToolExecutor for SandboxToolExecutor {
    fn shutdown(&self) {
        self.stop();
    }

    async fn bootstrap_context(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
    ) -> Result<String, AgentError> {
        let runtime = self.runtime()?;
        if !has_slack_binding(&runtime, owner, session_id).await? {
            return Ok(String::new());
        }
        require_session(&runtime, owner, session_id).await?;
        let tools = runtime
            .tool_registry()
            .ok_or_else(|| AgentError::config("native tool registry is unavailable"))?;
        let specs = TOOLS
            .iter()
            .map(|name| {
                tools
                    .server_tool(name)
                    .map(|tool| tool.spec())
                    .ok_or_else(|| {
                        AgentError::config(format!("native tool {name} is not registered"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let instructions =
            super::channel_preferences::session_instructions(&runtime.db, owner, session_id)
                .await
                .map_err(server_error)?;
        let mut context = String::new();
        super::channel_preferences::append_instructions(&mut context, &instructions);
        context.push_str(&format!(
            "\n\nNative tools: send one JSON object containing request_id, tool, and arguments on stdin to \"$TIDEBREAK_TOOL_HELPER\" tool-call. Use a unique request_id for each logical call and keep it unchanged when retrying that call. A pending conversation result has its own request_id inside output; to resume that conversation read, make a new helper call whose arguments contain only that conversation request_id. The helper prints output and artifact metadata after writing the artifacts relative to the engine working directory.\n\n{}\n\nAvailable native tool schemas:\n{}",
            super::conversation_tools::CONVERSATION_GUIDANCE,
            serde_json::to_string(&specs)?,
        ));
        if context.len() > 48 * 1024 {
            return Err(AgentError::config(
                "native tool bootstrap context exceeds 48 KiB",
            ));
        }
        Ok(context)
    }

    async fn enqueue(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        incarnation: CodeIncarnationId,
        request: &SupervisorToolRequest,
    ) -> Result<(), AgentError> {
        validate_request(request).map_err(AgentError::InvalidTarget)?;
        let runtime = self.runtime()?;
        require_session(&runtime, owner, session_id).await?;
        enqueue_native_tool_request(
            &runtime.db,
            owner,
            session_id,
            incarnation,
            &request.request_id,
            &request.tool,
            &request.arguments,
        )
        .await?;
        Ok(())
    }

    async fn service(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        incarnation: CodeIncarnationId,
    ) -> Result<Vec<SupervisorToolResult>, AgentError> {
        // Claim and local worker registration serialize, so a parallel pump
        // cannot mistake a newly claimed call for a worker lost at restart.
        let _serial = self.service_lock.lock().await;
        let runtime = self.runtime()?;
        if !has_slack_binding(&runtime, owner, session_id).await? {
            return Ok(Vec::new());
        }
        require_session(&runtime, owner, session_id).await?;
        let receipts =
            list_native_tool_requests(&runtime.db, owner, session_id, incarnation).await?;
        let mut results = Vec::new();
        for receipt in receipts {
            if receipt.status == NativeToolStatus::Completed {
                results.push(decode_result(&receipt)?);
                continue;
            }
            if self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&receipt.call_id)
            {
                continue;
            }
            if receipt.status == NativeToolStatus::Running {
                if !recovery_due(receipt.claimed_at, chrono::Utc::now()) {
                    continue;
                }
                let result = SupervisorToolResult::failed(receipt.request_id.clone(), UNCERTAIN);
                complete_native_tool_request(
                    &runtime.db,
                    owner,
                    &receipt,
                    &serde_json::to_value(&result)?,
                )
                .await?;
                results.push(result);
                continue;
            }
            let Ok(permit) = self.slots.clone().try_acquire_owned() else {
                continue;
            };
            match claim_native_tool_request(&runtime.db, owner, &receipt).await? {
                NativeToolClaim::Claimed(receipt) => {
                    self.start_worker(owner.clone(), session_id, incarnation, receipt, permit);
                }
                NativeToolClaim::Completed(receipt) => results.push(decode_result(&receipt)?),
                NativeToolClaim::Running(_) => (),
            }
        }
        Ok(results)
    }

    async fn authorize_delivery(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        incarnation: CodeIncarnationId,
        request_id: &str,
    ) -> Result<(), AgentError> {
        self.delivery_receipt(owner, session_id, incarnation, request_id)
            .await?;
        Ok(())
    }

    async fn mark_delivered(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        incarnation: CodeIncarnationId,
        request_id: &str,
    ) -> Result<(), AgentError> {
        let (runtime, receipt) = self
            .delivery_receipt(owner, session_id, incarnation, request_id)
            .await?;
        mark_native_tool_request_delivered(&runtime.db, owner, &receipt).await
    }
}

fn decode_result(receipt: &NativeToolReceipt) -> Result<SupervisorToolResult, AgentError> {
    let result: SupervisorToolResult =
        serde_json::from_value(receipt.result.clone().ok_or_else(|| {
            AgentError::Store("completed native tool receipt has no result".into())
        })?)?;
    result.validate().map_err(AgentError::InvalidTarget)?;
    if result.request_id != receipt.request_id {
        return Err(AgentError::InvalidTarget(
            "native tool receipt correlation changed".into(),
        ));
    }
    Ok(result)
}

fn server_error(error: crate::error::ServerError) -> AgentError {
    AgentError::Store(format!("{}: {}", error.kind(), error.message()))
}

fn bounded_error(error: &str) -> String {
    error.chars().take(2048).collect()
}

fn execution_remaining(
    claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let remaining = (claimed_at? + chrono::Duration::seconds(EXECUTION_TIMEOUT.as_secs() as i64)
        - now)
        .to_std()
        .ok()?;
    (!remaining.is_zero()).then_some(remaining.min(EXECUTION_TIMEOUT))
}

fn recovery_due(
    claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    claimed_at.is_none_or(|claimed| {
        now - claimed >= chrono::Duration::seconds(EXECUTION_TIMEOUT.as_secs() as i64 + 30)
    })
}

async fn authority_lost(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    call: CallId,
) {
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if runtime.update_quiesce_active()
            || require_session(runtime, owner, session).await.is_err()
        {
            return;
        }
        match list_native_tool_requests(&runtime.db, owner, session, incarnation).await {
            Ok(receipts)
                if receipts.iter().any(|receipt| {
                    receipt.call_id == call && receipt.status == NativeToolStatus::Running
                }) => {}
            _ => return,
        }
    }
}

async fn has_slack_binding(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    session: SessionId,
) -> Result<bool, AgentError> {
    Ok(
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, owner, session)
            .await?
            .iter()
            .any(|binding| binding.channel_kind == "slack"),
    )
}

async fn require_session(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    id: SessionId,
) -> Result<(), AgentError> {
    let session = tidebreak_core::db::code::get_session(&runtime.db, owner, id)
        .await?
        .ok_or_else(|| AgentError::AccessDenied("native tool session is unavailable".into()))?;
    if runtime.update_quiesce_active()
        || session.execution_location != ExecutionLocation::Sandbox
        || session.permission_mode != PermissionMode::Allow
        || matches!(
            session.lifecycle,
            SessionLifecycle::Ended | SessionLifecycle::Fenced
        )
    {
        return Err(AgentError::AccessDenied(
            "native tools require a live sandbox session in Allow mode".into(),
        ));
    }
    Ok(())
}

impl CodeRuntime {
    /// Run only the durable call that this incarnation claimed.
    async fn execute_sandbox_tool(
        self: &Arc<Self>,
        owner: &OwnerId,
        session_id: SessionId,
        incarnation: CodeIncarnationId,
        call_id: CallId,
        request: &SupervisorToolRequest,
    ) -> Result<SupervisorToolResult, AgentError> {
        validate_request(request).map_err(AgentError::InvalidTarget)?;
        require_session(self, owner, session_id).await?;
        let receipt = list_native_tool_requests(&self.db, owner, session_id, incarnation)
            .await?
            .into_iter()
            .find(|receipt| receipt.call_id == call_id && receipt.request_id == request.request_id)
            .filter(|receipt| {
                receipt.status == NativeToolStatus::Running
                    && receipt.tool == request.tool
                    && receipt.arguments == request.arguments
            })
            .ok_or_else(|| {
                AgentError::AccessDenied("native tool request has no live execution claim".into())
            })?;
        let ctx = ToolCtx::without_private_scratch(session_id, None).with_call_id(receipt.call_id);
        let (output, artifacts) = if request.tool.starts_with("conversation_") {
            // This adapter resolves exactly one bound thread and refuses ambiguity.
            let prepared = super::conversation_tools::execute_remote(
                self,
                &ctx,
                &request.tool,
                request.arguments.clone(),
            )
            .await
            .map_err(server_error)?;
            (
                prepared.output,
                prepared
                    .files
                    .into_iter()
                    .map(|file| SupervisorArtifact {
                        path: file.path,
                        media_type: file.media_type,
                        bytes: file.bytes,
                    })
                    .collect(),
            )
        } else {
            let registry = self
                .tool_registry()
                .ok_or_else(|| AgentError::config("native tool registry is unavailable"))?;
            let tool = registry.server_tool(&request.tool).ok_or_else(|| {
                AgentError::config(format!("native tool {} is not registered", request.tool))
            })?;
            (
                tool.execute(&ctx, request.arguments.clone()).await?,
                Vec::new(),
            )
        };
        let result = SupervisorToolResult {
            request_id: request.request_id.clone(),
            output: output_value(output),
            artifacts,
        };
        result.validate().map_err(AgentError::InvalidTarget)?;
        Ok(result)
    }

    /// The authoritative coordinator registry installed at process boot.
    pub fn tool_registry(&self) -> Option<Arc<tidebreak_core::ToolRegistry>> {
        self.tools.clone()
    }
}

fn output_value(output: ToolOutput) -> serde_json::Value {
    serde_json::json!({
        "content": output.content, "is_error": output.is_error,
        "error_category": output.error_category.map(|kind| kind.as_str()), "data": output.data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::remote::driver::HostToolExecutor;
    use std::sync::atomic::AtomicUsize;
    use tidebreak_core::db::code::*;
    use tidebreak_core::{ApprovalClass, Session, Tool, ToolSpec};

    #[derive(Default)]
    struct Calls {
        count: AtomicUsize,
        completed: AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    struct HeldTool {
        name: &'static str,
        calls: Arc<Calls>,
    }
    #[async_trait]
    impl Tool for HeldTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "fixture tool".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }
        async fn execute(
            &self,
            ctx: &ToolCtx,
            _: serde_json::Value,
        ) -> tidebreak_core::Result<ToolOutput> {
            self.calls.count.fetch_add(1, Ordering::SeqCst);
            self.calls.entered.notify_one();
            self.calls.release.notified().await;
            self.calls.completed.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text(ctx.call_id.unwrap().to_string()))
        }
    }

    async fn fixture(
        bound: bool,
        mode: PermissionMode,
    ) -> (
        tempfile::TempDir,
        Arc<CodeRuntime>,
        Session,
        CodeIncarnationId,
        Arc<Calls>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            tidebreak_core::DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("native.db").display()
            ))
            .await
            .unwrap(),
        );
        let calls = Arc::new(Calls::default());
        let mut tools = tidebreak_core::ToolRegistry::new();
        for name in TOOLS {
            tools.register(Box::new(HeldTool {
                name,
                calls: calls.clone(),
            }));
        }
        let runtime = Arc::new(
            CodeRuntime::new(
                db,
                dir.path().into(),
                Some(dir.path().join("worktrees")),
                None,
                None,
                None,
                None,
                None,
            )
            .with_tool_registry(Arc::new(tools)),
        );
        let mut session = crate::code::remote::fixtures::session_value();
        session.workspace_id = None;
        session.execution_location = ExecutionLocation::Sandbox;
        session.permission_mode = mode;
        insert_session(&runtime.db, &session).await.unwrap();
        if bound {
            let grant = mint_external_grant(
                &runtime.db,
                &session.owner,
                MintGrantSubject {
                    channel_kind: "slack",
                    external_identity: "fixture-user",
                    workspace_identity: "fixture-team",
                    kind: tidebreak_core::code::CodeGrantKind::Person,
                },
                &"a".repeat(64),
                &"b".repeat(64),
            )
            .await
            .unwrap();
            bind_external_session(
                &runtime.db,
                &session.owner,
                grant.id,
                "slack",
                "thread-1",
                session.id,
            )
            .await
            .unwrap();
        }
        let tidebreak_core::IncarnationAdmission::Admitted(incarnation) =
            create_incarnation_intent(&runtime.db, &session.owner, session.id, 1, 8)
                .await
                .unwrap()
        else {
            panic!("expected admission")
        };
        activate_incarnation(
            &runtime.db,
            &session.owner,
            incarnation.id,
            "fixture-sandbox",
        )
        .await
        .unwrap();
        (dir, runtime, session, incarnation.id, calls)
    }

    fn request() -> SupervisorToolRequest {
        SupervisorToolRequest {
            request_id: "call-1".into(),
            tool: "code_wait".into(),
            arguments: serde_json::json!({"session_ids":[]}),
        }
    }

    async fn completed(
        executor: &SandboxToolExecutor,
        session: &Session,
        incarnation: CodeIncarnationId,
    ) -> Vec<SupervisorToolResult> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let results = executor
                    .service(&session.owner, session.id, incarnation)
                    .await
                    .unwrap();
                if !results.is_empty() {
                    return results;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn pending_calls_do_not_block_pumps_and_replays_keep_the_same_call_id() {
        let (_dir, runtime, session, incarnation, calls) =
            fixture(true, PermissionMode::Allow).await;
        let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
        let request = request();
        executor
            .enqueue(&session.owner, session.id, incarnation, &request)
            .await
            .unwrap();
        executor
            .enqueue(&session.owner, session.id, incarnation, &request)
            .await
            .unwrap();
        let receipt =
            list_native_tool_requests(&runtime.db, &session.owner, session.id, incarnation)
                .await
                .unwrap()
                .remove(0);
        assert!(tokio::time::timeout(
            Duration::from_secs(1),
            executor.service(&session.owner, session.id, incarnation)
        )
        .await
        .unwrap()
        .unwrap()
        .is_empty());
        tokio::time::timeout(Duration::from_secs(1), calls.entered.notified())
            .await
            .unwrap();
        assert!(executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap()
            .is_empty());
        calls.release.notify_one();
        let first = completed(&executor, &session, incarnation).await;
        assert_eq!(first[0].output["content"], receipt.call_id.to_string());
        assert_eq!(
            executor
                .service(&session.owner, session.id, incarnation)
                .await
                .unwrap(),
            first
        );
        assert_eq!(calls.count.load(Ordering::SeqCst), 1);
        executor
            .mark_delivered(&session.owner, session.id, incarnation, "call-1")
            .await
            .unwrap();
        assert!(executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn overlapping_executors_wait_and_revocation_blocks_cached_delivery() {
        let (_dir, runtime, session, incarnation, calls) =
            fixture(true, PermissionMode::Allow).await;
        let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
        executor
            .enqueue(&session.owner, session.id, incarnation, &request())
            .await
            .unwrap();
        let receipt =
            list_native_tool_requests(&runtime.db, &session.owner, session.id, incarnation)
                .await
                .unwrap()
                .remove(0);
        assert!(matches!(
            claim_native_tool_request(&runtime.db, &session.owner, &receipt)
                .await
                .unwrap(),
            NativeToolClaim::Claimed(_)
        ));
        assert!(executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(calls.count.load(Ordering::SeqCst), 0);
        let cached = SupervisorToolResult::failed("call-1".into(), "fixture result");
        complete_native_tool_request(
            &runtime.db,
            &session.owner,
            &receipt,
            &serde_json::to_value(&cached).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            executor
                .service(&session.owner, session.id, incarnation)
                .await
                .unwrap(),
            vec![cached]
        );
        executor
            .authorize_delivery(&session.owner, session.id, incarnation, "call-1")
            .await
            .unwrap();
        let binding = list_bindings_for_session(&runtime.db, &session.owner, session.id)
            .await
            .unwrap()
            .remove(0);
        revoke_external_grant(
            &runtime.db,
            &session.owner,
            binding.grant_id,
            "fixture revocation",
        )
        .await
        .unwrap();
        assert!(executor
            .authorize_delivery(&session.owner, session.id, incarnation, "call-1")
            .await
            .is_err());
        assert!(executor
            .mark_delivered(&session.owner, session.id, incarnation, "call-1")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn ordinary_unbound_sandboxes_keep_working_and_bound_ask_mode_cannot_execute() {
        let (_dir, runtime, session, incarnation, _) = fixture(false, PermissionMode::Ask).await;
        let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
        assert!(executor
            .bootstrap_context(&session.owner, session.id)
            .await
            .unwrap()
            .is_empty());
        assert!(executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap()
            .is_empty());
        assert!(executor
            .enqueue(&session.owner, session.id, incarnation, &request())
            .await
            .is_err());
        let (_dir, runtime, session, incarnation, calls) = fixture(true, PermissionMode::Ask).await;
        let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
        assert!(executor
            .enqueue(&session.owner, session.id, incarnation, &request())
            .await
            .is_err());
        assert_eq!(calls.count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn execution_and_recovery_use_the_persisted_claim_time() {
        let now = chrono::Utc::now();
        assert_eq!(execution_remaining(Some(now), now), Some(EXECUTION_TIMEOUT));
        assert_eq!(
            execution_remaining(Some(now - chrono::Duration::seconds(60)), now),
            Some(Duration::from_secs(60))
        );
        assert!(execution_remaining(Some(now - chrono::Duration::seconds(120)), now).is_none());
        assert!(!recovery_due(
            Some(now - chrono::Duration::seconds(149)),
            now
        ));
        assert!(recovery_due(
            Some(now - chrono::Duration::seconds(150)),
            now
        ));
        assert!(execution_remaining(None, now).is_none());
        assert!(recovery_due(None, now));
    }

    #[tokio::test]
    async fn explicit_shutdown_aborts_workers_and_releases_the_runtime() {
        let (_dir, runtime, session, incarnation, calls) =
            fixture(true, PermissionMode::Allow).await;
        let weak = Arc::downgrade(&runtime);
        let executor = SandboxToolExecutor::new(weak.clone());
        executor
            .enqueue(&session.owner, session.id, incarnation, &request())
            .await
            .unwrap();
        executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), calls.entered.notified())
            .await
            .unwrap();
        executor.shutdown();
        drop(runtime);
        tokio::time::timeout(Duration::from_secs(1), async {
            while weak.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        calls.release.notify_one();
        assert_eq!(calls.completed.load(Ordering::SeqCst), 0);
        assert!(executor
            .enqueue(&session.owner, session.id, incarnation, &request())
            .await
            .is_err());
    }
}
