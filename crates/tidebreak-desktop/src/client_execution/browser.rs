//! Durable native executor for foreground browser tool calls.
//!
//! The server persists canonical client calls, while this module derives the
//! browser scope from the persisted chat and keeps all native capability state
//! outside the renderer. Receipts contain only call identity, lease fencing,
//! dispatch phase, and terminal model output. They never contain a browser
//! capability, workspace supplied by a model, or screenshot pixels.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use tauri::{AppHandle, Manager};
use tidebreak_core::{
    validate_browser_act_arguments, validate_browser_list_arguments,
    validate_browser_navigate_arguments, validate_browser_screenshot_arguments,
    validate_browser_snapshot_arguments, validate_browser_upload_arguments,
    validate_browser_wait_arguments, BrowserActArgs, BrowserGrantCapability, BrowserListArgs,
    BrowserListResult, BrowserNavigateArgs, BrowserOrigin, BrowserPageSnapshot,
    BrowserScreenshotArgs, BrowserScreenshotResult, BrowserSnapshotArgs, BrowserUploadArgs,
    BrowserUploadResource, BrowserUploadStatus, BrowserWaitArgs, BrowserWaitResult, CallId,
    ImageRef, OutputId, OutputRevisionId, SessionId, ToolCallExecution, ToolCallRecord,
    ToolCallStatus, BROWSER_ACT_TOOL, BROWSER_LIST_TOOL, BROWSER_NAVIGATE_TOOL,
    BROWSER_SCREENSHOT_TOOL, BROWSER_SNAPSHOT_TOOL, BROWSER_UPLOAD_TOOL, BROWSER_WAIT_TOOL,
};
use tidebreak_host_broker::{RelativePath, RootId, MAX_READ_FILE_BINARY_BYTES};
use tidebreak_server::output_files::{
    read_output_revision_bytes, require_exact_revision, require_live_output,
};
use uuid::Uuid;

use crate::browser_control::{BrowserConfirmationBinding, BrowserRegistry};
use crate::browser_semantics::BrowserUploadFile;
use crate::host_access::{AuthoritativeContext, HostAccess};
use crate::image_attachments::PublishedImageAttachment;
use crate::AppState;

use super::{
    control_plane, control_plane_error, private_receipt_error, FolderOperationPhase,
    ForegroundBrowserReceipt, StoredResolution,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// Keep the serialized result under the durable client-resolution ceiling.
const MAX_RESULT_CONTENT_BYTES: usize = 56 * 1024;

/// One live native capability per persisted foreground chat.
///
/// Reusing the capability lets a later wait or screenshot prove that it chains
/// from the same controller and semantic snapshot. Expired capabilities rotate
/// atomically in [`BrowserRegistry`], preserving Stop, takeover, instance, and
/// document fences without reusing code-session authority.
#[derive(Default)]
pub(crate) struct ForegroundBrowserExecutorState {
    capabilities: StdMutex<HashMap<Uuid, Uuid>>,
}

fn lock<'a, T>(mutex: &'a StdMutex<T>) -> StdMutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ForegroundBrowserExecutorState {
    fn capability_for(
        &self,
        registry: &BrowserRegistry,
        chat_id: Uuid,
        workspace_id: &str,
    ) -> Result<Uuid, String> {
        let mut capabilities = lock(&self.capabilities);
        if let Some(capability_id) = capabilities.get(&chat_id).copied() {
            if registry
                .heartbeat_agent_capability(capability_id, workspace_id)
                .is_ok()
            {
                return Ok(capability_id);
            }
            match registry.rotate_expired_agent_capability(
                capability_id,
                workspace_id,
                "Chat agent",
            ) {
                Ok(replacement) => {
                    capabilities.insert(chat_id, replacement);
                    return Ok(replacement);
                }
                Err(_) => {
                    capabilities.remove(&chat_id);
                    registry.revoke_agent_capability(capability_id);
                    return Err("browser capability is unavailable".to_owned());
                }
            }
        }

        let capability_id = registry.issue_agent_capability(workspace_id, "Chat agent");
        capabilities.insert(chat_id, capability_id);
        Ok(capability_id)
    }

    fn retain_live_chats(&self, registry: &BrowserRegistry, live_chat_ids: &HashSet<Uuid>) {
        let stale = {
            let mut capabilities = lock(&self.capabilities);
            let stale = capabilities
                .iter()
                .filter(|(chat_id, _)| !live_chat_ids.contains(chat_id))
                .map(|(_, capability_id)| *capability_id)
                .collect::<Vec<_>>();
            capabilities.retain(|chat_id, _| live_chat_ids.contains(chat_id));
            stale
        };
        for capability_id in stale {
            registry.revoke_agent_capability(capability_id);
        }
    }
}

/// Recover persisted outcomes, then discover new foreground browser calls.
/// The renderer is never an execution authority.
pub(crate) async fn recover_foreground_browser_operations(app: AppHandle) {
    loop {
        if recover_once(&app).await {
            eprintln!("tidebreak-desktop: foreground browser executor deferred work");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn recover_once(app: &AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let registry = app.state::<BrowserRegistry>();
    let receipts = match state.receipts.load_foreground_browsers() {
        Ok(receipts) => receipts,
        Err(error) => {
            eprintln!("tidebreak-desktop: foreground browser receipt recovery failed: {error}");
            return true;
        }
    };
    let recovered_call_ids: HashSet<CallId> =
        receipts.iter().map(|receipt| receipt.call_id).collect();

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
    let live_chat_ids = chats.iter().map(|chat| chat.id.0).collect::<HashSet<_>>();
    state
        .foreground_browser
        .retain_live_chats(&registry, &live_chat_ids);

    let mut failed = false;
    for receipt in receipts {
        if !live_chat_ids.contains(&receipt.chat_id.0) {
            if let Err(error) = state
                .receipts
                .remove_foreground_browser(receipt.call_id)
                .map_err(private_receipt_error)
            {
                eprintln!("tidebreak-desktop: foreground browser receipt deferred: {error}");
                failed = true;
            }
            continue;
        }
        if let Err(error) = execute_receipt(app, &state, &registry, receipt).await {
            eprintln!("tidebreak-desktop: foreground browser receipt deferred: {error}");
            failed = true;
        }
    }

    for chat in chats {
        let calls = match client.pending(chat.id).await {
            Ok(calls) => calls,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        for call in calls.into_iter().filter(|call| {
            !recovered_call_ids.contains(&call.id) && is_foreground_browser_call(call)
        }) {
            let receipt =
                ForegroundBrowserReceipt::new(chat.id, call.id, state.receipts.executor_id());
            if let Err(error) = execute_receipt(app, &state, &registry, receipt).await {
                eprintln!("tidebreak-desktop: foreground browser execution deferred: {error}");
                failed = true;
            }
        }
    }
    failed
}

async fn execute_receipt(
    app: &AppHandle,
    state: &HostAccess,
    registry: &BrowserRegistry,
    mut receipt: ForegroundBrowserReceipt,
) -> Result<(), String> {
    if let Some(resolution) = receipt.resolution.clone() {
        return publish_resolution(state, &receipt, &resolution).await;
    }
    // Navigation may already have started, and an observation may already
    // have disclosed page data. Never replay after the durable dispatch fence.
    if receipt.phase == FolderOperationPhase::DispatchStarted {
        receipt.resolution = Some(unavailable(
            "browser_operation_interrupted",
            "The browser operation could not be safely resumed after an interruption. Try again.",
        ));
        state
            .receipts
            .save_foreground_browser(&receipt)
            .map_err(private_receipt_error)?;
        return publish_resolution(
            state,
            &receipt,
            receipt.resolution.as_ref().expect("stored above"),
        )
        .await;
    }

    // Loading the persisted chat is the authority boundary. The resulting
    // scope never comes from call arguments or renderer state.
    let context = state.context(receipt.chat_id.0).await?;
    let workspace_id = context.foreground_browser_scope();
    let client = control_plane(state)?;

    // Re-read and validate the canonical call before claiming. This also
    // refuses a receipt whose call is now owned by another executor.
    let pending = client
        .pending(receipt.chat_id)
        .await
        .map_err(control_plane_error)?;
    let Some(call) = pending.into_iter().find(|call| call.id == receipt.call_id) else {
        return state
            .receipts
            .remove_foreground_browser(receipt.call_id)
            .map_err(private_receipt_error);
    };
    if !validate_call_identity(&call, receipt.chat_id, receipt.call_id) {
        state
            .receipts
            .remove_foreground_browser(receipt.call_id)
            .map_err(private_receipt_error)?;
        return Err(
            "local control plane returned an invalid foreground browser request".to_owned(),
        );
    }
    if call
        .client_executor_id
        .is_some_and(|executor_id| executor_id != receipt.executor_id)
    {
        return state
            .receipts
            .remove_foreground_browser(receipt.call_id)
            .map_err(private_receipt_error);
    }

    // Persist the exact lease token before claiming. A lost claim response can
    // then recover only that lease and cannot mint a second dispatch.
    state
        .receipts
        .save_foreground_browser(&receipt)
        .map_err(private_receipt_error)?;
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
        || !validate_call_identity(&claim.call, receipt.chat_id, receipt.call_id)
    {
        return Err(
            "local control plane returned an invalid foreground browser request".to_owned(),
        );
    }

    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    receipt.phase = FolderOperationPhase::DispatchStarted;
    state
        .receipts
        .save_foreground_browser(&receipt)
        .map_err(private_receipt_error)?;

    let resolution =
        match state
            .foreground_browser
            .capability_for(registry, context.chat_id, &workspace_id)
        {
            Ok(capability_id) => {
                execute_operation(app, state, registry, context, capability_id, &claim.call).await
            }
            Err(error) => map_native_error(None, error),
        };
    receipt.resolution = Some(resolution);
    state
        .receipts
        .save_foreground_browser(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        &receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

/// A claim conflict may belong to another executor. Never dispatch after one.
async fn recover_after_claim_conflict(
    state: &HostAccess,
    receipt: &mut ForegroundBrowserReceipt,
) -> Result<(), String> {
    let client = control_plane(state)?;
    let pending = client
        .pending(receipt.chat_id)
        .await
        .map_err(control_plane_error)?;
    let Some(call) = pending.into_iter().find(|call| call.id == receipt.call_id) else {
        return state
            .receipts
            .remove_foreground_browser(receipt.call_id)
            .map_err(private_receipt_error);
    };
    if !validate_call_identity(&call, receipt.chat_id, receipt.call_id) {
        state
            .receipts
            .remove_foreground_browser(receipt.call_id)
            .map_err(private_receipt_error)?;
        return Err(
            "local control plane returned an invalid foreground browser request".to_owned(),
        );
    }
    if call.client_executor_id != Some(receipt.executor_id) {
        return state
            .receipts
            .remove_foreground_browser(receipt.call_id)
            .map_err(private_receipt_error);
    }

    // The call still names this stable executor, but the claim response was
    // ambiguous. Store an exact terminal result and let resolution succeed
    // only if this receipt still owns the lease. No native dispatch occurs.
    receipt.resolution = Some(unavailable(
        "browser_operation_interrupted",
        "The browser operation could not be safely resumed after an interruption. Try again.",
    ));
    state
        .receipts
        .save_foreground_browser(receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

async fn publish_resolution(
    state: &HostAccess,
    receipt: &ForegroundBrowserReceipt,
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
            .remove_foreground_browser(receipt.call_id)
            .map_err(private_receipt_error),
        Err(error) if error.is_conflict() => {
            let pending = client
                .pending(receipt.chat_id)
                .await
                .map_err(control_plane_error)?
                .into_iter()
                .any(|call| call.id == receipt.call_id);
            if pending {
                Err("foreground browser result no longer owns the pending request".to_owned())
            } else {
                state
                    .receipts
                    .remove_foreground_browser(receipt.call_id)
                    .map_err(private_receipt_error)
            }
        }
        Err(error) => Err(control_plane_error(error)),
    }
}

fn validate_call_identity(call: &ToolCallRecord, chat_id: SessionId, call_id: CallId) -> bool {
    call.chat_id == chat_id && call.id == call_id && is_foreground_browser_call(call)
}

fn is_foreground_browser_call(call: &ToolCallRecord) -> bool {
    if call.execution != ToolCallExecution::Client
        || call.status != ToolCallStatus::Pending
        || call.raw_arguments.is_some()
    {
        return false;
    }
    match call.name.as_str() {
        BROWSER_LIST_TOOL => validate_browser_list_arguments(&call.arguments),
        BROWSER_NAVIGATE_TOOL => validate_browser_navigate_arguments(&call.arguments),
        BROWSER_SNAPSHOT_TOOL => validate_browser_snapshot_arguments(&call.arguments),
        BROWSER_WAIT_TOOL => validate_browser_wait_arguments(&call.arguments),
        BROWSER_SCREENSHOT_TOOL => validate_browser_screenshot_arguments(&call.arguments),
        BROWSER_ACT_TOOL => validate_browser_act_arguments(&call.arguments),
        BROWSER_UPLOAD_TOOL => validate_browser_upload_arguments(&call.arguments),
        _ => false,
    }
}

async fn execute_operation(
    app: &AppHandle,
    state: &HostAccess,
    registry: &BrowserRegistry,
    context: AuthoritativeContext,
    capability_id: Uuid,
    call: &ToolCallRecord,
) -> StoredResolution {
    match call.name.as_str() {
        BROWSER_LIST_TOOL => {
            if serde_json::from_value::<BrowserListArgs>(call.arguments.clone()).is_err() {
                return invalid_request();
            }
            match registry.list_for_capability(capability_id) {
                Ok(sessions) => completed(&BrowserListResult { sessions }),
                Err(error) => map_native_error(None, error),
            }
        }
        BROWSER_NAVIGATE_TOOL => {
            let Ok(arguments) =
                serde_json::from_value::<BrowserNavigateArgs>(call.arguments.clone())
            else {
                return invalid_request();
            };
            match crate::code_browser::navigate_browser_for_agent(
                app,
                registry,
                capability_id,
                &arguments,
            )
            .await
            {
                Ok(result) => completed(&result),
                Err(error) => map_native_error(Some(&arguments.browser_id), error),
            }
        }
        BROWSER_SNAPSHOT_TOOL => {
            let Ok(arguments) =
                serde_json::from_value::<BrowserSnapshotArgs>(call.arguments.clone())
            else {
                return invalid_request();
            };
            match crate::browser_semantics::browser_semantic_snapshot(
                app,
                registry,
                capability_id,
                arguments.clone(),
            )
            .await
            {
                Ok(result) => completed_snapshot(&result),
                Err(error) => map_native_error(Some(&arguments.browser_id), error),
            }
        }
        BROWSER_WAIT_TOOL => {
            let Ok(arguments) = serde_json::from_value::<BrowserWaitArgs>(call.arguments.clone())
            else {
                return invalid_request();
            };
            match crate::browser_semantics::browser_wait(
                app,
                registry,
                capability_id,
                arguments.clone(),
            )
            .await
            {
                Ok(result) => completed_wait(&result),
                Err(error) => map_native_error(Some(&arguments.browser_id), error),
            }
        }
        BROWSER_SCREENSHOT_TOOL => {
            let Ok(arguments) =
                serde_json::from_value::<BrowserScreenshotArgs>(call.arguments.clone())
            else {
                return invalid_request();
            };
            match crate::browser_semantics::browser_screenshot(
                app,
                registry,
                capability_id,
                arguments.clone(),
            )
            .await
            {
                Ok(result) => finish_screenshot(app, state, context, result).await,
                Err(error) => map_native_error(Some(&arguments.browser_id), error),
            }
        }
        BROWSER_ACT_TOOL => {
            let Ok(arguments) = serde_json::from_value::<BrowserActArgs>(call.arguments.clone())
            else {
                return invalid_request();
            };
            match crate::browser_semantics::browser_native_act(
                app,
                registry,
                capability_id,
                arguments.clone(),
            )
            .await
            {
                Ok(result) => completed(&result),
                Err(error) => map_native_error(Some(&arguments.browser_id), error),
            }
        }
        BROWSER_UPLOAD_TOOL => {
            let Ok(arguments) = serde_json::from_value::<BrowserUploadArgs>(call.arguments.clone())
            else {
                return invalid_request();
            };
            match execute_browser_upload_operation(
                app,
                state,
                registry,
                context,
                capability_id,
                arguments.clone(),
            )
            .await
            {
                Ok(result) => completed(&result),
                Err(error) => map_native_error(Some(&arguments.browser_id), error),
            }
        }
        _ => invalid_request(),
    }
}

enum BrowserUploadSourceIdentity {
    Output {
        output_id: OutputId,
        revision_id: OutputRevisionId,
        filename: String,
        media_type: String,
        byte_len: u64,
        sha256: [u8; 32],
    },
    ConnectedFile {
        root_id: RootId,
        path: RelativePath,
    },
}

struct ResolvedBrowserUploadSource {
    identity: BrowserUploadSourceIdentity,
    file: BrowserUploadFile,
}

async fn execute_browser_upload_operation(
    app: &AppHandle,
    state: &HostAccess,
    registry: &BrowserRegistry,
    context: AuthoritativeContext,
    capability_id: Uuid,
    arguments: BrowserUploadArgs,
) -> Result<tidebreak_core::BrowserUploadResult, String> {
    if !arguments.is_well_formed() {
        return Err("browser upload request is not valid".to_owned());
    }
    let host_snapshot = registry.begin_agent_observation(capability_id, &arguments.browser_id)?;
    if !host_snapshot
        .engine
        .as_ref()
        .is_some_and(|engine| engine.capabilities.semantic_actions)
    {
        return Ok(crate::browser_semantics::browser_upload_result(
            &arguments,
            BrowserUploadStatus::EngineFailure,
            "Trusted browser file attachment is not available on this platform.",
            None,
        ));
    }
    if !host_snapshot
        .agent_access
        .as_ref()
        .is_some_and(|access| access.can_transfer_files)
    {
        return Err("browser origin is not shared for this operation".to_owned());
    }
    let workspace_id = host_snapshot.workspace_id;
    let origin = host_snapshot
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let target = match registry.semantic_target(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        arguments.document_epoch,
        &arguments.target_ref,
    ) {
        Ok(target) => target,
        Err(crate::browser_control::BrowserTargetError::StaleTarget) => {
            return Ok(crate::browser_semantics::browser_upload_result(
                &arguments,
                BrowserUploadStatus::StaleTarget,
                "The page or target changed. Take a new snapshot before uploading.",
                None,
            ));
        }
        Err(crate::browser_control::BrowserTargetError::BrowserHidden) => {
            return Ok(crate::browser_semantics::browser_upload_result(
                &arguments,
                BrowserUploadStatus::HiddenTab,
                "Bring this browser tab to the foreground before uploading.",
                None,
            ));
        }
    };
    if !crate::browser_semantics::is_file_input(&target) {
        return Ok(crate::browser_semantics::browser_upload_result(
            &arguments,
            BrowserUploadStatus::InvalidTarget,
            "The selected target is not a file input. Use a file-input ref from the latest snapshot.",
            None,
        ));
    }

    let initial = resolve_browser_upload_source(app, state, context, &arguments.resource).await?;
    let target_label = "File input".to_owned();
    let _ = registry.set_agent_action(
        capability_id,
        &arguments.browser_id,
        Some("Waiting for upload confirmation"),
        false,
    );
    let approved = crate::browser_semantics::native_browser_upload_choice(
        app,
        &origin,
        &target_label,
        &initial.file,
    )
    .await?;
    if !approved {
        let _ = registry.set_agent_action(capability_id, &arguments.browser_id, None, false);
        return Ok(crate::browser_semantics::browser_upload_result(
            &arguments,
            BrowserUploadStatus::Declined,
            "The user declined this browser upload. Do not retry it without direction.",
            None,
        ));
    }

    let confirmation_binding = initial.file.binding.clone();
    let confirmation_id = registry.record_native_confirmation(
        capability_id,
        &arguments.browser_id,
        &origin,
        BrowserGrantCapability::BrowserTransferFiles,
        "upload_file",
        Some(&target_label),
        Some(&confirmation_binding),
    )?;
    let browser_id = arguments.browser_id.clone();
    let dispatch_app = app.clone();
    let dispatch_registry = registry.clone();
    let identity = initial.identity;
    drop(initial.file);
    let result = registry
        .dispatch_agent_with_confirmation_binding(
            capability_id,
            &browser_id,
            &origin,
            BrowserGrantCapability::BrowserTransferFiles,
            "upload_file",
            Some(&target_label),
            crate::browser_control::BrowserDispatchEffect::Consequential,
            Some(confirmation_id),
            Some(confirmation_binding.clone()),
            move || async move {
                let confirmed =
                    reresolve_browser_upload_source(&dispatch_app, state, context, &identity)
                        .await
                        .map_err(|_| {
                            "browser upload resource changed before attachment".to_owned()
                        })?;
                if confirmed.binding != confirmation_binding {
                    return Err("browser upload resource changed before attachment".to_owned());
                }
                crate::browser_semantics::execute_browser_upload(
                    dispatch_app,
                    dispatch_registry,
                    capability_id,
                    workspace_id,
                    arguments,
                    confirmed,
                )
                .await
            },
        )
        .await;
    if result.is_err() {
        let _ = registry.set_agent_action(capability_id, &host_snapshot.browser_id, None, false);
    }
    result.map_err(|error| {
        if error == "browser confirmation does not match this action" {
            "browser upload resource changed before attachment".to_owned()
        } else {
            error
        }
    })
}

async fn resolve_browser_upload_source(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    resource: &BrowserUploadResource,
) -> Result<ResolvedBrowserUploadSource, String> {
    match resource {
        BrowserUploadResource::Output { output_id } => {
            resolve_output_upload_source(app, state, context, OutputId::from(*output_id)).await
        }
        BrowserUploadResource::ConnectedFile { root_id, path } => {
            let root_id = RootId::from_uuid(*root_id)
                .map_err(|_| "browser upload resource is unavailable".to_owned())?;
            let path = RelativePath::parse(path)
                .map_err(|_| "browser upload resource is unavailable".to_owned())?;
            if path.is_root() {
                return Err("browser upload resource is unavailable".to_owned());
            }
            let file = resolve_connected_file_upload(state, context, root_id, &path).await?;
            Ok(ResolvedBrowserUploadSource {
                identity: BrowserUploadSourceIdentity::ConnectedFile { root_id, path },
                file,
            })
        }
    }
}

async fn reresolve_browser_upload_source(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    identity: &BrowserUploadSourceIdentity,
) -> Result<BrowserUploadFile, String> {
    match identity {
        BrowserUploadSourceIdentity::Output {
            output_id,
            revision_id,
            filename,
            media_type,
            byte_len,
            sha256,
        } => {
            let store = state
                .store()
                .ok_or_else(|| "browser upload resource is unavailable".to_owned())?;
            let chat_id = SessionId::from(context.chat_id);
            require_exact_revision(store, chat_id, *output_id, *revision_id, *byte_len, *sha256)
                .await
                .map_err(|_| "browser upload resource changed before attachment".to_owned())?;
            let (output, revision) = require_live_output(store, chat_id, *output_id)
                .await
                .map_err(|_| "browser upload resource changed before attachment".to_owned())?;
            if revision.id != *revision_id
                || output.filename != *filename
                || output.media_type != *media_type
            {
                return Err("browser upload resource changed before attachment".to_owned());
            }
            read_output_upload_file(app, chat_id, output, revision).await
        }
        BrowserUploadSourceIdentity::ConnectedFile { root_id, path } => {
            resolve_connected_file_upload(state, context, *root_id, path).await
        }
    }
}

async fn resolve_output_upload_source(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    output_id: OutputId,
) -> Result<ResolvedBrowserUploadSource, String> {
    let store = state
        .store()
        .ok_or_else(|| "browser upload resource is unavailable".to_owned())?;
    let chat_id = SessionId::from(context.chat_id);
    let (output, revision) = require_live_output(store, chat_id, output_id)
        .await
        .map_err(|_| "browser upload resource is unavailable".to_owned())?;
    let identity = BrowserUploadSourceIdentity::Output {
        output_id,
        revision_id: revision.id,
        filename: output.filename.clone(),
        media_type: output.media_type.clone(),
        byte_len: revision.byte_len,
        sha256: revision.sha256,
    };
    let file = read_output_upload_file(app, chat_id, output, revision).await?;
    Ok(ResolvedBrowserUploadSource { identity, file })
}

async fn read_output_upload_file(
    app: &AppHandle,
    chat_id: SessionId,
    output: tidebreak_core::OutputRecord,
    revision: tidebreak_core::OutputRevision,
) -> Result<BrowserUploadFile, String> {
    if revision.byte_len > MAX_READ_FILE_BINARY_BYTES as u64
        || tidebreak_core::validate_portable_filename(&output.filename).is_err()
    {
        return Err("browser upload resource is unavailable".to_owned());
    }
    let scratch_root = crate::data_dir(app)?.join("scratch");
    let filename = output.filename.clone();
    let media_type = output.media_type.clone();
    let binding = BrowserConfirmationBinding {
        filename: filename.clone(),
        byte_len: revision.byte_len,
        sha256: revision.sha256,
    };
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        read_output_revision_bytes(&scratch_root, chat_id, &output, &revision)
    })
    .await
    .map_err(|_| "browser upload resource is unavailable".to_owned())?
    .map_err(|_| "browser upload resource is unavailable".to_owned())?;
    make_browser_upload_file(filename, media_type, bytes, binding)
}

async fn resolve_connected_file_upload(
    state: &HostAccess,
    context: AuthoritativeContext,
    root_id: RootId,
    path: &RelativePath,
) -> Result<BrowserUploadFile, String> {
    let filename = path
        .as_str()
        .rsplit('/')
        .next()
        .filter(|name| tidebreak_core::validate_portable_filename(name).is_ok())
        .ok_or_else(|| "browser upload resource is unavailable".to_owned())?
        .to_owned();
    let staged_root = super::source_import::current_staged_root(state, context, root_id);
    if staged_root.is_some()
        && !super::source_import::root_is_still_attached(state, context, root_id).await
    {
        return Err("browser upload resource is unavailable".to_owned());
    }
    let source = super::source_import::select_source_bytes(
        staged_root,
        path,
        super::source_import::read_broker_file_bytes(state, context, root_id, path),
    )
    .await
    .map_err(|_| "browser upload resource is unavailable".to_owned())?;
    if !super::source_import::root_is_still_attached(state, context, root_id).await
        || !super::source_import::selected_staging_is_current(
            state,
            context,
            root_id,
            source.staged_root.as_deref(),
        )
    {
        return Err("browser upload resource is unavailable".to_owned());
    }
    let media_type = tidebreak_server::media_type::sniff_media_type(&source.bytes, Some(&filename));
    let binding = BrowserConfirmationBinding {
        filename: filename.clone(),
        byte_len: source.bytes.len() as u64,
        sha256: Sha256::digest(&source.bytes).into(),
    };
    make_browser_upload_file(filename, media_type, source.bytes, binding)
}

fn make_browser_upload_file(
    filename: String,
    media_type: String,
    bytes: Vec<u8>,
    binding: BrowserConfirmationBinding,
) -> Result<BrowserUploadFile, String> {
    if bytes.len() > MAX_READ_FILE_BINARY_BYTES
        || bytes.len() as u64 != binding.byte_len
        || <[u8; 32]>::from(Sha256::digest(&bytes)) != binding.sha256
        || filename != binding.filename
        || tidebreak_core::validate_portable_filename(&filename).is_err()
    {
        return Err("browser upload resource is unavailable".to_owned());
    }
    Ok(BrowserUploadFile {
        filename,
        media_type,
        bytes,
        binding,
    })
}

fn completed_snapshot(result: &BrowserPageSnapshot) -> StoredResolution {
    completed(result)
}

fn completed_wait(result: &BrowserWaitResult) -> StoredResolution {
    completed(result)
}

async fn finish_screenshot(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    screenshot: BrowserScreenshotResult,
) -> StoredResolution {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&screenshot.image_base64)
    else {
        return unavailable(
            "browser_screenshot_failed",
            "The browser screenshot could not be read. Take a new screenshot.",
        );
    };
    let app_state = app.state::<std::sync::Arc<AppState>>();
    let published = match crate::image_attachments::publish_image_bytes(
        app_state.inner(),
        state,
        context.chat_id,
        bytes,
    )
    .await
    {
        Ok(published) => published,
        Err(_) => {
            return unavailable(
                "image_publish_failed",
                "The browser screenshot could not be attached to this conversation. Take a new screenshot.",
            )
        }
    };
    let image_refs = image_refs(&published);
    if image_refs.is_empty() {
        return unavailable(
            "image_publish_failed",
            "The browser screenshot could not be attached to this conversation. Take a new screenshot.",
        );
    }
    screenshot_resolution(&screenshot, image_refs)
}

fn image_refs(published: &PublishedImageAttachment) -> Vec<ImageRef> {
    let Some(media_type) = tidebreak_core::ImageMediaType::parse(published.media_type()) else {
        return Vec::new();
    };
    vec![ImageRef {
        blob_id: published.blob_id(),
        media_type,
        width: published.width(),
        height: published.height(),
        byte_len: published.byte_len(),
    }]
}

fn screenshot_resolution(
    screenshot: &BrowserScreenshotResult,
    image_refs: Vec<ImageRef>,
) -> StoredResolution {
    let image = image_refs.first();
    completed_with_images(
        serde_json::json!({
            "browserId": screenshot.browser_id,
            "snapshotId": screenshot.snapshot_id,
            "documentEpoch": screenshot.document_epoch,
            "image": image.map(|image| serde_json::json!({
                "blobId": image.blob_id,
                "mediaType": image.media_type.as_str(),
                "width": image.width,
                "height": image.height,
                "byteLen": image.byte_len,
            })),
        }),
        Some(image_refs),
    )
}

fn completed<T: serde::Serialize>(result: &T) -> StoredResolution {
    match serde_json::to_string(result) {
        Ok(result) if result.len() <= MAX_RESULT_CONTENT_BYTES => StoredResolution::Completed {
            result,
            rows: None,
            images: None,
        },
        _ => unavailable(
            "browser_result_too_large",
            "The browser result was too large to return. Narrow the request and try again.",
        ),
    }
}

fn completed_with_images(
    result: serde_json::Value,
    images: Option<Vec<ImageRef>>,
) -> StoredResolution {
    match serde_json::to_string(&result) {
        Ok(result) if result.len() <= MAX_RESULT_CONTENT_BYTES => StoredResolution::Completed {
            result,
            rows: None,
            images,
        },
        _ => unavailable(
            "browser_result_too_large",
            "The browser result was too large to return. Narrow the request and try again.",
        ),
    }
}

fn invalid_request() -> StoredResolution {
    unavailable("invalid_request", "The browser request was not available.")
}

fn map_native_error(browser_id: Option<&str>, error: String) -> StoredResolution {
    let inner = strip_native_error_prefix(&error);
    if inner == "browser control was stopped by the user" {
        return unavailable(
            "stopped_by_user",
            "The user stopped browser control. Do not retry until the user resumes or shares the browser again.",
        );
    }
    if matches!(
        inner,
        "browser origin is not shared with this agent"
            | "browser origin is not shared for this operation"
            | "browser origin is not shared for control"
            | "browser has no authorized HTTP origin"
    ) {
        return unavailable(
            "browser_not_authorized",
            "This browser site is not shared with the chat. Ask the user to share it in the Browser panel.",
        );
    }
    if matches!(
        inner,
        "browser page is still loading"
            | "browser origin changed before dispatch"
            | "browser document changed while it was being inspected"
            | "browser document changed since the snapshot was taken"
            | "browser document changed while screenshot was being captured"
            | "browser snapshot is stale; take a new browser snapshot"
            | "browser session changed while control was transferring"
            | "browser session was replaced while waiting"
    ) {
        return unavailable(
            "stale_target",
            "The browser page changed. Take a new browser snapshot before continuing.",
        );
    }
    if inner == "semantic browser control is not available on this platform yet" {
        return unavailable(
            "browser_unsupported",
            "This browser operation is not available on this platform.",
        );
    }
    if inner == "browser capability is unavailable" {
        return unavailable(
            "browser_unavailable",
            "Browser access is not available right now. Try again.",
        );
    }
    if inner == "browser upload resource is unavailable" {
        return unavailable(
            "browser_upload_resource_unavailable",
            "That output or connected file is not available to this conversation. Choose a current resource and try again.",
        );
    }
    if inner == "browser upload resource changed before attachment" {
        return unavailable(
            "browser_upload_resource_changed",
            "That file changed before Tidebreak could attach it. Take a new browser snapshot, confirm the current file, and try again.",
        );
    }
    if inner == "browser is hidden" {
        return unavailable(
            "browser_hidden",
            "That browser tab is hidden. Ask the user to make it visible, then list browser tabs again.",
        );
    }
    if inner == "browser is controlled by another agent" {
        return unavailable(
            "browser_busy",
            "Another agent controls that browser tab. Wait for it to finish or ask the user to take over.",
        );
    }
    if inner == "browser is not controlled by this agent" {
        return unavailable(
            "browser_control_lost",
            "This chat no longer controls that browser tab. List browser tabs and take a new snapshot.",
        );
    }
    if browser_id.is_some()
        && (matches!(
            inner,
            "browser session is not registered"
                | "browser session is not open"
                | "browser session belongs to a different workspace"
        ) || inner.contains("belongs to a different workspace"))
    {
        return unavailable(
            "unknown_browser",
            "That browser tab is not available to this chat. List browser tabs again.",
        );
    }
    unavailable(
        "browser_operation_failed",
        "The browser operation failed. Try again once, then tell the user if it still fails.",
    )
}

fn strip_native_error_prefix(error: &str) -> &str {
    for prefix in [
        "screenshot authorization lapsed: ",
        "screenshot recording failed: ",
    ] {
        if let Some(inner) = error.strip_prefix(prefix) {
            return inner;
        }
    }
    error
}

fn unavailable(code: &str, message: &str) -> StoredResolution {
    StoredResolution::Failed {
        result: serde_json::json!({ "status": "unavailable", "message": message }).to_string(),
        error_code: code.to_owned(),
        error_detail: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: serde_json::Value) -> ToolCallRecord {
        ToolCallRecord {
            id: CallId::new(),
            chat_id: SessionId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".to_owned(),
            name: name.to_owned(),
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

    #[test]
    fn discovery_accepts_only_the_seven_validated_foreground_browser_tools() {
        assert!(is_foreground_browser_call(&call(
            BROWSER_LIST_TOOL,
            serde_json::json!({}),
        )));
        assert!(is_foreground_browser_call(&call(
            BROWSER_NAVIGATE_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "url": "https://example.com"
            }),
        )));
        assert!(is_foreground_browser_call(&call(
            BROWSER_SNAPSHOT_TOOL,
            serde_json::json!({ "browser_id": "browser-1" }),
        )));
        assert!(is_foreground_browser_call(&call(
            BROWSER_WAIT_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 1,
                "condition": { "kind": "load_state", "state": "ready" }
            }),
        )));
        assert!(is_foreground_browser_call(&call(
            BROWSER_SCREENSHOT_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 1
            }),
        )));

        assert!(is_foreground_browser_call(&call(
            BROWSER_ACT_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 1,
                "ref": "ref-1",
                "action": { "type": "click" }
            }),
        )));
        assert!(is_foreground_browser_call(&call(
            BROWSER_UPLOAD_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 1,
                "ref": "ref-1",
                "resource": {
                    "kind": "output",
                    "output_id": Uuid::new_v4()
                }
            }),
        )));
        assert!(!is_foreground_browser_call(&call(
            BROWSER_ACT_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 1,
                "ref": "ref-1",
                "action": { "type": "press", "key": "Ctrl+C" }
            }),
        )));
        assert!(!is_foreground_browser_call(&call(
            BROWSER_NAVIGATE_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "url": "file:///etc/passwd"
            }),
        )));
        assert!(!is_foreground_browser_call(&call(
            BROWSER_UPLOAD_TOOL,
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 1,
                "ref": "ref-1",
                "resource": {
                    "kind": "connected_file",
                    "root_id": Uuid::new_v4(),
                    "path": "../secret.txt"
                }
            }),
        )));
        let mut raw = call(BROWSER_LIST_TOOL, serde_json::json!({}));
        raw.raw_arguments = Some("not-json".to_owned());
        assert!(!is_foreground_browser_call(&raw));

        let mut server_owned = call(BROWSER_LIST_TOOL, serde_json::json!({}));
        server_owned.execution = ToolCallExecution::Server;
        assert!(!is_foreground_browser_call(&server_owned));

        let mut completed_call = call(BROWSER_LIST_TOOL, serde_json::json!({}));
        completed_call.status = ToolCallStatus::Completed;
        assert!(!is_foreground_browser_call(&completed_call));
    }

    #[test]
    fn chat_capabilities_are_isolated_reused_and_rotated() {
        let registry = BrowserRegistry::default();
        let state = ForegroundBrowserExecutorState::default();
        let first_chat = Uuid::new_v4();
        let second_chat = Uuid::new_v4();
        let first_scope = format!("foreground-chat:{first_chat}");
        let second_scope = format!("foreground-chat:{second_chat}");

        let first = state
            .capability_for(&registry, first_chat, &first_scope)
            .unwrap();
        assert_eq!(
            state
                .capability_for(&registry, first_chat, &first_scope)
                .unwrap(),
            first
        );
        assert_ne!(
            state
                .capability_for(&registry, second_chat, &second_scope)
                .unwrap(),
            first
        );
        assert!(registry
            .heartbeat_agent_capability(first, &second_scope)
            .is_err());

        registry.expire_agent_capability_for_test(first);
        let replacement = state
            .capability_for(&registry, first_chat, &first_scope)
            .unwrap();
        assert_ne!(replacement, first);
        assert!(registry
            .heartbeat_agent_capability(first, &first_scope)
            .is_err());
        assert!(registry
            .heartbeat_agent_capability(replacement, &first_scope)
            .is_ok());
    }

    #[test]
    fn upload_files_require_exact_portable_bounded_identity() {
        let bytes = b"exact upload bytes".to_vec();
        let binding = BrowserConfirmationBinding {
            filename: "report.pdf".to_owned(),
            byte_len: bytes.len() as u64,
            sha256: Sha256::digest(&bytes).into(),
        };
        let file = make_browser_upload_file(
            binding.filename.clone(),
            "application/pdf".to_owned(),
            bytes.clone(),
            binding.clone(),
        )
        .unwrap();
        assert_eq!(file.filename, "report.pdf");
        assert_eq!(file.bytes, bytes);
        assert!(file.binding == binding);

        let mut changed_digest = binding.clone();
        changed_digest.sha256[0] ^= 1;
        assert!(make_browser_upload_file(
            binding.filename.clone(),
            "application/pdf".to_owned(),
            bytes.clone(),
            changed_digest,
        )
        .is_err());

        let mut changed_length = binding.clone();
        changed_length.byte_len += 1;
        assert!(make_browser_upload_file(
            binding.filename.clone(),
            "application/pdf".to_owned(),
            bytes.clone(),
            changed_length,
        )
        .is_err());

        for filename in ["../secret", ".hidden", "folder/report.pdf", "report?.pdf"] {
            let malicious = BrowserConfirmationBinding {
                filename: filename.to_owned(),
                byte_len: bytes.len() as u64,
                sha256: Sha256::digest(&bytes).into(),
            };
            assert!(make_browser_upload_file(
                filename.to_owned(),
                "application/octet-stream".to_owned(),
                bytes.clone(),
                malicious,
            )
            .is_err());
        }

        let oversized = vec![0; MAX_READ_FILE_BINARY_BYTES + 1];
        let oversized_binding = BrowserConfirmationBinding {
            filename: "large.bin".to_owned(),
            byte_len: oversized.len() as u64,
            sha256: Sha256::digest(&oversized).into(),
        };
        assert!(make_browser_upload_file(
            oversized_binding.filename.clone(),
            "application/octet-stream".to_owned(),
            oversized,
            oversized_binding,
        )
        .is_err());
    }

    #[test]
    fn screenshot_resolution_keeps_pixels_and_base64_out_of_durable_output() {
        let image = ImageRef {
            blob_id: Uuid::new_v4(),
            media_type: tidebreak_core::ImageMediaType::Png,
            width: 800,
            height: 600,
            byte_len: 4,
        };
        let screenshot = BrowserScreenshotResult {
            browser_id: "browser-1".to_owned(),
            snapshot_id: "snapshot-1".to_owned(),
            document_epoch: 7,
            image_base64: "cGl4ZWxz".to_owned(),
            mime_type: "image/png".to_owned(),
        };

        let StoredResolution::Completed { result, images, .. } =
            screenshot_resolution(&screenshot, vec![image])
        else {
            panic!("screenshot metadata should fit");
        };
        assert!(!result.contains("cGl4ZWxz"));
        assert!(!result.contains("imageBase64"));
        assert_eq!(images, Some(vec![image]));
    }

    #[test]
    fn native_errors_map_to_stable_results_without_host_details() {
        let StoredResolution::Failed {
            result,
            error_code,
            error_detail,
        } = map_native_error(
            Some("copied-browser-id"),
            "browser session belongs to a different workspace: secret-scope".to_owned(),
        )
        else {
            panic!("a scope mismatch must fail");
        };
        assert_eq!(error_code, "unknown_browser");
        assert_eq!(error_detail, None);
        assert!(!result.contains("secret-scope"));

        let StoredResolution::Failed { error_code, .. } = map_native_error(
            Some("browser-1"),
            "browser is controlled by another agent".to_owned(),
        ) else {
            panic!("another controller must fail");
        };
        assert_eq!(error_code, "browser_busy");

        let StoredResolution::Failed { error_code, .. } = map_native_error(
            Some("browser-1"),
            "browser upload resource is unavailable".to_owned(),
        ) else {
            panic!("an unavailable logical resource must fail");
        };
        assert_eq!(error_code, "browser_upload_resource_unavailable");

        let StoredResolution::Failed { error_code, .. } = map_native_error(
            Some("browser-1"),
            "browser upload resource changed before attachment".to_owned(),
        ) else {
            panic!("a changed logical resource must fail");
        };
        assert_eq!(error_code, "browser_upload_resource_changed");
    }

    #[test]
    fn foreground_browser_receipts_round_trip_without_native_authority() {
        let temp = tempfile::tempdir().unwrap();
        let store = super::super::receipt_store::ReceiptStore::open(temp.path()).unwrap();
        let receipt =
            ForegroundBrowserReceipt::new(SessionId::new(), CallId::new(), store.executor_id());
        let serialized = serde_json::to_value(&receipt).unwrap();
        let keys = serialized
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            keys,
            std::collections::HashSet::from([
                "version",
                "chat_id",
                "call_id",
                "executor_id",
                "lease_token",
                "phase",
            ])
        );
        store.save_foreground_browser(&receipt).unwrap();
        drop(store);
        let store = super::super::receipt_store::ReceiptStore::open(temp.path()).unwrap();
        assert_eq!(
            store.load_foreground_browsers().unwrap(),
            vec![receipt.clone()]
        );
        let debug = format!("{receipt:?}");
        assert!(!debug.contains(&receipt.lease_token.to_string()));
        assert!(!debug.contains("foreground-chat:"));
        store.remove_foreground_browser(receipt.call_id).unwrap();
        assert!(store.load_foreground_browsers().unwrap().is_empty());
    }
}
