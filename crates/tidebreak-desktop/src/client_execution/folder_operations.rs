//! Durable native executor for foreground inspection of already connected folders.
//!
//! The renderer never supplies an execution context, host path, grant, or
//! broker handle. Canonical tool arguments are recovered from the checkpointed
//! server call, while the native executor derives current chat authority before
//! each broker operation.

use std::collections::HashSet;

use tauri::Manager;
use tidebreak_code_execution::host_paths::{resolve_scratch_directory, ScratchEntryKind};
use tidebreak_core::{
    validate_import_connected_file_arguments, validate_list_connected_folders_arguments,
    validate_list_folder_arguments, validate_read_connected_file_arguments, CallId,
    GrantedFolderCapability, ListFolderArgs, ReadConnectedFileArgs, ResultEntry, ResultEntryKind,
    ToolCallExecution, ToolCallRecord, ToolCallStatus, IMPORT_CONNECTED_FILE_TOOL,
    LIST_CONNECTED_FOLDERS_TOOL, LIST_FOLDER_TOOL, READ_CONNECTED_FILE_TOOL,
};
use tidebreak_host_broker::{
    Capability, DirectoryEntry, EntryKind, OperationEnvelope, OperationRequest, OperationResult,
    PathRequest, ReadFileResult, RelativePath, RootId, PROTOCOL_VERSION,
};

use crate::host_access::{AuthoritativeContext, HostAccess};

use super::{
    control_plane, control_plane_error, private_receipt_error, source_import, DispatchRecovery,
    FolderOperationPhase, FolderOperationReceipt, StoredResolution,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const MAX_DIRECTORY_ENTRIES: usize = 128;
const MAX_RESULT_CONTENT_BYTES: usize = 60 * 1024;
const MAX_FILE_CONTENT_BYTES: usize = 56 * 1024;
/// Ceiling on a file read out of a staged folder, before the result is trimmed
/// to [`MAX_FILE_CONTENT_BYTES`]. It exists so a large file the agent staged is
/// refused rather than buffered whole to be thrown away.
const MAX_STAGED_READ_BYTES: u64 = 4 * 1024 * 1024;

/// Recover persisted outcomes, then discover new matching client calls. The
/// loop is deliberately native-owned: no renderer event or user action is an
/// execution authority.
pub(crate) async fn recover_connected_folder_operations(app: tauri::AppHandle) {
    loop {
        let failed = recover_once(&app).await;
        if failed {
            eprintln!("tidebreak-desktop: connected-folder executor deferred work");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn recover_once(app: &tauri::AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let receipts = match state.receipts.load_operations() {
        Ok(receipts) => receipts,
        Err(error) => {
            eprintln!("tidebreak-desktop: connected-folder receipt recovery failed: {error}");
            return true;
        }
    };
    let recovered_call_ids: HashSet<CallId> =
        receipts.iter().map(|receipt| receipt.call_id).collect();
    let mut failed = false;
    for receipt in receipts {
        if let Err(error) = execute_receipt(app, &state, receipt).await {
            eprintln!("tidebreak-desktop: connected-folder receipt deferred: {error}");
            failed = true;
        }
    }

    let Some(store) = state.store() else {
        return true;
    };
    let client = match control_plane(&state) {
        Ok(client) => client,
        Err(_) => return true,
    };
    let chats = match store.list_chats().await {
        Ok(chats) => chats,
        Err(_) => return true,
    };
    for chat in chats {
        let calls = match client.pending(chat.id).await {
            Ok(calls) => calls,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        for call in calls
            .into_iter()
            .filter(|call| should_discover_call(call, &recovered_call_ids))
        {
            let receipt = FolderOperationReceipt::new(
                chat.id,
                call.id,
                state.receipts.executor_id(),
                dispatch_recovery(&call.name),
            );
            if let Err(error) = execute_receipt(app, &state, receipt).await {
                eprintln!("tidebreak-desktop: connected-folder execution deferred: {error}");
                failed = true;
            }
        }
    }
    failed
}

fn should_discover_call(call: &ToolCallRecord, recovered_call_ids: &HashSet<CallId>) -> bool {
    !recovered_call_ids.contains(&call.id) && is_connected_folder_call(call)
}

async fn execute_receipt(
    app: &tauri::AppHandle,
    state: &HostAccess,
    mut receipt: FolderOperationReceipt,
) -> Result<(), String> {
    if let Some(resolution) = receipt.resolution.clone() {
        return publish_resolution(state, &receipt, &resolution).await;
    }
    if dispatch_is_ambiguous(&receipt) {
        return terminalize_interrupted_dispatch(state, &mut receipt).await;
    }

    let context = state.context(receipt.chat_id.0).await?;

    // Persist the chosen lease token before claiming. If a response is lost,
    // recovery retries the exact claim rather than leaving a live lease with no
    // local recovery authority.
    state
        .receipts
        .save_operation(&receipt)
        .map_err(private_receipt_error)?;
    let client = control_plane(state)?;
    let claim = match client
        .claim(
            receipt.chat_id,
            receipt.call_id,
            receipt.executor_id,
            receipt.lease_token,
        )
        .await
    {
        Ok(claim) => claim,
        Err(error) if error.is_conflict() => {
            return recover_after_claim_conflict(state, &mut receipt).await;
        }
        Err(error) => return Err(control_plane_error(error)),
    };
    if claim.call.chat_id != receipt.chat_id
        || claim.call.id != receipt.call_id
        || claim.lease_token != receipt.lease_token
        || claim.call.client_executor_id != Some(receipt.executor_id)
        || !is_connected_folder_call(&claim.call)
    {
        return Err("local control plane returned an invalid connected-folder request".to_owned());
    }

    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    // This is the final durable fence before host I/O. If the process stops
    // afterwards, a read may have reached the broker, so recovery must publish
    // the safe interrupted result rather than issue another request.
    receipt.phase = FolderOperationPhase::DispatchStarted;
    state
        .receipts
        .save_operation(&receipt)
        .map_err(private_receipt_error)?;
    let resolution = execute_operation(app, state, context, &claim.call).await;
    receipt.resolution = Some(resolution);
    state
        .receipts
        .save_operation(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        &receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

/// Whether an interrupted dispatch must be terminalized instead of retried.
///
/// A read has no durable outcome to reconcile against, so recovery closes it
/// out. An import derives its source identity from the exact request, so
/// running it again converges on the same single source.
fn dispatch_is_ambiguous(receipt: &FolderOperationReceipt) -> bool {
    receipt.phase == FolderOperationPhase::DispatchStarted
        && receipt.resolution.is_none()
        && receipt.recovery == DispatchRecovery::Terminalize
}

/// Recovery policy for one connected-folder tool.
fn dispatch_recovery(name: &str) -> DispatchRecovery {
    match name {
        IMPORT_CONNECTED_FILE_TOOL => DispatchRecovery::Retry,
        _ => DispatchRecovery::Terminalize,
    }
}

async fn terminalize_interrupted_dispatch(
    state: &HostAccess,
    receipt: &mut FolderOperationReceipt,
) -> Result<(), String> {
    receipt.resolution = Some(interrupted_resolution());
    state
        .receipts
        .save_operation(receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

/// A read may have reached the broker before a desktop process died, but the
/// broker deliberately has no read-result receipt to replay. If the persisted
/// client claim can no longer be recovered, terminalize with the same exact
/// lease token instead of issuing another ambiguous read. The server accepts
/// that token through its expired-claim resolution path only after the lease is
/// actually expired.
///
/// This holds even for an operation whose [`DispatchRecovery`] permits a retry.
/// Once the lease is gone another executor may own the call, so running it again
/// here would race that owner. An import terminalized this way is still only one
/// model retry away from the same single source, because its identity is derived
/// from the request rather than from this attempt.
async fn recover_after_claim_conflict(
    state: &HostAccess,
    receipt: &mut FolderOperationReceipt,
) -> Result<(), String> {
    let client = control_plane(state)?;
    let pending = client
        .pending(receipt.chat_id)
        .await
        .map_err(control_plane_error)?;
    let Some(call) = pending.into_iter().find(|call| call.id == receipt.call_id) else {
        return state
            .receipts
            .remove_operation(receipt.call_id)
            .map_err(private_receipt_error);
    };
    if call.chat_id != receipt.chat_id || !is_connected_folder_call(&call) {
        return Err("local control plane returned an invalid connected-folder request".to_owned());
    }
    if call.client_executor_id != Some(receipt.executor_id) {
        return state
            .receipts
            .remove_operation(receipt.call_id)
            .map_err(private_receipt_error);
    }

    terminalize_interrupted_dispatch(state, receipt).await
}

async fn execute_operation(
    app: &tauri::AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
) -> StoredResolution {
    if call.name == IMPORT_CONNECTED_FILE_TOOL {
        let Ok(request) = source_import::parse(call) else {
            return unavailable("invalid_request", "The folder request was not available.");
        };
        return source_import::execute(state, app, context, &request).await;
    }
    let request = match broker_request(call) {
        Ok(request) => request,
        Err(()) => return unavailable("invalid_request", "The folder request was not available."),
    };
    match staged_outcome(state, context, &request).await {
        StagedOutcome::Unstaged => {}
        StagedOutcome::Result(result) => return serialize_result(call, result),
        StagedOutcome::Missing => {
            return unavailable(
                "path_not_found",
                "That path is not in the connected folder.",
            )
        }
    }
    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: tidebreak_host_broker::RequestId::new(),
            context: context.execution,
            request,
        })
        .await;
    match result {
        Ok(result) => serialize_result(call, result),
        // Broker transport errors are intentionally not surfaced to the model.
        // Authorization is rechecked by the broker before bytes are released.
        Err(_) => unavailable(
            "folder_unavailable",
            "That connected folder is no longer available to this conversation. You can ask the user to connect a folder again.",
        ),
    }
}

/// What the staged copy of a granted folder has to say about one read.
enum StagedOutcome {
    /// The folder is not staged for this turn, so the user's folder is still
    /// the only view and the broker answers as it always has.
    Unstaged,
    /// The staged copy's answer, in the shape the broker would have returned.
    Result(OperationResult),
    /// The folder is staged and the path is not in the staged tree — because
    /// the agent deleted it this turn, or because it never existed. Answering
    /// from the user's folder here would show the model a file the shell it is
    /// driving cannot see.
    Missing,
}

/// Answer a folder read from this turn's staged copy when the folder has one.
///
/// Exec writes into a per-turn copy of every writable granted folder rather
/// than the folder itself, so inside a turn the user's folder is the stale
/// view: a file the agent has just written is not in it, and one the agent has
/// deleted is still there. A `list_folder` that disagreed with the shell in the
/// same turn is worse than either view alone, because nothing tells the model
/// which one is lying — so the folder tools read the staged copy too.
///
/// Only reads are redirected. A folder the turn does not stage — no exec grant,
/// a read-only grant, a folder the overlay could not copy, or a managed
/// execution provider that mounts nothing — takes the broker path unchanged.
///
/// Authority stays with the broker. The staged copy is served only after the
/// broker confirms this conversation may read that root *now*, which is what
/// keeps a grant revoked mid-turn from being answered out of staging. The
/// probe is a root listing rather than the request itself because the request
/// may name a path that exists only in the staged tree.
async fn staged_outcome(
    state: &HostAccess,
    context: AuthoritativeContext,
    request: &OperationRequest,
) -> StagedOutcome {
    let (root_id, path, read_file) = match request {
        OperationRequest::ListDirectory(PathRequest { root_id, path }) => (root_id, path, false),
        OperationRequest::ReadFile(PathRequest { root_id, path }) => (root_id, path, true),
        _ => return StagedOutcome::Unstaged,
    };
    let Some(staged) = state.staged_folders() else {
        return StagedOutcome::Unstaged;
    };
    let Ok(root) = tidebreak_core::HostRootId::from_uuid(root_id.as_uuid()) else {
        return StagedOutcome::Unstaged;
    };
    let Some(overlay) = staged.staged_root(tidebreak_core::ChatId::from(context.chat_id), root)
    else {
        return StagedOutcome::Unstaged;
    };
    if !may_read_root(state, context, *root_id).await {
        return StagedOutcome::Unstaged;
    }
    let staged = if read_file {
        staged_file(&overlay, path).await
    } else {
        staged_directory(&overlay, path).await
    };
    staged.map_or(StagedOutcome::Missing, StagedOutcome::Result)
}

/// Whether the broker would allow this conversation to read `root_id` right now.
///
/// `list_roots` filters its answer through the same `ReadFiles` authorization
/// call that gates a read of the folder, so a root appearing in it is the
/// broker's live judgement rather than a cached one. It is asked instead of the
/// caller's own request because it does not depend on the requested path
/// existing in the user's folder.
async fn may_read_root(state: &HostAccess, context: AuthoritativeContext, root_id: RootId) -> bool {
    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: tidebreak_host_broker::RequestId::new(),
            context: context.execution,
            request: OperationRequest::ListRoots,
        })
        .await;
    matches!(
        result,
        Ok(OperationResult::ListRoots { roots })
            if roots.iter().any(|root| root.root_id == root_id)
    )
}

/// List one directory inside a staged folder, in the broker's own result shape.
async fn staged_directory(
    overlay: &std::path::Path,
    path: &RelativePath,
) -> Option<OperationResult> {
    let directory = resolve_scratch_directory(overlay, staged_relative(path), false).await?;
    let entries = directory
        .entries()
        .await
        .ok()?
        .into_iter()
        // A name the protocol cannot address is one no follow-up call could
        // name, and the broker drops it from a listing for the same reason.
        .filter(|entry| RelativePath::parse(&entry.name).is_ok())
        .map(|entry| DirectoryEntry {
            name: entry.name,
            kind: match entry.kind {
                ScratchEntryKind::File => EntryKind::File,
                ScratchEntryKind::Directory => EntryKind::Directory,
                ScratchEntryKind::Other => EntryKind::Other,
            },
        })
        .take(MAX_DIRECTORY_ENTRIES)
        .collect();
    Some(OperationResult::ListDirectory { entries })
}

/// Read one file inside a staged folder, in the broker's own result shape.
async fn staged_file(overlay: &std::path::Path, path: &RelativePath) -> Option<OperationResult> {
    let (prefix, name) = staged_relative(path).rsplit_once('/').map_or_else(
        || ("", staged_relative(path)),
        |(prefix, name)| (prefix, name),
    );
    if name.is_empty() {
        return None;
    }
    let directory = resolve_scratch_directory(overlay, prefix, false).await?;
    if directory.file_stamp(name).await?.len > MAX_STAGED_READ_BYTES {
        return None;
    }
    let bytes = directory.read_file(name).await.ok()?;
    let bytes_read = bytes.len();
    Some(OperationResult::ReadFile(ReadFileResult {
        content: String::from_utf8(bytes).ok()?,
        bytes: bytes_read,
    }))
}

/// The overlay-relative form of a protocol path. The protocol spells the root
/// itself `.`, which the pinned scratch walk refuses rather than resolves.
fn staged_relative(path: &RelativePath) -> &str {
    if path.is_root() {
        ""
    } else {
        path.as_str()
    }
}

fn broker_request(call: &ToolCallRecord) -> Result<OperationRequest, ()> {
    match call.name.as_str() {
        LIST_CONNECTED_FOLDERS_TOOL => Ok(OperationRequest::ListRoots),
        LIST_FOLDER_TOOL => {
            let args: ListFolderArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| ())?;
            let root_id = RootId::from_uuid(args.root_id).map_err(|_| ())?;
            let path = RelativePath::parse(&args.path).map_err(|_| ())?;
            Ok(OperationRequest::ListDirectory(PathRequest {
                root_id,
                path,
            }))
        }
        READ_CONNECTED_FILE_TOOL => {
            let args: ReadConnectedFileArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| ())?;
            let root_id = RootId::from_uuid(args.root_id).map_err(|_| ())?;
            let path = RelativePath::parse(&args.path).map_err(|_| ())?;
            if path.is_root() {
                return Err(());
            }
            Ok(OperationRequest::ReadFile(PathRequest { root_id, path }))
        }
        _ => Err(()),
    }
}

fn serialize_result(call: &ToolCallRecord, result: OperationResult) -> StoredResolution {
    // The rows are built from the same values the model-facing payload is, so
    // the card and the model can never disagree about what the folder held.
    let (result, rows) = match result {
        OperationResult::ListRoots { roots } => {
            let roots: Vec<_> = roots.into_iter().take(MAX_DIRECTORY_ENTRIES).collect();
            let rows = roots
                .iter()
                .map(|root| ResultEntry::new(ResultEntryKind::Folder, root.display_name.clone()))
                .collect();
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
                    // The broker's own word for what an entry is; anything that
                    // is not a directory reads as a file.
                    let kind = if format!("{:?}", entry.kind).to_lowercase().contains("dir") {
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
            // card, so the row reports the read rather than replaying it.
            // The name comes from the request: a read result is bytes, and
            // only the arguments say which file they came from.
            let path = serde_json::from_value::<ReadConnectedFileArgs>(call.arguments.clone())
                .map(|args| args.path)
                .unwrap_or_default();
            let mut row = ResultEntry::new(ResultEntryKind::File, read_file_name(&path))
                .with_meta(tidebreak_core::format_bytes(content.len() as u64));
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
        Ok(result) if result.len() <= MAX_RESULT_CONTENT_BYTES => StoredResolution::Completed {
            result,
            rows: Some(serde_json::json!({ "entries": rows })),
        },
        _ => unavailable(
            "result_too_large",
            "The connected-folder result was too large to return.",
        ),
    }
}

pub(super) fn granted_folder_capabilities(
    capabilities: &[Capability],
) -> Vec<GrantedFolderCapability> {
    capabilities
        .iter()
        .filter_map(|capability| match capability {
            Capability::ReadFiles => Some(GrantedFolderCapability::ReadFiles),
            Capability::WriteFiles => Some(GrantedFolderCapability::WriteFiles),
            Capability::ExecuteCommands => Some(GrantedFolderCapability::ExecuteCommands),
            Capability::ListRoots => None,
            // Capability is non-exhaustive. Unknown future per-folder reach is
            // intentionally under-reported until this one conversion and the
            // model-facing vocabulary are extended together.
            _ => None,
        })
        .collect()
}

pub(super) fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn unavailable(code: &str, message: &str) -> StoredResolution {
    StoredResolution::Failed {
        result: serde_json::json!({ "status": "unavailable", "message": message }).to_string(),
        error_code: code.to_owned(),
        error_detail: None,
    }
}

fn interrupted_resolution() -> StoredResolution {
    unavailable(
        "folder_operation_interrupted",
        "The folder operation could not be safely resumed after an interruption. Please try again.",
    )
}

async fn publish_resolution(
    state: &HostAccess,
    receipt: &FolderOperationReceipt,
    resolution: &StoredResolution,
) -> Result<(), String> {
    let client = control_plane(state)?;
    match client
        .resolve(
            receipt.chat_id,
            receipt.call_id,
            receipt.lease_token,
            resolution,
        )
        .await
    {
        Ok(()) => state
            .receipts
            .remove_operation(receipt.call_id)
            .map_err(private_receipt_error),
        Err(error) if error.is_conflict() => {
            let pending = client
                .pending(receipt.chat_id)
                .await
                .map_err(control_plane_error)?
                .into_iter()
                .any(|call| call.id == receipt.call_id);
            if pending {
                Err("connected-folder result no longer owns the pending request".to_owned())
            } else {
                state
                    .receipts
                    .remove_operation(receipt.call_id)
                    .map_err(private_receipt_error)
            }
        }
        Err(error) => Err(control_plane_error(error)),
    }
}

fn is_connected_folder_call(call: &ToolCallRecord) -> bool {
    if call.execution != ToolCallExecution::Client || call.status != ToolCallStatus::Pending {
        return false;
    }
    match call.name.as_str() {
        LIST_CONNECTED_FOLDERS_TOOL => validate_list_connected_folders_arguments(&call.arguments),
        LIST_FOLDER_TOOL => validate_list_folder_arguments(&call.arguments),
        READ_CONNECTED_FILE_TOOL => validate_read_connected_file_arguments(&call.arguments),
        IMPORT_CONNECTED_FILE_TOOL => validate_import_connected_file_arguments(&call.arguments),
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::ChatId;

    #[test]
    fn broker_capabilities_reach_the_model_facing_folder_listing() {
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: LIST_CONNECTED_FOLDERS_TOOL.into(),
            arguments: serde_json::json!({}),
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
        };
        let root_id = RootId::new();
        let resolution = serialize_result(
            &call,
            OperationResult::ListRoots {
                roots: vec![tidebreak_host_broker::RootAccess {
                    root_id,
                    display_name: "Documents".into(),
                    capabilities: vec![
                        Capability::ReadFiles,
                        Capability::WriteFiles,
                        Capability::ExecuteCommands,
                    ],
                }],
            },
        );
        let StoredResolution::Completed { result, .. } = resolution else {
            panic!("expected completed result");
        };
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            result["folders"][0]["capabilities"],
            serde_json::json!(["read_files", "write_files", "execute_commands"])
        );
    }

    /// The confusion this exists to prevent: the agent writes a file with
    /// `exec`, calls `list_folder` to confirm it, and is shown a folder that
    /// does not have it. Exec works in the turn's staged copy, so the folder
    /// tools read that copy too — including a file that only exists there, and
    /// excluding one the agent deleted — while the user's folder is left
    /// untouched until the turn ends.
    #[tokio::test]
    async fn a_folder_listing_shows_what_exec_wrote_in_the_same_turn() {
        let granted = tempfile::tempdir().unwrap();
        std::fs::write(granted.path().join("notes.md"), "original").unwrap();

        let scratch = tempfile::tempdir().unwrap();
        let overlay = tidebreak_code_execution::WriteOverlay::prepare(
            scratch.path(),
            "chat",
            &[granted.path().to_path_buf()],
        )
        .await
        .expect("a readable granted folder stages");
        let staged = overlay.slots()[0].overlay().to_path_buf();

        // What exec does mid-turn.
        std::fs::write(staged.join("report.md"), "draft").unwrap();
        std::fs::remove_file(staged.join("notes.md")).unwrap();

        let Some(OperationResult::ListDirectory { entries }) =
            staged_directory(&staged, &RelativePath::root()).await
        else {
            panic!("a staged folder lists from its staged copy");
        };
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["report.md"]);

        let Some(OperationResult::ReadFile(file)) =
            staged_file(&staged, &RelativePath::parse("report.md").unwrap()).await
        else {
            panic!("a staged folder reads from its staged copy");
        };
        assert_eq!(file.content, "draft");

        // A file the agent deleted this turn is gone from the tool's view too,
        // rather than being served from the folder the shell cannot see.
        assert!(
            staged_file(&staged, &RelativePath::parse("notes.md").unwrap())
                .await
                .is_none()
        );

        // None of it has reached the user's folder: the tools report the turn's
        // view, they do not apply it.
        assert!(granted.path().join("notes.md").exists());
        assert!(!granted.path().join("report.md").exists());
    }

    #[test]
    fn results_are_bounded_and_do_not_include_host_error_detail() {
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: READ_CONNECTED_FILE_TOOL.into(),
            arguments: serde_json::json!({
                "root_id": uuid::Uuid::new_v4(),
                "path": "reports/q3.md",
            }),
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
        };
        let result = serialize_result(
            &call,
            OperationResult::ReadFile(tidebreak_host_broker::ReadFileResult {
                content: "x".repeat(MAX_FILE_CONTENT_BYTES + 1),
                bytes: MAX_FILE_CONTENT_BYTES + 1,
            }),
        );
        let StoredResolution::Completed { result, rows } = result else {
            panic!("expected bounded result");
        };
        assert!(result.len() <= MAX_RESULT_CONTENT_BYTES);
        assert!(result.contains("\"truncated\":true"));
        // The card reports the read rather than replaying the file, and names
        // the file from the request — a read result is only bytes.
        let rows = rows.expect("a completed read reports its row");
        assert_eq!(rows["entries"][0]["label"], "q3.md");
        assert_eq!(rows["entries"][0]["kind"], "file");
        assert_eq!(rows["entries"][0]["detail"], "truncated at the read limit");
        assert!(!rows.to_string().contains('x'));
    }

    #[test]
    fn native_executor_accepts_only_valid_foreground_folder_calls() {
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: LIST_FOLDER_TOOL.into(),
            arguments: serde_json::json!({ "root_id": uuid::Uuid::new_v4(), "path": "notes" }),
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
        };
        assert!(is_connected_folder_call(&call));
        let mut forged = call;
        forged.arguments =
            serde_json::json!({ "root_id": uuid::Uuid::new_v4(), "path": "../secret" });
        assert!(!is_connected_folder_call(&forged));
    }

    #[test]
    fn pending_discovery_never_replaces_a_recovered_operation_receipt() {
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: LIST_CONNECTED_FOLDERS_TOOL.into(),
            arguments: serde_json::json!({}),
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
        };
        assert!(should_discover_call(&call, &HashSet::new()));
        assert!(!should_discover_call(&call, &HashSet::from([call.id])));
    }

    #[test]
    fn interrupted_claims_are_terminalized_without_replaying_the_read() {
        let StoredResolution::Failed {
            result,
            error_code,
            error_detail,
        } = interrupted_resolution()
        else {
            panic!("interrupted folder work must have a stable terminal result");
        };
        assert_eq!(error_code, "folder_operation_interrupted");
        assert!(error_detail.is_none());
        assert!(result.contains("could not be safely resumed"));
        assert!(!result.contains("/Users/"));
    }

    #[test]
    fn only_an_idempotent_operation_may_be_retried_after_an_interruption() {
        // A read discloses bytes the broker keeps no receipt for, so a second
        // attempt has nothing to reconcile against.
        for read_only in [
            LIST_CONNECTED_FOLDERS_TOOL,
            LIST_FOLDER_TOOL,
            READ_CONNECTED_FILE_TOOL,
        ] {
            assert_eq!(dispatch_recovery(read_only), DispatchRecovery::Terminalize);
        }
        // An import derives its source identity from the exact request, so
        // repeating it recovers the one source instead of adding another.
        assert_eq!(
            dispatch_recovery(IMPORT_CONNECTED_FILE_TOOL),
            DispatchRecovery::Retry
        );

        let mut import = FolderOperationReceipt::new(
            ChatId::new(),
            CallId::new(),
            uuid::Uuid::new_v4(),
            DispatchRecovery::Retry,
        );
        import.phase = FolderOperationPhase::DispatchStarted;
        assert!(!dispatch_is_ambiguous(&import));
        // The retry only applies while this executor still holds the lease. A
        // resolution already exists once the work finished, and recovery then
        // republishes it rather than running the operation a second time.
        import.resolution = Some(interrupted_resolution());
        assert!(!dispatch_is_ambiguous(&import));
    }

    #[test]
    fn the_executor_accepts_imports_under_the_same_path_rules_as_reads() {
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: IMPORT_CONNECTED_FILE_TOOL.into(),
            arguments: serde_json::json!({
                "root_id": uuid::Uuid::new_v4(),
                "path": "reports/q3.pdf"
            }),
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
        };
        assert!(is_connected_folder_call(&call));
        let mut traversal = call.clone();
        traversal.arguments["path"] = serde_json::json!("../secret.pdf");
        assert!(!is_connected_folder_call(&traversal));
        // An import is never routed through the plain read dispatch table.
        assert!(broker_request(&call).is_err());
    }

    #[test]
    fn dispatch_started_receipt_is_terminalized_even_with_a_live_lease() {
        let mut receipt = FolderOperationReceipt::new(
            ChatId::new(),
            CallId::new(),
            uuid::Uuid::new_v4(),
            DispatchRecovery::Terminalize,
        );
        assert_eq!(receipt.phase, FolderOperationPhase::NotStarted);
        assert!(!dispatch_is_ambiguous(&receipt));
        receipt.phase = FolderOperationPhase::DispatchStarted;
        // Recovery checks this persisted phase before claiming, so it cannot
        // run a second read while the original lease still remains live.
        assert!(dispatch_is_ambiguous(&receipt));
    }
}
