//! Durable synchronization of a picker-registered broker root into the
//! product conversation projection.

use openwave_core::{
    HostRootId, RootAttachmentChangeAction, RootAttachmentChangeFailure, RootAttachmentChangeId,
    RootAttachmentChangePhase, RootAttachmentChangeTerminal, RootAttachmentSubjectKind,
};
use openwave_host_broker::{
    ConsentMethod, ControlRequest, ControlResult, LookupRegisterRootReceiptRequest,
    LookupRootAttachmentReceiptRequest, OperationId, RegisterRootReceipt,
    RootAttachmentMutationKind, RootAttachmentMutationReceipt, RootAttachmentMutationRequest,
    RootId, RootSummary, SubjectKind,
};

use super::control_plane::{ControlPlaneError, RootAttachmentChangeView};
use super::receipt_store::{
    AttachmentPhase, CleanupPhase, FolderAccessReceipt, ProductRootAttachmentSync, StoredResolution,
};
use super::{
    connected_resolution, control_plane, control_plane_error, failed_resolution,
    private_receipt_error,
};
use crate::host_access::{AuthoritativeContext, HostAccess};

pub(super) async fn synchronize_product_attachment(
    state: &HostAccess,
    receipt: &mut FolderAccessReceipt,
    registration_context: AuthoritativeContext,
    root: RootSummary,
) -> Result<StoredResolution, String> {
    let root_id = HostRootId::from_uuid(root.root_id.as_uuid())
        .map_err(|_| "host broker returned an invalid registered root".to_owned())?;
    let current_context = state.context(receipt.chat_id.0).await?;
    if current_context.subject != registration_context.subject
        || current_context.chat_id != registration_context.chat_id
    {
        return Err("conversation authority changed during folder registration".to_owned());
    }

    if receipt.product_sync.is_none() {
        let store = state
            .store()
            .ok_or_else(|| "OpenWave is still starting".to_owned())?;
        let chat = store
            .get_chat(receipt.chat_id)
            .await
            .map_err(|_| "could not load the conversation attachment revision".to_owned())?
            .ok_or_else(|| "conversation not found".to_owned())?;
        let mut change_id = RootAttachmentChangeId::new();
        while change_id.as_uuid() == &receipt.call_id.0
            || *change_id.as_uuid() == receipt.registration_operation_id.as_uuid()
        {
            change_id = RootAttachmentChangeId::new();
        }
        receipt.product_sync = Some(ProductRootAttachmentSync {
            change_id,
            root_id,
            display_name: root.display_name.clone(),
            expected_attachment_revision: chat.attachment_revision,
            created_at: chrono::Utc::now(),
            cleanup_operation_id: distinct_operation_id(receipt, change_id),
            cleanup_phase: CleanupPhase::NotStarted,
            attachment_phase: AttachmentPhase::Prepared,
        });
        // This exact id, CAS fence, and timestamp must survive an ambiguous
        // begin response or process exit. Save them before product dispatch.
        state
            .receipts
            .save(receipt)
            .map_err(private_receipt_error)?;
    }
    drive_product_attachment(state, receipt, current_context, root).await
}

pub(super) async fn resume_product_attachment(
    state: &HostAccess,
    receipt: &mut FolderAccessReceipt,
    context: AuthoritativeContext,
) -> Result<StoredResolution, String> {
    let sync = receipt
        .product_sync
        .clone()
        .ok_or_else(|| "folder attachment recovery metadata is missing".to_owned())?;
    let root_id = RootId::from_uuid(*sync.root_id.as_uuid())
        .map_err(|_| "invalid recovered folder root identity".to_owned())?;
    drive_product_attachment(
        state,
        receipt,
        context,
        RootSummary {
            root_id,
            display_name: sync.display_name,
        },
    )
    .await
}

async fn drive_product_attachment(
    state: &HostAccess,
    receipt: &mut FolderAccessReceipt,
    current_context: AuthoritativeContext,
    root: RootSummary,
) -> Result<StoredResolution, String> {
    let root_id = HostRootId::from_uuid(root.root_id.as_uuid())
        .map_err(|_| "host broker returned an invalid registered root".to_owned())?;
    let sync = receipt
        .product_sync
        .clone()
        .ok_or_else(|| "folder attachment recovery metadata is missing".to_owned())?;
    if sync.root_id != root_id || sync.display_name != root.display_name {
        return Err("registered root changed during folder attachment recovery".to_owned());
    }

    let client = control_plane(state)?;
    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    let begun = match client
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
        Err(error) if permanent_product_begin_failure(&error) => {
            cleanup_rejected_registration(
                state,
                receipt,
                &sync,
                current_context,
                error.is_kind("root_attachment_identity_conflict"),
            )
            .await?;
            return Ok(failed_resolution(
                "folder_access_product_sync_rejected",
                "The selected folder could not be synchronized with this conversation.",
                None,
            ));
        }
        Err(error) => return Err(product_begin_error(error, sync.change_id)),
    };
    validate_product_change(&begun.change, receipt, &sync, current_context)?;

    match begun.change.phase {
        RootAttachmentChangePhase::Completed => {
            verify_completed_product_attachment(state, &begun.change).await?;
            return connected_resolution(root);
        }
        RootAttachmentChangePhase::Failed => {
            verify_failed_product_attachment(state, &begun.change).await?;
            return Ok(product_attachment_failed_resolution(&begun.change));
        }
        RootAttachmentChangePhase::AwaitingBroker => {}
    }

    let terminal = reconcile_broker_attachment(state, receipt, &sync, current_context).await?;
    let finished = client
        .finish_root_attachment_change(sync.change_id, &terminal)
        .await
        .map_err(|error| product_finish_error(error, sync.change_id))?;
    validate_product_change(&finished.change, receipt, &sync, current_context)?;
    match finished.change.phase {
        RootAttachmentChangePhase::Completed => {
            verify_completed_product_attachment(state, &finished.change).await?;
            connected_resolution(root)
        }
        RootAttachmentChangePhase::Failed => {
            verify_failed_product_attachment(state, &finished.change).await?;
            Ok(product_attachment_failed_resolution(&finished.change))
        }
        RootAttachmentChangePhase::AwaitingBroker => Err(
            "product folder attachment remained pending after its terminal broker receipt"
                .to_owned(),
        ),
    }
}

fn distinct_operation_id(
    receipt: &FolderAccessReceipt,
    change_id: RootAttachmentChangeId,
) -> OperationId {
    loop {
        let id = OperationId::new();
        if id.as_uuid() != receipt.call_id.0
            && id.as_uuid() != receipt.registration_operation_id.as_uuid()
            && id.as_uuid() != *change_id.as_uuid()
        {
            return id;
        }
    }
}

async fn cleanup_rejected_registration(
    state: &HostAccess,
    receipt: &mut FolderAccessReceipt,
    sync: &ProductRootAttachmentSync,
    context: AuthoritativeContext,
    allow_identity_collision: bool,
) -> Result<(), String> {
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(receipt.chat_id)
        .await
        .map_err(|_| "could not inspect the rejected folder attachment".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    if chat
        .root_attachments
        .iter()
        .any(|attachment| attachment.root_id == sync.root_id)
    {
        // A concurrent exact product attach already converged this root. Never
        // tear down authority that current product state still names.
        return Ok(());
    }
    if !allow_identity_collision
        && store
            .get_root_attachment_change(sync.change_id)
            .await
            .map_err(|_| "could not inspect rejected folder attachment work".to_owned())?
            .is_some()
    {
        return Err("rejected folder attachment identity still owns product work".to_owned());
    }

    if cleanup_receipt(state, receipt, sync, context)
        .await?
        .is_none()
    {
        if let Some(stored) = receipt.product_sync.as_mut() {
            stored.cleanup_phase = CleanupPhase::DispatchAttempted;
        }
        state
            .receipts
            .save(receipt)
            .map_err(private_receipt_error)?;

        let broker_root_id = RootId::from_uuid(*sync.root_id.as_uuid())
            .map_err(|_| "invalid cleanup folder root identity".to_owned())?;
        let deadline = tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
        let first = state
            .broker
            .control_without_retry(
                ControlRequest::DetachRoot(RootAttachmentMutationRequest {
                    operation_id: sync.cleanup_operation_id,
                    subject: context.subject,
                    conversation_id: context.chat_id,
                    root_id: broker_root_id,
                    consent_method: None,
                }),
                deadline,
            )
            .await;
        match first {
            Ok(ControlResult::DetachRoot(_)) | Err(_) => {}
            Ok(_) => return Err("host broker returned an unexpected cleanup response".to_owned()),
        }
        if cleanup_receipt(state, receipt, sync, context)
            .await?
            .is_none()
        {
            return Err("folder cleanup has no durable broker outcome yet".to_owned());
        }
    }

    if let Some(stored) = receipt.product_sync.as_mut() {
        stored.cleanup_phase = CleanupPhase::Completed;
    }
    state
        .receipts
        .save(receipt)
        .map_err(private_receipt_error)?;
    let chat = store
        .get_chat(receipt.chat_id)
        .await
        .map_err(|_| "could not verify rejected folder cleanup".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    if chat
        .root_attachments
        .iter()
        .any(|attachment| attachment.root_id == sync.root_id)
        || (!allow_identity_collision
            && store
                .get_root_attachment_change(sync.change_id)
                .await
                .map_err(|_| "could not verify rejected folder cleanup".to_owned())?
                .is_some())
    {
        return Err("rejected folder cleanup contradicts product state".to_owned());
    }
    Ok(())
}

async fn cleanup_receipt(
    state: &HostAccess,
    receipt: &FolderAccessReceipt,
    sync: &ProductRootAttachmentSync,
    context: AuthoritativeContext,
) -> Result<Option<()>, String> {
    let broker_root_id = RootId::from_uuid(*sync.root_id.as_uuid())
        .map_err(|_| "invalid cleanup folder root identity".to_owned())?;
    let result = state
        .broker
        .control(ControlRequest::LookupRootAttachmentReceipt(
            LookupRootAttachmentReceiptRequest {
                operation_id: sync.cleanup_operation_id,
                subject: context.subject,
                conversation_id: context.chat_id,
                root_id: broker_root_id,
                mutation: RootAttachmentMutationKind::Detach,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::LookupRootAttachmentReceipt(result) = result else {
        return Err("host broker returned an unexpected cleanup receipt".to_owned());
    };
    if result.operation_id != sync.cleanup_operation_id {
        return Err("host broker returned a mismatched cleanup receipt".to_owned());
    }
    match cleanup_receipt_outcome(result.receipt, broker_root_id)? {
        CleanupReceiptOutcome::Unknown => Ok(None),
        CleanupReceiptOutcome::Detached => Ok(Some(())),
        CleanupReceiptOutcome::FailedNeedsRegistrationCheck => {
            confirm_registration_disconnected(state, receipt, sync, context).await?;
            Ok(Some(()))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupReceiptOutcome {
    Unknown,
    Detached,
    FailedNeedsRegistrationCheck,
}

fn cleanup_receipt_outcome(
    receipt: RootAttachmentMutationReceipt,
    broker_root_id: RootId,
) -> Result<CleanupReceiptOutcome, String> {
    match receipt {
        RootAttachmentMutationReceipt::Unknown => Ok(CleanupReceiptOutcome::Unknown),
        RootAttachmentMutationReceipt::Completed {
            result,
            currently_attached,
        } if result.root_id == broker_root_id
            && result.mutation == RootAttachmentMutationKind::Detach
            && !currently_attached =>
        {
            Ok(CleanupReceiptOutcome::Detached)
        }
        RootAttachmentMutationReceipt::Completed { .. } => {
            Err("host broker cleanup receipt contradicts live attachment state".to_owned())
        }
        RootAttachmentMutationReceipt::Failed { .. } => {
            Ok(CleanupReceiptOutcome::FailedNeedsRegistrationCheck)
        }
        _ => Err("host broker returned an unsupported cleanup receipt".to_owned()),
    }
}

async fn confirm_registration_disconnected(
    state: &HostAccess,
    receipt: &FolderAccessReceipt,
    sync: &ProductRootAttachmentSync,
    context: AuthoritativeContext,
) -> Result<(), String> {
    let result = state
        .broker
        .control(ControlRequest::LookupRegisterRootReceipt(
            LookupRegisterRootReceiptRequest {
                operation_id: receipt.registration_operation_id,
                subject: context.subject,
                conversation_id: context.chat_id,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::LookupRegisterRootReceipt(result) = result else {
        return Err("host broker returned an unexpected registration cleanup receipt".to_owned());
    };
    if result.operation_id != receipt.registration_operation_id {
        return Err("host broker returned a mismatched registration cleanup receipt".to_owned());
    }
    registration_disconnected_outcome(result.receipt, sync)
}

fn registration_disconnected_outcome(
    receipt: RegisterRootReceipt,
    sync: &ProductRootAttachmentSync,
) -> Result<(), String> {
    match receipt {
        RegisterRootReceipt::Disconnected { root }
            if root.root_id.as_uuid() == *sync.root_id.as_uuid()
                && root.display_name == sync.display_name =>
        {
            Ok(())
        }
        RegisterRootReceipt::Disconnected { .. } => {
            Err("registration cleanup receipt names a different root".to_owned())
        }
        RegisterRootReceipt::Completed { .. }
        | RegisterRootReceipt::Pending
        | RegisterRootReceipt::Unknown
        | RegisterRootReceipt::Failed { .. } => {
            Err("rejected folder cleanup is not authoritatively detached".to_owned())
        }
        _ => Err("host broker returned an unsupported registration cleanup receipt".to_owned()),
    }
}

async fn reconcile_broker_attachment(
    state: &HostAccess,
    receipt: &mut FolderAccessReceipt,
    sync: &ProductRootAttachmentSync,
    context: AuthoritativeContext,
) -> Result<RootAttachmentChangeTerminal, String> {
    if let Some(terminal) = lookup_broker_attachment(state, sync, context).await? {
        return Ok(terminal);
    }

    let client = control_plane(state)?;
    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    if let Some(stored) = receipt.product_sync.as_mut() {
        stored.attachment_phase = AttachmentPhase::DispatchAttempted;
    }
    state
        .receipts
        .save(receipt)
        .map_err(private_receipt_error)?;

    let operation_id = attachment_operation_id(sync)?;
    let broker_root_id = RootId::from_uuid(*sync.root_id.as_uuid())
        .map_err(|_| "invalid product folder root identity".to_owned())?;
    let dispatch_deadline = tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
    let first = state
        .broker
        .control_without_retry(
            ControlRequest::AttachRoot(RootAttachmentMutationRequest {
                operation_id,
                subject: context.subject,
                conversation_id: context.chat_id,
                root_id: broker_root_id,
                consent_method: Some(ConsentMethod::PermissionDialog),
            }),
            dispatch_deadline,
        )
        .await;
    match first {
        Ok(ControlResult::AttachRoot(_)) | Err(_) => {}
        Ok(_) => return Err("host broker returned an unexpected attachment response".to_owned()),
    }

    lookup_broker_attachment(state, sync, context)
        .await?
        .ok_or_else(|| {
            "folder attachment has no durable broker outcome yet; recovery will retry".to_owned()
        })
}

async fn lookup_broker_attachment(
    state: &HostAccess,
    sync: &ProductRootAttachmentSync,
    context: AuthoritativeContext,
) -> Result<Option<RootAttachmentChangeTerminal>, String> {
    let operation_id = attachment_operation_id(sync)?;
    let broker_root_id = RootId::from_uuid(*sync.root_id.as_uuid())
        .map_err(|_| "invalid product folder root identity".to_owned())?;
    let result = state
        .broker
        .control(ControlRequest::LookupRootAttachmentReceipt(
            LookupRootAttachmentReceiptRequest {
                operation_id,
                subject: context.subject,
                conversation_id: context.chat_id,
                root_id: broker_root_id,
                mutation: RootAttachmentMutationKind::Attach,
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
    attachment_receipt_terminal(result.receipt, broker_root_id)
}

fn attachment_receipt_terminal(
    receipt: RootAttachmentMutationReceipt,
    broker_root_id: RootId,
) -> Result<Option<RootAttachmentChangeTerminal>, String> {
    match receipt {
        RootAttachmentMutationReceipt::Unknown => Ok(None),
        RootAttachmentMutationReceipt::Completed {
            result,
            currently_attached,
        } => {
            if result.root_id != broker_root_id
                || result.mutation != RootAttachmentMutationKind::Attach
            {
                return Err("host broker and product folder attachment state disagree".to_owned());
            }
            if currently_attached {
                Ok(Some(RootAttachmentChangeTerminal::Completed {
                    broker_changed: result.changed,
                    broker_currently_attached: true,
                }))
            } else {
                Ok(Some(RootAttachmentChangeTerminal::Failed {
                    broker_changed: Some(result.changed),
                    broker_currently_attached: Some(false),
                    failure: RootAttachmentChangeFailure {
                        code: "broker_attachment_disconnected".to_owned(),
                        message:
                            "The selected folder was disconnected before synchronization completed."
                                .to_owned(),
                        retryable: false,
                    },
                }))
            }
        }
        RootAttachmentMutationReceipt::Failed { .. } => {
            Ok(Some(RootAttachmentChangeTerminal::Failed {
                broker_changed: None,
                broker_currently_attached: None,
                failure: RootAttachmentChangeFailure {
                    code: "broker_attachment_failed".to_owned(),
                    message: "The host broker could not complete this folder attachment."
                        .to_owned(),
                    retryable: false,
                },
            }))
        }
        _ => Err("host broker returned an unsupported attachment receipt".to_owned()),
    }
}

pub(super) fn attachment_operation_id(
    sync: &ProductRootAttachmentSync,
) -> Result<OperationId, String> {
    OperationId::from_uuid(*sync.change_id.as_uuid())
        .map_err(|_| "invalid product folder attachment identity".to_owned())
}

pub(super) fn validate_product_change(
    change: &RootAttachmentChangeView,
    receipt: &FolderAccessReceipt,
    sync: &ProductRootAttachmentSync,
    context: AuthoritativeContext,
) -> Result<(), String> {
    let subject_kind = match context.subject.kind() {
        SubjectKind::Project => RootAttachmentSubjectKind::Project,
        SubjectKind::Conversation => RootAttachmentSubjectKind::Conversation,
    };
    if change.id != sync.change_id
        || change.chat_id != receipt.chat_id
        || change.root_id != sync.root_id
        || change.action != RootAttachmentChangeAction::Attach
        || change.subject_kind != subject_kind
        || change.subject_id != context.subject.id()
        || change.expected_revision != sync.expected_attachment_revision
        || change.before_revision != sync.expected_attachment_revision
        || change.intent_revision < change.before_revision
        || change.intent_revision > change.before_revision.saturating_add(1)
    {
        return Err(
            "local control plane returned a mismatched product folder attachment".to_owned(),
        );
    }
    if change.phase == RootAttachmentChangePhase::Completed
        && (change.result_revision.is_none()
            || change.broker_currently_attached != Some(true)
            || change.failure.is_some())
    {
        return Err(
            "completed product folder attachment has contradictory terminal state".to_owned(),
        );
    }
    Ok(())
}

async fn verify_completed_product_attachment(
    state: &HostAccess,
    change: &RootAttachmentChangeView,
) -> Result<(), String> {
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(change.chat_id)
        .await
        .map_err(|_| "could not verify the product folder attachment".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    if Some(chat.attachment_revision) != change.result_revision
        || !chat
            .root_attachments
            .iter()
            .any(|attachment| attachment.root_id == change.root_id)
    {
        return Err("host broker and product folder attachment state disagree".to_owned());
    }
    Ok(())
}

async fn verify_failed_product_attachment(
    state: &HostAccess,
    change: &RootAttachmentChangeView,
) -> Result<(), String> {
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(change.chat_id)
        .await
        .map_err(|_| "could not verify the failed product folder attachment".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    let newly_projected = change.intent_revision > change.before_revision;
    if Some(chat.attachment_revision) != change.result_revision
        || (newly_projected
            && chat
                .root_attachments
                .iter()
                .any(|attachment| attachment.root_id == change.root_id))
    {
        return Err("failed product folder attachment did not roll back safely".to_owned());
    }
    Ok(())
}

fn product_attachment_failed_resolution(change: &RootAttachmentChangeView) -> StoredResolution {
    let detail = change
        .failure
        .as_ref()
        .map(|failure| failure.message.clone());
    failed_resolution(
        "folder_access_product_sync_failed",
        "The selected folder could not be synchronized with this conversation.",
        detail,
    )
}

fn product_begin_error(error: ControlPlaneError, change_id: RootAttachmentChangeId) -> String {
    if error.is_kind("root_attachment_revision_conflict") {
        return format!(
            "folder attachment {change_id} was fenced by a conversation revision change"
        );
    }
    if error.is_conflict() {
        return format!("folder attachment {change_id} could not begin safely: {error}");
    }
    control_plane_error(error)
}

fn permanent_product_begin_failure(error: &ControlPlaneError) -> bool {
    [
        "root_attachment_revision_conflict",
        "root_attachment_identity_conflict",
        "root_attachment_capacity_exceeded",
        "root_attachment_revision_exhausted",
    ]
    .iter()
    .any(|kind| error.is_kind(kind))
}

fn product_finish_error(error: ControlPlaneError, change_id: RootAttachmentChangeId) -> String {
    if error.is_kind("root_attachment_broker_state_mismatch") || error.is_conflict() {
        return format!("folder attachment {change_id} failed closed: {error}");
    }
    control_plane_error(error)
}

#[cfg(test)]
mod tests {
    use openwave_host_broker::{ErrorCode, ErrorResponse};

    use super::*;

    #[test]
    fn stable_begin_conflicts_terminalize_but_chat_busy_defers() {
        let conflict = |kind: &str| ControlPlaneError::Http {
            status: 409,
            kind: kind.to_owned(),
            message: "conflict".to_owned(),
        };
        assert!(permanent_product_begin_failure(&conflict(
            "root_attachment_revision_conflict"
        )));
        assert!(permanent_product_begin_failure(&conflict(
            "root_attachment_identity_conflict"
        )));
        assert!(!permanent_product_begin_failure(&conflict(
            "root_attachment_chat_busy"
        )));
    }

    #[test]
    fn durable_broker_failure_becomes_a_bounded_product_terminal() {
        let root_id = RootId::new();
        let terminal = attachment_receipt_terminal(
            RootAttachmentMutationReceipt::Failed {
                error: ErrorResponse {
                    code: ErrorCode::HostIo,
                    message: "private broker detail".to_owned(),
                    retryable: true,
                },
            },
            root_id,
        )
        .unwrap()
        .unwrap();
        let RootAttachmentChangeTerminal::Failed { failure, .. } = terminal else {
            panic!("expected failed product terminal")
        };
        assert_eq!(failure.code, "broker_attachment_failed");
        assert!(!failure.message.contains("private broker detail"));
        assert!(!failure.retryable);
    }

    #[test]
    fn historical_attach_revocation_rolls_back_and_cleanup_requires_detached_state() {
        let root_id = RootId::new();
        let terminal = attachment_receipt_terminal(
            RootAttachmentMutationReceipt::Completed {
                result: openwave_host_broker::RootAttachmentMutationResult {
                    root_id,
                    mutation: RootAttachmentMutationKind::Attach,
                    changed: true,
                },
                currently_attached: false,
            },
            root_id,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            terminal,
            RootAttachmentChangeTerminal::Failed {
                broker_changed: Some(true),
                broker_currently_attached: Some(false),
                ..
            }
        ));

        assert_eq!(
            cleanup_receipt_outcome(
                RootAttachmentMutationReceipt::Completed {
                    result: openwave_host_broker::RootAttachmentMutationResult {
                        root_id,
                        mutation: RootAttachmentMutationKind::Detach,
                        changed: true,
                    },
                    currently_attached: false,
                },
                root_id,
            )
            .unwrap(),
            CleanupReceiptOutcome::Detached
        );
    }

    #[test]
    fn revoke_before_cleanup_confirms_detachment_through_registration_receipt() {
        let root_id = RootId::new();
        assert_eq!(
            cleanup_receipt_outcome(
                RootAttachmentMutationReceipt::Failed {
                    error: ErrorResponse {
                        code: ErrorCode::InvalidRoot,
                        message: "unknown root".to_owned(),
                        retryable: false,
                    },
                },
                root_id,
            )
            .unwrap(),
            CleanupReceiptOutcome::FailedNeedsRegistrationCheck
        );
        let sync = ProductRootAttachmentSync {
            change_id: RootAttachmentChangeId::new(),
            root_id: HostRootId::from_uuid(root_id.as_uuid()).unwrap(),
            display_name: "Documents".to_owned(),
            expected_attachment_revision: 0,
            created_at: chrono::Utc::now(),
            cleanup_operation_id: OperationId::new(),
            cleanup_phase: CleanupPhase::DispatchAttempted,
            attachment_phase: AttachmentPhase::Prepared,
        };
        assert!(registration_disconnected_outcome(
            RegisterRootReceipt::Disconnected {
                root: RootSummary {
                    root_id,
                    display_name: "Documents".to_owned(),
                },
            },
            &sync,
        )
        .is_ok());
        assert!(registration_disconnected_outcome(
            RegisterRootReceipt::Completed {
                root: RootSummary {
                    root_id,
                    display_name: "Documents".to_owned(),
                },
            },
            &sync,
        )
        .is_err());
    }
}
