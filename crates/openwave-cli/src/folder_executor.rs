//! Headless executor for the client-executed connected-folder tools.
//!
//! `list_connected_folders`, `list_folder`, `read_connected_file`,
//! `import_connected_file`, and `write_output_to_connected_folder` are parked by
//! the server for a *trusted client* to run. On the desktop that client is the
//! native process (`openwave-desktop`'s `client_execution::folder_operations`).
//! A headless install had no such client at all, so a turn that used a folder
//! an operator had connected parked forever. This module is that client for the
//! processes that own broker state on this machine: `openwave serve`, and the
//! engine `openwave -p` and `openwave tui` embed.
//!
//! **It is not a second authority.** Every folder operation goes to
//! [`openwave_host_broker`]'s capability-checked operation surface, which
//! re-authorizes the conversation against its live grants before any byte is
//! read. This module chooses no path, caches no decision, and has no branch
//! that can answer a folder request from anything other than the broker's
//! answer.
//!
//! **It never widens a grant.** The tools here read folders that are *already*
//! connected. `request_folder_access` — the only tool that asks for a new one —
//! is deliberately not in this module's dispatch table; it keeps the typed
//! refusal [`crate::print`] gives it, and standing consent still comes only
//! from `openwave folder connect`. See [`crate::folder`].
//!
//! **Only the process that owns the broker runs it.** The executor needs the
//! server's client-executor credential, which [`crate::connect::Session`] holds
//! only when it embedded the engine. An attached (`--server`) run has no such
//! credential and starts no executor: the call belongs to whichever process owns
//! that server's host state, and now — if that process is `openwave serve`, an
//! embedded `openwave -p`, or `openwave tui` — it is actually answered there.
//! Each host operation opens the broker briefly and drops it, so operator
//! `openwave folder` provisioning can take `host-broker.lock` between calls
//! without stopping the daemon.
//!
//! ## Recovery
//!
//! The desktop's executor persists a receipt before it claims, because the
//! server hands a claimed call to no other executor: only the exact
//! `(executor_id, lease_token)` pair can ever recover or resolve it
//! (`claim_client_tool_call` refuses a second executor outright, expired lease
//! or not). Without a durable receipt a crash between claim and resolve would
//! strand the call permanently — the same hang this module exists to remove. So
//! it keeps the same receipt, in the same shape and with the same policy:
//!
//! - The lease token is chosen and written to disk *before* the claim, so a
//!   later run retries the exact claim rather than leaving a live lease nobody
//!   can operate.
//! - `dispatch_started` is written *before* any host I/O. After an
//!   interruption a read is terminalized with a typed failure rather than
//!   replayed — the broker keeps no read receipt to reconcile against, so a
//!   second read would be a second disclosure this process cannot account for.
//!   An import is the one operation that may be retried, because the server
//!   derives its document id from the request, so a repeat converges on the one
//!   source instead of adding another.
//! - The terminal outcome is written before it is published, so a crash in
//!   between republishes the same result instead of computing a new one.
//!
//! ## What headless cannot do, and says so
//!
//! `write_output_to_connected_folder` writes host bytes through the exec
//! write-overlay materializer, which a headless embedding does not have
//! (`bind_configured` installs no `ExecFolderGrantResolver`, so no connected
//! folder is ever staged). Rather than improvise a second write path with no
//! snapshot, no trash, and no undo, the executor fails the call closed with the
//! same `output_writeback_authority_unavailable` code the desktop produces when
//! its own materializer is missing. The turn continues; nothing is written.
//!
//! That same absence is why nothing here consults a staged folder view: with no
//! folder-grant resolver there is no per-turn copy for the broker's answer to
//! disagree with. An embedding that gains one must extend this module in the
//! same change, exactly as the desktop's executor had to.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use openwave_core::{
    validate_import_connected_file_arguments, validate_list_connected_folders_arguments,
    validate_list_folder_arguments, validate_read_connected_file_arguments,
    validate_write_output_to_connected_folder_arguments, AgentError, CallId, ChatId,
    GrantedFolderCapability, ImportConnectedFileArgs, ImportConnectedFileResult, ListFolderArgs,
    ReadConnectedFileArgs, Result, ResultEntry, ResultEntryKind, ToolCallExecution, ToolCallRecord,
    ToolCallStatus, IMPORT_CONNECTED_FILE_TOOL, LIST_CONNECTED_FOLDERS_TOOL, LIST_FOLDER_TOOL,
    READ_CONNECTED_FILE_TOOL, WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};
use openwave_host_broker::{
    Capability, EntryKind, ExecutionContext, OperationEnvelope, OperationRequest, OperationResult,
    PathRequest, RelativePath, RequestId, Response, RootId, PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::api::client::{Client, ClientExecutionOutcome};

mod receipts;

use receipts::{DispatchRecovery, Receipt, ReceiptStore};

/// How often the executor looks for parked folder work.
///
/// A print-mode run is waiting on the answer, so it polls tightly over the one
/// chat it drives. `serve` walks every conversation it can see and takes the
/// desktop executor's cadence.
const DRIVEN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const DAEMON_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Bounds on what one folder answer may carry back to the model. These match
/// the desktop executor's (`client_execution::folder_operations`) so a
/// conversation reads the same either side of the same data directory.
const MAX_DIRECTORY_ENTRIES: usize = 128;
const MAX_RESULT_CONTENT_BYTES: usize = 60 * 1024;
const MAX_FILE_CONTENT_BYTES: usize = 56 * 1024;

/// Scheme for the durable identity of a source imported from a connected root.
///
/// `{scheme}:{opaque root id}/{root-relative path}` — the pathless vocabulary
/// the broker's audit trail uses, never an absolute host path. The server
/// derives the document id from this string and the chat, so importing the same
/// file twice converges on one source. It is the desktop executor's scheme
/// verbatim: the two shells must name the same source for the same file.
const CONNECTED_FOLDER_URI_SCHEME: &str = "connected-folder";

/// Which conversations this executor answers for.
pub enum Scope {
    /// The one chat a print-mode run is driving.
    Chat(ChatId),
    /// Every conversation the daemon's own credential can see.
    AllChats,
}

/// The headless client executor.
pub struct FolderExecutor {
    client: Client,
    /// The server's native-only executor credential. Holding it is what makes
    /// this process the trusted surface for that server; an attached run has
    /// none and never constructs a `FolderExecutor`.
    executor_token: String,
    /// This profile's stable executor identity, shared with `openwave folder`.
    /// It must survive restarts: only the executor that claimed a call may
    /// recover it.
    executor_id: Uuid,
    receipts: ReceiptStore,
    data_dir: PathBuf,
}

impl FolderExecutor {
    /// Build an executor over an embedded engine, or `None` when this process is
    /// not the one that owns the server's host state.
    ///
    /// `executor_token` is the whole gate: [`crate::connect::Session`] returns
    /// it only for an embedded engine, and no flag hands it to an attaching
    /// client.
    pub fn new(
        client: Client,
        executor_token: Option<&str>,
        data_dir: &Path,
    ) -> Result<Option<Self>> {
        let Some(executor_token) = executor_token else {
            return Ok(None);
        };
        Ok(Some(Self {
            client,
            executor_token: executor_token.to_owned(),
            executor_id: crate::folder::executor_identity(data_dir)?,
            receipts: ReceiptStore::open(data_dir)?,
            data_dir: data_dir.to_path_buf(),
        }))
    }

    /// Poll for parked folder work until the task is dropped.
    ///
    /// Every failure is transient by construction: nothing is removed from the
    /// receipt store until the server has the terminal result, so the next pass
    /// picks the work up again. Diagnostics go to the profile's log file rather
    /// than stderr — this loop polls, and a repeating notice would flood a print
    /// run's stderr and overwrite the TUI's terminal.
    pub async fn run(self, scope: Scope) {
        let interval = match scope {
            Scope::Chat(_) => DRIVEN_POLL_INTERVAL,
            Scope::AllChats => DAEMON_POLL_INTERVAL,
        };
        loop {
            if let Err(error) = self.sweep(&scope).await {
                tracing::warn!(%error, "connected-folder work deferred");
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// One pass: finish what an earlier run left behind, then take up new work.
    ///
    /// One conversation's failure never hides another's: the daemon walks every
    /// chat it can see, and a chat that cannot be read this pass is reported and
    /// retried on the next.
    async fn sweep(&self, scope: &Scope) -> Result<()> {
        let recovered = self.recover().await?;
        for chat in self.chats(scope).await? {
            if let Err(error) = self.discover(chat, &recovered).await {
                tracing::warn!(%error, "connected-folder work deferred");
            }
        }
        Ok(())
    }

    async fn chats(&self, scope: &Scope) -> Result<Vec<ChatId>> {
        match scope {
            Scope::Chat(chat) => Ok(vec![*chat]),
            Scope::AllChats => Ok(self
                .client
                .list_chats()
                .await?
                .into_iter()
                .map(|chat| chat.id)
                .collect()),
        }
    }

    /// Drive every persisted receipt to a terminal result, and report which
    /// calls they covered so discovery cannot start a second attempt on one.
    async fn recover(&self) -> Result<HashSet<CallId>> {
        let receipts = self.receipts.load()?;
        let covered = receipts.iter().map(|receipt| receipt.call_id).collect();
        for receipt in receipts {
            if let Err(error) = self.execute(receipt).await {
                tracing::warn!(%error, "connected-folder recovery deferred");
            }
        }
        Ok(covered)
    }

    /// Claim and run the folder calls parked on one chat.
    async fn discover(&self, chat: ChatId, covered: &HashSet<CallId>) -> Result<()> {
        let pending = self
            .client
            .pending_client_executions(&self.executor_token, chat)
            .await?;
        for call in pending {
            if covered.contains(&call.id) || !handles(&call.name) {
                continue;
            }
            // Never race another executor. The server hands a claimed call to
            // nobody else, so a call already owned is either being run by its
            // owner or is that owner's to recover from its own receipt.
            if call.client_executor_id.is_some() {
                continue;
            }
            let receipt =
                Receipt::new(chat, call.id, self.executor_id, recovery_policy(&call.name));
            self.receipts.save(&receipt)?;
            if let Err(error) = self.execute(receipt).await {
                tracing::warn!(%error, "connected-folder execution deferred");
            }
        }
        Ok(())
    }

    /// Take one receipt from wherever it stopped to a published terminal result.
    async fn execute(&self, mut receipt: Receipt) -> Result<()> {
        if let Some(outcome) = receipt.outcome.clone() {
            return self.publish(&receipt, &outcome).await;
        }
        // Host I/O had started and its outcome was never recorded. For a read
        // there is nothing to reconcile against, so close the call out rather
        // than disclose the file a second time.
        if receipt.dispatch_started && receipt.recovery == DispatchRecovery::Terminalize {
            return self.terminalize(&mut receipt, interrupted()).await;
        }

        let claim = match self
            .client
            .claim_client_execution(
                &self.executor_token,
                receipt.chat_id,
                receipt.call_id,
                receipt.executor_id,
                receipt.lease_token,
            )
            .await
        {
            Ok(claim) => claim,
            Err(error) if error.is_conflict() => {
                return self.recover_after_claim_conflict(&receipt).await;
            }
            Err(error) => return Err(error.into()),
        };
        // The claim response is the authoritative statement of what was claimed.
        // Canonical arguments come from it — the checkpointed call the server
        // holds — and never from a restatement by whoever announced the work.
        // The recovery policy is checked against that same record rather than
        // the name discovery saw, so the crash rules can never be applied to a
        // different operation than the one that runs.
        let call = &claim.call;
        if call.chat_id != receipt.chat_id
            || call.id != receipt.call_id
            || claim.lease_token != receipt.lease_token
            || call.client_executor_id != Some(receipt.executor_id)
            || !is_canonical_folder_call(call)
            || recovery_policy(&call.name) != receipt.recovery
        {
            return Err(AgentError::msg(
                "the server returned an invalid connected-folder claim",
            ));
        }

        self.client
            .heartbeat_client_execution(
                &self.executor_token,
                receipt.chat_id,
                receipt.call_id,
                receipt.lease_token,
            )
            .await?;
        // The last durable fence before host I/O.
        receipt.dispatch_started = true;
        self.receipts.save(&receipt)?;
        let outcome = self.run_call(receipt.chat_id, call).await;
        self.terminalize(&mut receipt, outcome).await
    }

    /// The claim was refused. Decide from the server's own pending set whether
    /// this receipt still describes live work.
    ///
    /// A conflict is not proof the claim was lost: the route answers it whenever
    /// the call is not claimable *right now*, which includes a conversation
    /// another writer momentarily holds. So the pending set decides, and the
    /// default is to keep the receipt:
    ///
    /// - Gone from the pending set: the call is settled and the receipt is spent.
    /// - Parked and owned by a different executor: that executor's to finish,
    ///   and nothing here may run or resolve it. Only this case discards a
    ///   receipt without settling the call, and only because it now belongs to
    ///   somebody else.
    /// - Parked and still this executor's — or still unclaimed: the refusal was
    ///   transient. The receipt is kept so the next pass retries the exact claim,
    ///   with `dispatch_started` still deciding whether the operation may run.
    async fn recover_after_claim_conflict(&self, receipt: &Receipt) -> Result<()> {
        let pending = self
            .client
            .pending_client_executions(&self.executor_token, receipt.chat_id)
            .await?;
        let Some(call) = pending.into_iter().find(|call| call.id == receipt.call_id) else {
            return self.receipts.remove(receipt.call_id);
        };
        if call
            .client_executor_id
            .is_some_and(|owner| owner != receipt.executor_id)
        {
            self.receipts.remove(receipt.call_id)?;
            return Err(AgentError::msg(
                "the connected-folder call is owned by another executor",
            ));
        }
        Err(AgentError::msg(
            "the connected-folder claim could not be taken yet",
        ))
    }

    async fn terminalize(
        &self,
        receipt: &mut Receipt,
        outcome: ClientExecutionOutcome,
    ) -> Result<()> {
        receipt.outcome = Some(outcome.clone());
        self.receipts.save(receipt)?;
        self.publish(receipt, &outcome).await
    }

    /// Hand the terminal result to the server, then forget the receipt.
    ///
    /// A conflict is only final once the call is no longer parked: the route
    /// answers it both for a different terminal result already committed and for
    /// a conversation another writer holds. Dropping the receipt on the second
    /// would strand the call for good, so the pending set decides again.
    async fn publish(&self, receipt: &Receipt, outcome: &ClientExecutionOutcome) -> Result<()> {
        match self
            .client
            .resolve_client_execution(
                &self.executor_token,
                receipt.chat_id,
                receipt.call_id,
                receipt.lease_token,
                outcome,
            )
            .await
        {
            Ok(()) => self.receipts.remove(receipt.call_id),
            Err(error) if error.is_conflict() => {
                let still_parked = self
                    .client
                    .pending_client_executions(&self.executor_token, receipt.chat_id)
                    .await?
                    .into_iter()
                    .any(|call| call.id == receipt.call_id);
                if still_parked {
                    Err(AgentError::msg(
                        "the connected-folder result was not accepted yet",
                    ))
                } else {
                    self.receipts.remove(receipt.call_id)
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Run one claimed call, returning the model-facing terminal outcome.
    async fn run_call(&self, chat: ChatId, call: &ToolCallRecord) -> ClientExecutionOutcome {
        // The one call a headless install cannot honor. It needs the exec
        // write-overlay materializer, which no headless embedding installs, so
        // it fails closed with the desktop's own code for that state rather than
        // reaching the host by another route. Decided before any authority is
        // derived, because none of it applies.
        if call.name == WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL {
            return ClientExecutionOutcome::Failed {
                result: "That output could not be written to the connected folder.".to_owned(),
                error_code: "output_writeback_authority_unavailable".to_owned(),
                error_detail: None,
            };
        }
        // Derived here, from the conversation's current authority, immediately
        // before the broker is asked. It is never carried across calls.
        let context = match self.execution_context(chat).await {
            Ok(context) => context,
            Err(_) => return folder_unavailable(),
        };
        if call.name == IMPORT_CONNECTED_FILE_TOOL {
            return self.import(chat, context, call).await;
        }
        let Ok(request) = broker_request(call) else {
            return unavailable("invalid_request", "The folder request was not available.");
        };
        match self.broker_operation(context, request).await {
            Ok(result) => serialize_result(call, result),
            // Broker transport and host errors deliberately do not reach the
            // model: authorization is the broker's to state, and everything else
            // is host detail.
            Err(_) => folder_unavailable(),
        }
    }

    /// Import one connected file as a conversation source.
    ///
    /// The bytes are read through the broker (so the read is authorized at read
    /// time) and published through the same ingest route `openwave attach` and
    /// the desktop use. The server derives the document id from the origin URI,
    /// which is what makes a repeated import recover the one source.
    async fn import(
        &self,
        chat: ChatId,
        context: ExecutionContext,
        call: &ToolCallRecord,
    ) -> ClientExecutionOutcome {
        let Ok(args) = serde_json::from_value::<ImportConnectedFileArgs>(call.arguments.clone())
        else {
            return import_unavailable("That file could not be read.");
        };
        let Ok((root_id, path)) = root_and_path(args.root_id, &args.path, false) else {
            return import_unavailable("That file could not be read.");
        };
        let Some(title) = path.as_str().rsplit('/').next().map(str::to_owned) else {
            return import_unavailable("That file could not be read.");
        };
        let request = OperationRequest::ReadFileBinary(PathRequest {
            root_id,
            path: path.clone(),
        });
        let bytes = match self.broker_operation(context, request).await {
            Ok(OperationResult::ReadFileBinary(file)) => {
                use base64::Engine as _;

                match base64::engine::general_purpose::STANDARD.decode(&file.content_base64) {
                    Ok(bytes) => bytes,
                    Err(_) => return import_unavailable("That file could not be read."),
                }
            }
            Ok(_) => return import_unavailable("That file could not be read."),
            Err(_) => {
                return import_unavailable(
                    "That file is not available to this conversation. It may be too large, or \
                     the folder may no longer be connected.",
                )
            }
        };
        if bytes.is_empty() {
            return import_unavailable("That file is empty, so there is nothing to import.");
        }

        let media_type =
            openwave_server::media_type::sniff_media_type(&bytes, Some(title.as_str()));
        let byte_len = bytes.len() as u64;
        let uri = format!(
            "{CONNECTED_FOLDER_URI_SCHEME}:{}/{}",
            root_id.as_uuid(),
            path.as_str()
        );
        // The ingest route owns title policy and refuses one it will not store,
        // so a host filename never becomes display text by passing through here.
        let Ok(ingested) = self
            .client
            .publish_document_source(chat, Some(title.as_str()), Some(&uri), &media_type, bytes)
            .await
        else {
            return import_unavailable("That file could not be added to this conversation.");
        };
        let result = ImportConnectedFileResult::Imported {
            document_id: ingested.document_id,
            title: title.clone(),
            media_type,
            bytes: byte_len,
            readiness: ingested.readiness,
        };
        match serde_json::to_string(&result) {
            Ok(result) => ClientExecutionOutcome::Completed {
                result,
                rows: Some(serde_json::json!({
                    "entries": [ResultEntry::new(ResultEntryKind::Source, title)],
                })),
            },
            Err(_) => import_unavailable("That file could not be added to this conversation."),
        }
    }

    /// Ask the broker for one operation, on the conversation's own authority.
    ///
    /// This is the only path to a connected folder in this module. The broker
    /// authorizes the request against the conversation's live grants, opens the
    /// root through its own pinned descriptor, and re-authorizes after the read
    /// — none of which this process can influence, skip, or cache.
    ///
    /// The broker handle is opened for this operation only and dropped before
    /// return, so `host-broker.lock` is free for `openwave folder` provisioning
    /// between tool calls. Holding it for the life of `serve` would pin the
    /// lock under the daemon and make operator connect refuse forever.
    async fn broker_operation(
        &self,
        context: ExecutionContext,
        request: OperationRequest,
    ) -> Result<OperationResult> {
        let data_dir = self.data_dir.clone();
        let envelope = OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            context,
            request,
        };
        // The in-process broker does real, blocking host I/O. Open and drop
        // inside the blocking task so the lock never outlives the operation.
        let response = tokio::task::spawn_blocking(move || {
            let broker = crate::folder::open_broker(&data_dir)?;
            Ok::<_, AgentError>(broker.operator().handle(envelope).response)
        })
        .await
        .map_err(|_| AgentError::msg("the connected-folder operation did not complete"))??;
        match response {
            Response::Ok(result) => Ok(result),
            Response::Error(error) => Err(AgentError::msg(format!(
                "connected-folder operation refused ({:?})",
                error.code
            ))),
        }
    }

    /// The broker execution context a conversation's folder reads run under.
    ///
    /// A chat inside a project reads on that project's standing consent; a loose
    /// chat reads on its own. This is the desktop's derivation, read live from
    /// the chat rather than remembered, so a chat moved between projects cannot
    /// carry the old authority into a later call.
    async fn execution_context(&self, chat: ChatId) -> Result<ExecutionContext> {
        let summary = self.client.get_chat(chat).await?;
        match summary.project_id {
            Some(project) => ExecutionContext::project_chat(chat.0, project.0),
            None => ExecutionContext::standalone(chat.0),
        }
        .map_err(|_| AgentError::msg("invalid conversation context"))
    }
}

/// Whether this executor answers for a tool.
///
/// `request_folder_access` is deliberately absent: it asks for consent this
/// process cannot give, and print mode's typed refusal remains the only answer
/// a headless run has for it.
fn handles(name: &str) -> bool {
    matches!(
        name,
        LIST_CONNECTED_FOLDERS_TOOL
            | LIST_FOLDER_TOOL
            | READ_CONNECTED_FILE_TOOL
            | IMPORT_CONNECTED_FILE_TOOL
            | WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL
    )
}

/// Recovery policy for one connected-folder tool, matching the desktop's.
///
/// A read has no durable host outcome to reconcile against, so an interrupted
/// one is closed out. An import derives its source identity from the exact
/// request, so running it again converges on the same single source. A
/// write-back never reaches the host from here at all, so replaying its
/// fail-closed answer is free.
fn recovery_policy(name: &str) -> DispatchRecovery {
    match name {
        IMPORT_CONNECTED_FILE_TOOL | WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL => {
            DispatchRecovery::Retry
        }
        _ => DispatchRecovery::Terminalize,
    }
}

/// Whether a claimed record really is the pending, well-formed folder call this
/// executor may run. The argument validators are the same ones the server
/// applied before the call was checkpointed, re-run here so a stored payload
/// that could no longer pass them never reaches a host operation.
fn is_canonical_folder_call(call: &ToolCallRecord) -> bool {
    if call.execution != ToolCallExecution::Client || call.status != ToolCallStatus::Pending {
        return false;
    }
    match call.name.as_str() {
        LIST_CONNECTED_FOLDERS_TOOL => validate_list_connected_folders_arguments(&call.arguments),
        LIST_FOLDER_TOOL => validate_list_folder_arguments(&call.arguments),
        READ_CONNECTED_FILE_TOOL => validate_read_connected_file_arguments(&call.arguments),
        IMPORT_CONNECTED_FILE_TOOL => validate_import_connected_file_arguments(&call.arguments),
        WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL => {
            validate_write_output_to_connected_folder_arguments(&call.arguments)
        }
        _ => false,
    }
}

/// Recover the broker request behind one read-shaped folder call.
///
/// The path grammar is parsed by the broker's own [`RelativePath`], so a payload
/// the broker would refuse is refused here instead of being handed to it.
fn broker_request(call: &ToolCallRecord) -> std::result::Result<OperationRequest, ()> {
    match call.name.as_str() {
        LIST_CONNECTED_FOLDERS_TOOL => Ok(OperationRequest::ListRoots),
        LIST_FOLDER_TOOL => {
            let args: ListFolderArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| ())?;
            let (root_id, path) = root_and_path(args.root_id, &args.path, true)?;
            Ok(OperationRequest::ListDirectory(PathRequest {
                root_id,
                path,
            }))
        }
        READ_CONNECTED_FILE_TOOL => {
            let args: ReadConnectedFileArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| ())?;
            let (root_id, path) = root_and_path(args.root_id, &args.path, false)?;
            Ok(OperationRequest::ReadFile(PathRequest { root_id, path }))
        }
        _ => Err(()),
    }
}

/// Parse a model-proposed root and path into the broker's own vocabulary.
/// `allow_root` distinguishes a directory listing, which may name the root
/// itself, from a file operation, which may not.
fn root_and_path(
    root_id: Uuid,
    path: &str,
    allow_root: bool,
) -> std::result::Result<(RootId, RelativePath), ()> {
    let root_id = RootId::from_uuid(root_id).map_err(|_| ())?;
    let path = RelativePath::parse(path).map_err(|_| ())?;
    if path.is_root() && !allow_root {
        return Err(());
    }
    Ok((root_id, path))
}

/// Project one broker result into the model-facing payload and the card's rows.
///
/// This mirrors the desktop executor's `serialize_result`
/// (`openwave-desktop/src/client_execution/folder_operations.rs`) field for
/// field and bound for bound: one conversation may be opened by either shell
/// against the same data directory, and a `list_folder` that answered
/// differently in each would be a defect in whichever one the reader was not
/// using. A change to either payload is a change to both.
fn serialize_result(call: &ToolCallRecord, result: OperationResult) -> ClientExecutionOutcome {
    // The rows are built from the same values the model-facing payload is, so
    // the card and the model can never disagree about what the folder held.
    let (result, rows) = match result {
        OperationResult::ListRoots { roots } => {
            let roots: Vec<_> = roots.into_iter().take(MAX_DIRECTORY_ENTRIES).collect();
            let rows = roots
                .iter()
                .map(|root| ResultEntry::new(ResultEntryKind::Folder, root.display_name.clone()))
                .collect::<Vec<_>>();
            (
                serde_json::json!({
                    "status": "ok",
                    "folders": roots.iter().map(|root| serde_json::json!({
                        "root_id": root.root_id,
                        "display_name": root.display_name,
                        "capabilities": granted_folder_capabilities(&root.capabilities),
                    })).collect::<Vec<_>>(),
                }),
                rows,
            )
        }
        OperationResult::ListDirectory { entries } => {
            let entries: Vec<_> = entries.into_iter().take(MAX_DIRECTORY_ENTRIES).collect();
            let rows = entries
                .iter()
                .map(|entry| {
                    let kind = if entry.kind == EntryKind::Directory {
                        ResultEntryKind::Folder
                    } else {
                        ResultEntryKind::File
                    };
                    ResultEntry::new(kind, entry.name.clone())
                })
                .collect();
            (
                serde_json::json!({
                    "status": "ok",
                    "entries": entries.iter().map(|entry| serde_json::json!({
                        "name": entry.name,
                        "kind": entry.kind,
                    })).collect::<Vec<_>>(),
                }),
                rows,
            )
        }
        OperationResult::ReadFile(file) => {
            let (content, truncated) = truncate_utf8(&file.content, MAX_FILE_CONTENT_BYTES);
            // The file's text is what the model reads and far too much for a
            // card, so the row reports the read rather than replaying it. The
            // name comes from the request: a read result is bytes, and only the
            // arguments say which file they came from.
            let path = serde_json::from_value::<ReadConnectedFileArgs>(call.arguments.clone())
                .map(|args| args.path)
                .unwrap_or_default();
            let mut row = ResultEntry::new(ResultEntryKind::File, read_file_name(&path))
                .with_meta(openwave_core::format_bytes(content.len() as u64));
            if truncated {
                row = row.with_detail("truncated at the read limit");
            }
            (
                serde_json::json!({
                    "status": "ok",
                    "content": content,
                    "truncated": truncated,
                }),
                vec![row],
            )
        }
        _ => {
            return unavailable(
                "unsupported_result",
                "The connected-folder operation is not available.",
            )
        }
    };
    match serde_json::to_string(&result) {
        Ok(result) if result.len() <= MAX_RESULT_CONTENT_BYTES => {
            ClientExecutionOutcome::Completed {
                result,
                rows: Some(serde_json::json!({ "entries": rows })),
            }
        }
        _ => unavailable(
            "result_too_large",
            "The connected-folder result was too large to return.",
        ),
    }
}

/// The model-facing names for what the broker currently grants on a folder.
fn granted_folder_capabilities(capabilities: &[Capability]) -> Vec<GrantedFolderCapability> {
    capabilities
        .iter()
        .filter_map(|capability| match capability {
            Capability::ReadFiles => Some(GrantedFolderCapability::ReadFiles),
            Capability::WriteFiles => Some(GrantedFolderCapability::WriteFiles),
            Capability::ExecuteCommands => Some(GrantedFolderCapability::ExecuteCommands),
            Capability::ListRoots => None,
            // `Capability` is non-exhaustive. Unknown future per-folder reach is
            // intentionally under-reported until this conversion and the
            // model-facing vocabulary are extended together.
            _ => None,
        })
        .collect()
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

/// The last segment of a connected-folder path, so a row leads with the file.
fn read_file_name(path: &str) -> String {
    let name = path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(path);
    if name.is_empty() {
        "file".to_owned()
    } else {
        name.to_owned()
    }
}

fn unavailable(code: &str, message: &str) -> ClientExecutionOutcome {
    ClientExecutionOutcome::Failed {
        result: serde_json::json!({ "status": "unavailable", "message": message }).to_string(),
        error_code: code.to_owned(),
        error_detail: None,
    }
}

fn folder_unavailable() -> ClientExecutionOutcome {
    unavailable(
        "folder_unavailable",
        "That connected folder is no longer available to this conversation. You can ask the user \
         to connect a folder again.",
    )
}

fn import_unavailable(message: &str) -> ClientExecutionOutcome {
    let result = ImportConnectedFileResult::Unavailable {
        message: message.to_owned(),
    };
    ClientExecutionOutcome::Failed {
        result: serde_json::to_string(&result)
            .unwrap_or_else(|_| r#"{"status":"unavailable","message":"unavailable"}"#.to_owned()),
        error_code: "import_unavailable".to_owned(),
        error_detail: None,
    }
}

fn interrupted() -> ClientExecutionOutcome {
    unavailable(
        "folder_operation_interrupted",
        "The folder operation could not be safely resumed after an interruption. Please try again.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::{TurnId, REQUEST_FOLDER_ACCESS_TOOL};

    fn call(name: &str, arguments: serde_json::Value) -> ToolCallRecord {
        ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: TurnId::new(),
            provider_id: "tool-1".into(),
            name: name.into(),
            arguments,
            raw_arguments: None,
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        }
    }

    /// The boundary between "a folder this conversation already has" and "ask
    /// for another one". A headless run answers the first and must never answer
    /// the second: `request_folder_access` has no route into this executor, so
    /// print mode's typed refusal stays the only answer to it and no folder
    /// grant can be created or widened from a parked call.
    #[test]
    fn the_executor_never_answers_a_request_for_new_folder_access() {
        assert!(!handles(REQUEST_FOLDER_ACCESS_TOOL));
        assert!(!is_canonical_folder_call(&call(
            REQUEST_FOLDER_ACCESS_TOOL,
            serde_json::json!({
                "reason": "Read reports",
                "requested_capabilities": ["read_files"],
            }),
        )));
        // Nor by any other name: dispatch is a closed match, and a name it does
        // not know produces no broker request at all.
        assert!(broker_request(&call("exec", serde_json::json!({}))).is_err());
        for name in [
            LIST_CONNECTED_FOLDERS_TOOL,
            LIST_FOLDER_TOOL,
            READ_CONNECTED_FILE_TOOL,
            IMPORT_CONNECTED_FILE_TOOL,
            WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
        ] {
            assert!(handles(name), "{name}");
        }
    }

    /// Model-proposed paths reach the broker only in the broker's own grammar.
    /// Anything the broker would refuse — traversal, an absolute path, a root
    /// where a file is required — is refused before a host operation exists,
    /// and an import is never routed through the read dispatch table.
    #[test]
    fn a_path_the_broker_would_refuse_never_becomes_a_host_operation() {
        let root = Uuid::new_v4();
        for path in ["../secret", "reports/../secret", "/etc/passwd", "a\\b"] {
            assert!(
                broker_request(&call(
                    LIST_FOLDER_TOOL,
                    serde_json::json!({ "root_id": root, "path": path })
                ))
                .is_err(),
                "{path}"
            );
            assert!(root_and_path(root, path, true).is_err(), "{path}");
        }
        // A listing may name the folder itself; a file read may not.
        assert!(broker_request(&call(
            LIST_FOLDER_TOOL,
            serde_json::json!({ "root_id": root, "path": "" })
        ))
        .is_ok());
        assert!(broker_request(&call(
            READ_CONNECTED_FILE_TOOL,
            serde_json::json!({ "root_id": root, "path": "" })
        ))
        .is_err());
        // A nil root id is not a root.
        assert!(root_and_path(Uuid::nil(), "notes.md", false).is_err());
        // An import is resolved by its own path, not this table.
        assert!(broker_request(&call(
            IMPORT_CONNECTED_FILE_TOOL,
            serde_json::json!({ "root_id": root, "path": "reports/q3.pdf" })
        ))
        .is_err());
    }

    /// Every terminal answer is bounded and carries no host detail: not the
    /// path, not an OS error, not the file's bytes in the card.
    #[test]
    fn terminal_answers_are_bounded_and_free_of_host_detail() {
        let read = call(
            READ_CONNECTED_FILE_TOOL,
            serde_json::json!({ "root_id": Uuid::new_v4(), "path": "reports/q3.md" }),
        );
        let outcome = serialize_result(
            &read,
            OperationResult::ReadFile(openwave_host_broker::ReadFileResult {
                content: "x".repeat(MAX_FILE_CONTENT_BYTES + 1),
                bytes: MAX_FILE_CONTENT_BYTES + 1,
            }),
        );
        let ClientExecutionOutcome::Completed { result, rows } = outcome else {
            panic!("a bounded read completes");
        };
        assert!(result.len() <= MAX_RESULT_CONTENT_BYTES);
        assert!(result.contains("\"truncated\":true"));
        let rows = rows.expect("a completed read reports its row");
        assert_eq!(rows["entries"][0]["label"], "q3.md");
        assert_eq!(rows["entries"][0]["detail"], "truncated at the read limit");
        // The card reports the read rather than replaying the file.
        assert!(!rows.to_string().contains('x'));

        for failure in [
            interrupted(),
            folder_unavailable(),
            import_unavailable("That file could not be read."),
        ] {
            let ClientExecutionOutcome::Failed {
                result,
                error_code,
                error_detail,
            } = failure
            else {
                panic!("a refusal is terminal");
            };
            assert!(!error_code.is_empty());
            assert!(
                error_detail.is_none(),
                "no host diagnostics reach the model"
            );
            assert!(!result.contains('/'), "{result}");
        }
    }

    /// The recovery table this module's crash safety rests on. A read that may
    /// already have reached the broker is closed out rather than repeated; an
    /// import, whose source identity the server derives from the request, may
    /// converge on the same source by running again.
    #[test]
    fn only_a_convergent_operation_may_be_repeated_after_an_interruption() {
        for read_only in [
            LIST_CONNECTED_FOLDERS_TOOL,
            LIST_FOLDER_TOOL,
            READ_CONNECTED_FILE_TOOL,
        ] {
            assert_eq!(
                recovery_policy(read_only),
                DispatchRecovery::Terminalize,
                "{read_only}"
            );
        }
        assert_eq!(
            recovery_policy(IMPORT_CONNECTED_FILE_TOOL),
            DispatchRecovery::Retry
        );
    }

    /// The folder listing must tell the model what the broker actually grants,
    /// because that is what decides whether it tries to write or execute. The
    /// vocabulary is the model-facing one, not the broker's internal set.
    #[test]
    fn the_folder_listing_reports_the_brokers_own_capabilities() {
        let outcome = serialize_result(
            &call(LIST_CONNECTED_FOLDERS_TOOL, serde_json::json!({})),
            OperationResult::ListRoots {
                roots: vec![openwave_host_broker::RootAccess {
                    root_id: RootId::new(),
                    display_name: "reports".into(),
                    capabilities: vec![
                        Capability::ListRoots,
                        Capability::ReadFiles,
                        Capability::WriteFiles,
                        Capability::ExecuteCommands,
                    ],
                }],
            },
        );
        let ClientExecutionOutcome::Completed { result, .. } = outcome else {
            panic!("a listing completes");
        };
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            result["folders"][0]["capabilities"],
            serde_json::json!(["read_files", "write_files", "execute_commands"])
        );
    }
}
