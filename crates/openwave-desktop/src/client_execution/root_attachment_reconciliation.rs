//! Native reconciliation for manual connected-folder mutations.
//!
//! Product intent is committed before the broker mutation. Every broker call
//! uses the product change UUID as its idempotency identity, and recovery
//! always looks up that exact receipt before dispatching it again.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Timelike, Utc};
use openwave_core::{
    ChatId, HostRootId, RootAttachmentChangeAction, RootAttachmentChangeFailure,
    RootAttachmentChangeId, RootAttachmentChangePhase, RootAttachmentChangeTerminal,
    RootAttachmentSubjectKind, MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};
use openwave_host_broker::{
    ConsentMethod, ControlRequest, ControlResult, ExecutionContext,
    LookupRegisterRootReceiptRequest, LookupRootAttachmentReceiptRequest, OperationId,
    RegisterRootReceipt, RegisterRootRequest, RootAttachmentMutationKind,
    RootAttachmentMutationReceipt, RootAttachmentMutationRequest, RootId, RootSummary, SubjectKind,
};
use tauri::Manager;

use super::control_plane::{ControlPlaneError, RootAttachmentChangeView};
use super::{
    control_plane, control_plane_error, ManualFolderConnectReceipt, ProductRootAttachmentSync,
    RegistrationPhase,
};
use crate::host_access::{AuthoritativeContext, ConnectedFolder, HostAccess};

const RECOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const RECOVERY_LIMIT: usize = 64;
static MANUAL_RECOVERY_CURSOR: AtomicUsize = AtomicUsize::new(0);
static PRODUCT_RECOVERY_CURSOR: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct BeginFingerprint {
    id: RootAttachmentChangeId,
    chat_id: ChatId,
    root_id: HostRootId,
    action: RootAttachmentChangeAction,
    expected_revision: i64,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectMode {
    Interactive,
    Recovery,
}

/// Register a picker-selected path and report success only after product and
/// broker attachment state have converged.
pub(crate) async fn connect_selected_folder(
    state: &HostAccess,
    context: AuthoritativeContext,
    path: PathBuf,
) -> Result<ConnectedFolder, String> {
    let mut receipt =
        ManualFolderConnectReceipt::new(ChatId::from(context.chat_id), context.subject, path);
    state
        .receipts
        .save_manual_connect(&receipt)
        .map_err(private_receipt_error)?;
    drive_manual_connect(state, &mut receipt, ConnectMode::Interactive).await
}

/// Attach a host-approved root to one exact conversation after native consent.
///
/// The root summary carries no path or authority. The broker creates the
/// conversation grant only when the durable product attachment is driven.
pub(crate) async fn connect_existing_root(
    state: &HostAccess,
    context: AuthoritativeContext,
    root: RootSummary,
) -> Result<ConnectedFolder, String> {
    let product_root_id = HostRootId::from_uuid(root.root_id.as_uuid())
        .map_err(|_| "invalid approved folder identity".to_owned())?;
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(ChatId::from(context.chat_id))
        .await
        .map_err(|_| "could not load connected folders".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    let fingerprint = BeginFingerprint {
        id: RootAttachmentChangeId::new(),
        chat_id: chat.id,
        root_id: product_root_id,
        action: RootAttachmentChangeAction::Attach,
        expected_revision: chat.attachment_revision,
        created_at: canonical_now(),
    };
    let begun = control_plane(state)?
        .begin_root_attachment_change(
            fingerprint.chat_id,
            fingerprint.id,
            fingerprint.root_id,
            fingerprint.action,
            fingerprint.expected_revision,
            fingerprint.created_at,
        )
        .await
        .map_err(begin_error)?;
    validate_begin_change(&begun.change, context, fingerprint)?;
    let finished = drive_change(state, begun.change).await?;
    verify_product_terminal(state, &finished).await?;
    match finished.phase {
        RootAttachmentChangePhase::Completed => Ok(connected_folder(root)),
        RootAttachmentChangePhase::Failed => {
            Err("The approved folder could not be connected to this chat.".to_owned())
        }
        RootAttachmentChangePhase::AwaitingBroker => {
            Err("folder attachment is still pending".to_owned())
        }
    }
}

/// Begin and drive an exact conversation-only detach. Global root revocation
/// is deliberately not part of the renderer's connected-folder action.
pub(crate) async fn disconnect_root(
    state: &HostAccess,
    context: AuthoritativeContext,
    root_id: RootId,
) -> Result<bool, String> {
    let product_root_id = HostRootId::from_uuid(root_id.as_uuid())
        .map_err(|_| "invalid connected folder identity".to_owned())?;
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(ChatId::from(context.chat_id))
        .await
        .map_err(|_| "could not load connected folders".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    if !chat
        .root_attachments
        .iter()
        .any(|attachment| attachment.root_id == product_root_id)
    {
        return Ok(false);
    }

    let fingerprint = BeginFingerprint {
        id: RootAttachmentChangeId::new(),
        chat_id: chat.id,
        root_id: product_root_id,
        action: RootAttachmentChangeAction::Detach,
        expected_revision: chat.attachment_revision,
        created_at: canonical_now(),
    };
    let begun = control_plane(state)?
        .begin_root_attachment_change(
            fingerprint.chat_id,
            fingerprint.id,
            fingerprint.root_id,
            fingerprint.action,
            fingerprint.expected_revision,
            fingerprint.created_at,
        )
        .await
        .map_err(begin_error)?;
    validate_begin_change(&begun.change, context, fingerprint)?;
    let finished = drive_change(state, begun.change).await?;
    verify_product_terminal(state, &finished).await?;
    match finished.phase {
        RootAttachmentChangePhase::Completed => Ok(true),
        RootAttachmentChangePhase::Failed => {
            Err("The folder could not be disconnected safely. It remains connected.".to_owned())
        }
        RootAttachmentChangePhase::AwaitingBroker => {
            Err("folder disconnection is still pending".to_owned())
        }
    }
}

/// Bounded startup and steady-state recovery of private picker receipts and
/// server-owned attachment changes. One native loop owns this work; each pass
/// is sequential and capped so large backlogs cannot monopolize the runtime.
pub(crate) async fn recover_root_attachment_changes(app: tauri::AppHandle) {
    loop {
        if let Err(error) = recover_once(&app).await {
            eprintln!("openwave-desktop: root attachment recovery deferred: {error}");
        }
        tokio::time::sleep(RECOVERY_INTERVAL).await;
    }
}

async fn recover_once(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<HostAccess>();
    let receipts = state
        .receipts
        .load_manual_connects()
        .map_err(private_receipt_error)?;
    let locally_owned = receipts
        .iter()
        .map(|receipt| receipt.change_id)
        .collect::<HashSet<_>>();
    for mut receipt in rotating_batch(receipts, &MANUAL_RECOVERY_CURSOR) {
        let exclusive = state.root_changes.lock().await;
        if let Err(error) =
            drive_manual_connect(state.inner(), &mut receipt, ConnectMode::Recovery).await
        {
            eprintln!("openwave-desktop: manual folder connection deferred: {error}");
        }
        drop(exclusive);
        tokio::task::yield_now().await;
    }

    let pending = control_plane(state.inner())?
        .pending_root_attachment_changes(MAX_PENDING_ROOT_ATTACHMENT_CHANGES as usize)
        .await
        .map_err(control_plane_error)?;
    for change in rotating_batch(pending, &PRODUCT_RECOVERY_CURSOR) {
        if locally_owned.contains(&change.id) {
            continue;
        }
        let exclusive = state.root_changes.lock().await;
        if let Err(error) = recover_pending_change(state.inner(), change).await {
            eprintln!("openwave-desktop: pending root attachment deferred: {error}");
        }
        drop(exclusive);
        tokio::task::yield_now().await;
    }
    Ok(())
}

fn rotating_batch<T>(mut work: Vec<T>, cursor: &AtomicUsize) -> impl Iterator<Item = T> {
    if !work.is_empty() {
        let start = cursor.fetch_add(RECOVERY_LIMIT, Ordering::Relaxed) % work.len();
        work.rotate_left(start);
    }
    work.into_iter().take(RECOVERY_LIMIT)
}

async fn recover_pending_change(
    state: &HostAccess,
    change: RootAttachmentChangeView,
) -> Result<(), String> {
    let context = state.context(change.chat_id.0).await?;
    validate_change_context(&change, context)?;
    let finished = drive_change(state, change).await?;
    verify_product_terminal(state, &finished).await
}

async fn drive_manual_connect(
    state: &HostAccess,
    receipt: &mut ManualFolderConnectReceipt,
    mode: ConnectMode,
) -> Result<ConnectedFolder, String> {
    if receipt.registration_phase == RegistrationPhase::NotStarted {
        if mode == ConnectMode::Recovery {
            // The picker selection was persisted, but native dispatch was not.
            // A restart must never turn that stale selection into new authority.
            state
                .receipts
                .remove_manual_connect(receipt.change_id)
                .map_err(private_receipt_error)?;
            return Err("unstarted folder connection was discarded".to_owned());
        }
        receipt.registration_phase = RegistrationPhase::Attempted;
        state
            .receipts
            .save_manual_connect(receipt)
            .map_err(private_receipt_error)?;
        let deadline = tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
        let first = state
            .broker
            .control_without_retry(
                ControlRequest::RegisterRoot(RegisterRootRequest {
                    operation_id: receipt.registration_operation_id,
                    subject: receipt.subject,
                    conversation_id: receipt.chat_id.0,
                    path: receipt.path.clone(),
                    consent_method: ConsentMethod::FolderPicker,
                }),
                deadline,
            )
            .await;
        match first {
            Ok(ControlResult::RegisterRoot(_)) | Err(_) => {}
            Ok(_) => {
                return Err("host broker returned an unexpected registration response".to_owned())
            }
        }
    }

    let root = match lookup_registration(state, receipt).await? {
        RegisterRootReceipt::Completed { root } => root,
        RegisterRootReceipt::Unknown if mode == ConnectMode::Recovery => {
            state
                .receipts
                .remove_manual_connect(receipt.change_id)
                .map_err(private_receipt_error)?;
            return Err("unknown folder registration was discarded".to_owned());
        }
        RegisterRootReceipt::Unknown | RegisterRootReceipt::Pending => {
            return Err("folder registration has no durable outcome yet".to_owned())
        }
        RegisterRootReceipt::Disconnected { .. } | RegisterRootReceipt::Failed { .. } => {
            state
                .receipts
                .remove_manual_connect(receipt.change_id)
                .map_err(private_receipt_error)?;
            return Err("The selected folder could not be connected.".to_owned());
        }
        _ => return Err("host broker returned an unsupported registration receipt".to_owned()),
    };

    let context = match state.context(receipt.chat_id.0).await {
        Ok(context) if context.subject == receipt.subject => context,
        _ => {
            if receipt.product_sync.is_none() {
                install_product_sync(receipt, &root, 0)?;
                state
                    .receipts
                    .save_manual_connect(receipt)
                    .map_err(private_receipt_error)?;
            }
            cleanup_rejected_registration(state, receipt, stored_context(receipt)?).await?;
            return Err("conversation authority changed during folder registration".to_owned());
        }
    };
    if receipt.product_sync.is_none() {
        let store = state
            .store()
            .ok_or_else(|| "OpenWave is still starting".to_owned())?;
        let chat = store
            .get_chat(receipt.chat_id)
            .await
            .map_err(|_| "could not load the folder attachment revision".to_owned())?
            .ok_or_else(|| "conversation not found".to_owned())?;
        install_product_sync(receipt, &root, chat.attachment_revision)?;
        state
            .receipts
            .save_manual_connect(receipt)
            .map_err(private_receipt_error)?;
    }
    let sync = receipt
        .product_sync
        .clone()
        .ok_or_else(|| "manual folder attachment metadata is missing".to_owned())?;
    if *sync.root_id.as_uuid() != root.root_id.as_uuid()
        || sync.display_name != root.display_name
        || sync.change_id != receipt.change_id
    {
        return Err("registered folder changed during recovery".to_owned());
    }

    let begun = match control_plane(state)?
        .begin_root_attachment_change(
            receipt.chat_id,
            sync.change_id,
            sync.root_id,
            RootAttachmentChangeAction::Attach,
            sync.expected_attachment_revision,
            sync.created_at,
        )
        .await
    {
        Ok(begun) => begun,
        Err(error) if permanent_begin_failure(&error) => {
            if product_contains_root(state, receipt.chat_id, sync.root_id).await? {
                match lookup_registration(state, receipt).await? {
                    RegisterRootReceipt::Completed { root: current }
                        if current.root_id == root.root_id
                            && current.display_name == root.display_name =>
                    {
                        state
                            .receipts
                            .remove_manual_connect(receipt.change_id)
                            .map_err(private_receipt_error)?;
                        return Ok(connected_folder(root));
                    }
                    _ => {
                        return Err("product and broker folder attachment state disagree".to_owned())
                    }
                }
            }
            cleanup_rejected_registration(state, receipt, context).await?;
            return Err(
                "The selected folder could not be synchronized with this conversation.".to_owned(),
            );
        }
        Err(error) => return Err(begin_error(error)),
    };
    validate_begin_change(
        &begun.change,
        context,
        BeginFingerprint {
            id: sync.change_id,
            chat_id: receipt.chat_id,
            root_id: sync.root_id,
            action: RootAttachmentChangeAction::Attach,
            expected_revision: sync.expected_attachment_revision,
            created_at: sync.created_at,
        },
    )?;
    let finished = drive_change(state, begun.change).await?;
    verify_product_terminal(state, &finished).await?;
    match finished.phase {
        RootAttachmentChangePhase::Completed => {
            state
                .receipts
                .remove_manual_connect(receipt.change_id)
                .map_err(private_receipt_error)?;
            Ok(connected_folder(root))
        }
        RootAttachmentChangePhase::Failed => {
            state
                .receipts
                .remove_manual_connect(receipt.change_id)
                .map_err(private_receipt_error)?;
            Err("The selected folder could not be synchronized with this conversation.".to_owned())
        }
        RootAttachmentChangePhase::AwaitingBroker => {
            Err("folder attachment is still pending".to_owned())
        }
    }
}

fn install_product_sync(
    receipt: &mut ManualFolderConnectReceipt,
    root: &RootSummary,
    expected_attachment_revision: i64,
) -> Result<(), String> {
    let root_id = HostRootId::from_uuid(root.root_id.as_uuid())
        .map_err(|_| "host broker returned an invalid root identity".to_owned())?;
    receipt.product_sync = Some(ProductRootAttachmentSync {
        change_id: receipt.change_id,
        root_id,
        display_name: root.display_name.clone(),
        expected_attachment_revision,
        created_at: canonical_now(),
        cleanup_operation_id: receipt.cleanup_operation_id,
        cleanup_phase: super::receipt_store::CleanupPhase::NotStarted,
        attachment_phase: super::receipt_store::AttachmentPhase::Prepared,
    });
    Ok(())
}

fn stored_context(receipt: &ManualFolderConnectReceipt) -> Result<AuthoritativeContext, String> {
    let execution = match receipt.subject.kind() {
        SubjectKind::Project => {
            ExecutionContext::project_chat(receipt.chat_id.0, receipt.subject.id())
        }
        SubjectKind::Conversation => ExecutionContext::standalone(receipt.chat_id.0),
    }
    .map_err(|_| "invalid stored folder authority".to_owned())?;
    Ok(AuthoritativeContext {
        chat_id: receipt.chat_id.0,
        execution,
        subject: receipt.subject,
    })
}

async fn lookup_registration(
    state: &HostAccess,
    receipt: &ManualFolderConnectReceipt,
) -> Result<RegisterRootReceipt, String> {
    let result = state
        .broker
        .control(ControlRequest::LookupRegisterRootReceipt(
            LookupRegisterRootReceiptRequest {
                operation_id: receipt.registration_operation_id,
                subject: receipt.subject,
                conversation_id: receipt.chat_id.0,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::LookupRegisterRootReceipt(result) = result else {
        return Err("host broker returned an unexpected registration receipt".to_owned());
    };
    if result.operation_id != receipt.registration_operation_id {
        return Err("host broker returned a mismatched registration receipt".to_owned());
    }
    Ok(result.receipt)
}

async fn cleanup_rejected_registration(
    state: &HostAccess,
    receipt: &mut ManualFolderConnectReceipt,
    context: AuthoritativeContext,
) -> Result<(), String> {
    let sync = receipt
        .product_sync
        .clone()
        .ok_or_else(|| "manual folder cleanup metadata is missing".to_owned())?;
    if product_contains_root(state, receipt.chat_id, sync.root_id).await? {
        return Err("folder is already present in product state".to_owned());
    }
    let root_id = RootId::from_uuid(*sync.root_id.as_uuid())
        .map_err(|_| "invalid cleanup root identity".to_owned())?;
    let mut cleanup = lookup_mutation(
        state,
        context,
        sync.cleanup_operation_id,
        root_id,
        RootAttachmentChangeAction::Detach,
    )
    .await?;
    if matches!(cleanup, RootAttachmentMutationReceipt::Unknown) {
        if let Some(stored) = receipt.product_sync.as_mut() {
            stored.cleanup_phase = super::receipt_store::CleanupPhase::DispatchAttempted;
        }
        state
            .receipts
            .save_manual_connect(receipt)
            .map_err(private_receipt_error)?;
        dispatch_mutation(
            state,
            context,
            sync.cleanup_operation_id,
            root_id,
            RootAttachmentChangeAction::Detach,
        )
        .await?;
        cleanup = lookup_mutation(
            state,
            context,
            sync.cleanup_operation_id,
            root_id,
            RootAttachmentChangeAction::Detach,
        )
        .await?;
    }
    match cleanup {
        RootAttachmentMutationReceipt::Completed {
            result,
            currently_attached: false,
        } if result.root_id == root_id && result.mutation == RootAttachmentMutationKind::Detach => {
        }
        RootAttachmentMutationReceipt::Failed { .. } => {
            let registration = lookup_registration(state, receipt).await?;
            match registration {
                RegisterRootReceipt::Disconnected { root }
                    if root.root_id == root_id && root.display_name == sync.display_name => {}
                _ => return Err("folder cleanup has no authoritative detached outcome".to_owned()),
            }
        }
        _ => return Err("folder cleanup has no durable detached outcome yet".to_owned()),
    }
    if product_contains_root(state, receipt.chat_id, sync.root_id).await? {
        return Err("folder cleanup contradicts product state".to_owned());
    }
    if let Some(stored) = receipt.product_sync.as_mut() {
        stored.cleanup_phase = super::receipt_store::CleanupPhase::Completed;
    }
    state
        .receipts
        .save_manual_connect(receipt)
        .map_err(private_receipt_error)?;
    state
        .receipts
        .remove_manual_connect(receipt.change_id)
        .map_err(private_receipt_error)
}

async fn drive_change(
    state: &HostAccess,
    change: RootAttachmentChangeView,
) -> Result<RootAttachmentChangeView, String> {
    if change.phase != RootAttachmentChangePhase::AwaitingBroker {
        return Ok(change);
    }
    let context = state.context(change.chat_id.0).await?;
    validate_change_context(&change, context)?;
    let operation_id = operation_id(change.id)?;
    let root_id = RootId::from_uuid(*change.root_id.as_uuid())
        .map_err(|_| "invalid product root identity".to_owned())?;
    let mut receipt = lookup_mutation(state, context, operation_id, root_id, change.action).await?;
    if matches!(receipt, RootAttachmentMutationReceipt::Unknown) {
        dispatch_mutation(state, context, operation_id, root_id, change.action).await?;
        receipt = lookup_mutation(state, context, operation_id, root_id, change.action).await?;
    }
    let terminal = mutation_terminal(receipt, root_id, change.action)?
        .ok_or_else(|| "root attachment has no durable broker outcome yet".to_owned())?;
    let finished = control_plane(state)?
        .finish_root_attachment_change(change.id, &terminal)
        .await
        .map_err(finish_error)?;
    validate_same_change(&change, &finished.change)?;
    Ok(finished.change)
}

async fn lookup_mutation(
    state: &HostAccess,
    context: AuthoritativeContext,
    operation_id: OperationId,
    root_id: RootId,
    action: RootAttachmentChangeAction,
) -> Result<RootAttachmentMutationReceipt, String> {
    let result = state
        .broker
        .control(ControlRequest::LookupRootAttachmentReceipt(
            LookupRootAttachmentReceiptRequest {
                operation_id,
                subject: context.subject,
                conversation_id: context.chat_id,
                root_id,
                mutation: mutation_kind(action),
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::LookupRootAttachmentReceipt(result) = result else {
        return Err("host broker returned an unexpected attachment receipt".to_owned());
    };
    if result.operation_id != operation_id {
        return Err("host broker returned a mismatched attachment receipt".to_owned());
    }
    Ok(result.receipt)
}

async fn dispatch_mutation(
    state: &HostAccess,
    context: AuthoritativeContext,
    operation_id: OperationId,
    root_id: RootId,
    action: RootAttachmentChangeAction,
) -> Result<(), String> {
    let request = RootAttachmentMutationRequest {
        operation_id,
        subject: context.subject,
        conversation_id: context.chat_id,
        root_id,
        consent_method: match action {
            RootAttachmentChangeAction::Attach => Some(ConsentMethod::PermissionDialog),
            RootAttachmentChangeAction::Detach => None,
        },
    };
    let control = match action {
        RootAttachmentChangeAction::Attach => ControlRequest::AttachRoot(request),
        RootAttachmentChangeAction::Detach => ControlRequest::DetachRoot(request),
    };
    let deadline = tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
    match state.broker.control_without_retry(control, deadline).await {
        Ok(ControlResult::AttachRoot(_)) if action == RootAttachmentChangeAction::Attach => Ok(()),
        Ok(ControlResult::DetachRoot(_)) if action == RootAttachmentChangeAction::Detach => Ok(()),
        Err(_) => Ok(()),
        Ok(_) => Err("host broker returned an unexpected attachment response".to_owned()),
    }
}

fn mutation_terminal(
    receipt: RootAttachmentMutationReceipt,
    root_id: RootId,
    action: RootAttachmentChangeAction,
) -> Result<Option<RootAttachmentChangeTerminal>, String> {
    let desired = action == RootAttachmentChangeAction::Attach;
    match receipt {
        RootAttachmentMutationReceipt::Unknown => Ok(None),
        RootAttachmentMutationReceipt::Completed {
            result,
            currently_attached,
        } => {
            if result.root_id != root_id || result.mutation != mutation_kind(action) {
                return Err("broker attachment receipt contradicts product intent".to_owned());
            }
            if currently_attached == desired {
                Ok(Some(RootAttachmentChangeTerminal::Completed {
                    broker_changed: result.changed,
                    broker_currently_attached: currently_attached,
                }))
            } else {
                Ok(Some(RootAttachmentChangeTerminal::Failed {
                    broker_changed: Some(result.changed),
                    broker_currently_attached: Some(currently_attached),
                    failure: safe_failure(
                        "broker_attachment_superseded",
                        "The folder attachment changed before synchronization completed.",
                    ),
                }))
            }
        }
        RootAttachmentMutationReceipt::Failed { .. } => {
            Ok(Some(RootAttachmentChangeTerminal::Failed {
                broker_changed: None,
                broker_currently_attached: None,
                failure: safe_failure(
                    "broker_attachment_failed",
                    "The host broker could not complete this folder attachment change.",
                ),
            }))
        }
        _ => Err("host broker returned an unsupported attachment receipt".to_owned()),
    }
}

fn safe_failure(code: &str, message: &str) -> RootAttachmentChangeFailure {
    RootAttachmentChangeFailure {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable: false,
    }
}

fn mutation_kind(action: RootAttachmentChangeAction) -> RootAttachmentMutationKind {
    match action {
        RootAttachmentChangeAction::Attach => RootAttachmentMutationKind::Attach,
        RootAttachmentChangeAction::Detach => RootAttachmentMutationKind::Detach,
    }
}

fn operation_id(change_id: RootAttachmentChangeId) -> Result<OperationId, String> {
    OperationId::from_uuid(*change_id.as_uuid())
        .map_err(|_| "invalid root attachment change identity".to_owned())
}

fn validate_begin_change(
    change: &RootAttachmentChangeView,
    context: AuthoritativeContext,
    expected: BeginFingerprint,
) -> Result<(), String> {
    validate_change_context(change, context)?;
    if change.id != expected.id
        || change.chat_id != expected.chat_id
        || change.root_id != expected.root_id
        || change.action != expected.action
        || change.expected_revision != expected.expected_revision
        || change.created_at != expected.created_at
    {
        return Err("local control plane returned the wrong root attachment operation".to_owned());
    }
    Ok(())
}

fn validate_change_context(
    change: &RootAttachmentChangeView,
    context: AuthoritativeContext,
) -> Result<(), String> {
    let expected_subject_kind = match context.subject.kind() {
        SubjectKind::Project => RootAttachmentSubjectKind::Project,
        SubjectKind::Conversation => RootAttachmentSubjectKind::Conversation,
    };
    let expected_intent_revision = change.before_revision.checked_add(i64::from(
        change.action == RootAttachmentChangeAction::Attach && !change.projection_existed_before,
    ));
    let terminal_shape_valid = match change.phase {
        RootAttachmentChangePhase::AwaitingBroker => {
            change.result_revision.is_none()
                && change.broker_currently_attached.is_none()
                && change.failure.is_none()
        }
        RootAttachmentChangePhase::Completed => {
            change.result_revision.is_some()
                && change.broker_currently_attached
                    == Some(change.action == RootAttachmentChangeAction::Attach)
                && change.failure.is_none()
        }
        RootAttachmentChangePhase::Failed => {
            change.result_revision.is_some()
                && change.failure.is_some()
                && !change.broker_currently_attached.is_some_and(|attached| {
                    attached == (change.action == RootAttachmentChangeAction::Attach)
                })
        }
    };
    if change.id.as_uuid().is_nil()
        || change.chat_id.0 != context.chat_id
        || change.subject_kind != expected_subject_kind
        || change.subject_id != context.subject.id()
        || change.before_revision != change.expected_revision
        || expected_intent_revision != Some(change.intent_revision)
        || !terminal_shape_valid
    {
        return Err("local control plane returned a mismatched root attachment change".to_owned());
    }
    Ok(())
}

fn validate_same_change(
    expected: &RootAttachmentChangeView,
    actual: &RootAttachmentChangeView,
) -> Result<(), String> {
    if actual.id != expected.id
        || actual.chat_id != expected.chat_id
        || actual.root_id != expected.root_id
        || actual.action != expected.action
        || actual.subject_kind != expected.subject_kind
        || actual.subject_id != expected.subject_id
        || actual.expected_revision != expected.expected_revision
        || actual.before_revision != expected.before_revision
        || actual.intent_revision != expected.intent_revision
        || actual.projection_existed_before != expected.projection_existed_before
        || actual.created_at != expected.created_at
        || actual.phase == RootAttachmentChangePhase::AwaitingBroker
    {
        return Err("local control plane changed root attachment identity".to_owned());
    }
    Ok(())
}

fn canonical_now() -> DateTime<Utc> {
    let now = Utc::now();
    now.with_nanosecond((now.nanosecond() / 1_000) * 1_000)
        .expect("microsecond timestamp is valid")
}

async fn verify_product_terminal(
    state: &HostAccess,
    change: &RootAttachmentChangeView,
) -> Result<(), String> {
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(change.chat_id)
        .await
        .map_err(|_| "could not verify connected folders".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    if Some(chat.attachment_revision) != change.result_revision {
        return Err("product folder revision does not match its terminal change".to_owned());
    }
    let attached = chat
        .root_attachments
        .iter()
        .any(|attachment| attachment.root_id == change.root_id);
    let expected = match change.phase {
        RootAttachmentChangePhase::Completed => change.action == RootAttachmentChangeAction::Attach,
        RootAttachmentChangePhase::Failed => change.projection_existed_before,
        RootAttachmentChangePhase::AwaitingBroker => {
            return Err("root attachment is not terminal".to_owned())
        }
    };
    if attached != expected {
        return Err("product and broker folder attachment state disagree".to_owned());
    }
    Ok(())
}

async fn product_contains_root(
    state: &HostAccess,
    chat_id: ChatId,
    root_id: HostRootId,
) -> Result<bool, String> {
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    Ok(store
        .get_chat(chat_id)
        .await
        .map_err(|_| "could not inspect connected folders".to_owned())?
        .is_some_and(|chat| {
            chat.root_attachments
                .iter()
                .any(|attachment| attachment.root_id == root_id)
        }))
}

fn connected_folder(root: RootSummary) -> ConnectedFolder {
    ConnectedFolder {
        root_id: root.root_id,
        display_name: root.display_name,
    }
}

fn permanent_begin_failure(error: &ControlPlaneError) -> bool {
    [
        "root_attachment_revision_conflict",
        "root_attachment_identity_conflict",
        "root_attachment_capacity_exceeded",
        "root_attachment_revision_exhausted",
    ]
    .iter()
    .any(|kind| error.is_kind(kind))
}

fn begin_error(error: ControlPlaneError) -> String {
    if error.is_conflict() {
        "folder attachment could not begin safely".to_owned()
    } else {
        control_plane_error(error)
    }
}

fn finish_error(error: ControlPlaneError) -> String {
    if error.is_conflict() {
        "folder attachment failed closed".to_owned()
    } else {
        control_plane_error(error)
    }
}

fn private_receipt_error(_error: std::io::Error) -> String {
    "could not update private folder recovery state".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_host_broker::{ErrorCode, ErrorResponse, RootAttachmentMutationResult};

    #[test]
    fn attach_and_detach_receipts_map_to_exact_terminals() {
        let root_id = RootId::new();
        for action in [
            RootAttachmentChangeAction::Attach,
            RootAttachmentChangeAction::Detach,
        ] {
            let desired = action == RootAttachmentChangeAction::Attach;
            let terminal = mutation_terminal(
                RootAttachmentMutationReceipt::Completed {
                    result: RootAttachmentMutationResult {
                        root_id,
                        mutation: mutation_kind(action),
                        changed: true,
                    },
                    currently_attached: desired,
                },
                root_id,
                action,
            )
            .unwrap()
            .unwrap();
            assert!(matches!(
                terminal,
                RootAttachmentChangeTerminal::Completed { .. }
            ));
        }
    }

    #[test]
    fn stale_current_state_and_broker_errors_fail_with_safe_bounded_details() {
        let root_id = RootId::new();
        let stale = mutation_terminal(
            RootAttachmentMutationReceipt::Completed {
                result: RootAttachmentMutationResult {
                    root_id,
                    mutation: RootAttachmentMutationKind::Attach,
                    changed: true,
                },
                currently_attached: false,
            },
            root_id,
            RootAttachmentChangeAction::Attach,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(stale, RootAttachmentChangeTerminal::Failed { .. }));

        let failed = mutation_terminal(
            RootAttachmentMutationReceipt::Failed {
                error: ErrorResponse {
                    code: ErrorCode::HostIo,
                    message: "/private/secret/path".to_owned(),
                    retryable: true,
                },
            },
            root_id,
            RootAttachmentChangeAction::Detach,
        )
        .unwrap()
        .unwrap();
        let RootAttachmentChangeTerminal::Failed { failure, .. } = failed else {
            panic!("expected safe failure")
        };
        assert!(!failure.message.contains("secret"));
        assert!(!failure.retryable);
    }
}
