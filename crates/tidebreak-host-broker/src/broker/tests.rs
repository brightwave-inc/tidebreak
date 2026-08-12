use super::*;
use crate::{AppFolderPathRequest, WriteApproval};

#[derive(Default)]
struct CollectingAudit {
    events: Mutex<Vec<AuditEvent>>,
}

impl AuditSink for CollectingAudit {
    fn record(&self, event: &AuditEvent) -> Result<(), AuditError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

/// An audit sink whose storage can be taken away mid-session.
#[derive(Default)]
struct BreakableAudit {
    broken: AtomicBool,
}

impl AuditSink for BreakableAudit {
    fn record(&self, _event: &AuditEvent) -> Result<(), AuditError> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(AuditError::Io(io::Error::other("injected audit failure")));
        }
        Ok(())
    }
}

fn test_policy(temp: &tempfile::TempDir) -> RootPolicy {
    RootPolicy::for_test(
        temp.path().join("home"),
        vec![temp.path().join("sensitive")],
        vec![temp.path().to_path_buf()],
        Vec::new(),
    )
}

fn setup() -> (tempfile::TempDir, Broker, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join("Documents");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
    std::fs::create_dir(root.join("reports")).unwrap();
    let policy = test_policy(&temp);
    (temp, Broker::new(policy), root)
}

#[test]
fn a_no_exec_host_does_not_report_command_reach_for_a_fresh_folder() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("home/Documents");
    std::fs::create_dir_all(&root).unwrap();
    let broker = Broker::new_with_execute_commands(test_policy(&temp), false);
    let conversation = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        root,
        OperationId::new(),
    );

    assert_eq!(
        operate(
            &broker.operator(),
            ExecutionContext::standalone(conversation).unwrap(),
            OperationRequest::ListRoots,
        )
        .unwrap(),
        OperationResult::ListRoots {
            roots: vec![RootAccess {
                root_id: registered.root.root_id,
                display_name: registered.root.display_name,
                capabilities: vec![Capability::ReadFiles, Capability::WriteFiles],
            }],
        }
    );
}

fn durable_setup() -> (tempfile::TempDir, Broker, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("home/Documents");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
    let state_dir = temp.path().join("app-data/host-broker");
    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    (temp, broker, root, state_dir)
}

fn audited_setup() -> (tempfile::TempDir, Broker, PathBuf, Arc<CollectingAudit>) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("home/Documents");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
    let audit = Arc::new(CollectingAudit::default());
    let broker = Broker::with_audit_sink(test_policy(&temp), audit.clone());
    (temp, broker, root, audit)
}

fn register(
    controller: &Controller,
    subject: GrantSubject,
    conversation_id: Uuid,
    path: PathBuf,
    operation_id: OperationId,
) -> RegisterRootResult {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id,
            subject,
            conversation_id,
            path,
            consent_method: ConsentMethod::FolderPicker,
        }),
    }))
    .unwrap();
    let ControlResult::RegisterRoot(result) = result else {
        panic!("unexpected control result")
    };
    result
}

/// The listing shape a folder gets from a plain registration, which grants
/// reading, writing, and exec reach together.
fn picker_access(root: RootSummary) -> RootAccess {
    RootAccess {
        root_id: root.root_id,
        display_name: root.display_name,
        capabilities: vec![
            Capability::ReadFiles,
            Capability::WriteFiles,
            Capability::ExecuteCommands,
        ],
    }
}

fn mutate_attachment(
    controller: &Controller,
    operation_id: OperationId,
    subject: GrantSubject,
    conversation_id: Uuid,
    root_id: RootId,
    mutation: RootAttachmentMutationKind,
) -> Result<RootAttachmentMutationResult, ErrorResponse> {
    let request = RootAttachmentMutationRequest {
        operation_id,
        subject,
        conversation_id,
        root_id,
        consent_method: match mutation {
            RootAttachmentMutationKind::Attach => Some(ConsentMethod::PermissionDialog),
            RootAttachmentMutationKind::Detach => None,
        },
    };
    let control = match mutation {
        RootAttachmentMutationKind::Attach => ControlRequest::AttachRoot(request),
        RootAttachmentMutationKind::Detach => ControlRequest::DetachRoot(request),
    };
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: control,
    }))?;
    match result {
        ControlResult::AttachRoot(result) | ControlResult::DetachRoot(result) => Ok(result),
        _ => panic!("unexpected control result"),
    }
}

fn lookup_attachment_receipt(
    controller: &Controller,
    request: LookupRootAttachmentReceiptRequest,
) -> Result<RootAttachmentMutationReceipt, ErrorResponse> {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::LookupRootAttachmentReceipt(request),
    }))?;
    let ControlResult::LookupRootAttachmentReceipt(result) = result else {
        panic!("unexpected control result")
    };
    Ok(result.receipt)
}

fn lookup_register_receipt(
    controller: &Controller,
    operation_id: OperationId,
    subject: GrantSubject,
    conversation_id: Uuid,
) -> Result<LookupRegisterRootReceiptResult, ErrorResponse> {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::LookupRegisterRootReceipt(LookupRegisterRootReceiptRequest {
            operation_id,
            subject,
            conversation_id,
        }),
    }))?;
    let ControlResult::LookupRegisterRootReceipt(result) = result else {
        panic!("unexpected control result")
    };
    Ok(result)
}

fn operate(
    operator: &Operator,
    context: ExecutionContext,
    request: OperationRequest,
) -> Result<OperationResult, ErrorResponse> {
    unwrap_response(operator.handle(OperationEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        context,
        request,
    }))
}

fn write_request(
    operation_id: OperationId,
    root_id: RootId,
    path: &str,
    mode: WriteFileMode,
    approval: Option<WriteApproval>,
    content: &[u8],
) -> OperationRequest {
    OperationRequest::WriteFile(WriteFileRequest {
        operation_id,
        root_id,
        path: RelativePath::parse(path).unwrap(),
        mode,
        approval,
        content_base64: BASE64.encode(content),
        bytes: content.len(),
        sha256: Sha256::digest(content).into(),
    })
}

fn list_approved(controller: &Controller) -> Vec<RootSummary> {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::ListApprovedRoots,
    }))
    .unwrap();
    let ControlResult::ListApprovedRoots { roots } = result else {
        panic!("unexpected control result")
    };
    roots
}

fn list_unavailable(controller: &Controller) -> Vec<UnavailableRootSummary> {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::ListUnavailableRoots,
    }))
    .unwrap();
    let ControlResult::ListUnavailableRoots { roots } = result else {
        panic!("unexpected control result")
    };
    roots
}

fn resolve_exec(
    controller: &Controller,
    context: ExecutionContext,
    root_ids: Vec<RootId>,
) -> Result<Vec<ResolvedExecRoot>, ErrorResponse> {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::ResolveExecRoots(ResolveExecRootsRequest { context, root_ids }),
    }))?;
    let ControlResult::ResolveExecRoots { roots } = result else {
        panic!("unexpected control result")
    };
    Ok(roots)
}

fn revoke(
    controller: &Controller,
    operation_id: OperationId,
    subject: GrantSubject,
    root_id: RootId,
) -> RevokeRootResult {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id,
            subject,
            root_id,
        }),
    }))
    .unwrap();
    let ControlResult::RevokeRoot(result) = result else {
        panic!("unexpected control result")
    };
    result
}

fn install_legacy_alias(
    broker: &Broker,
    source_root_id: RootId,
    alias_root_id: RootId,
    subject: GrantSubject,
    conversation_id: Uuid,
    selected_path: PathBuf,
    operation_id: OperationId,
) {
    let consent = ConsentRecord::new(ConsentMethod::FolderPicker, Utc::now());
    let mut state = broker.shared.state.lock().unwrap();
    let source = state.roots.get(&source_root_id).unwrap().clone();
    let display_name = source.display_name.clone();
    assert!(state
        .roots
        .insert(
            alias_root_id,
            RegisteredRoot {
                owner: subject,
                display_name: display_name.clone(),
                root: source.root,
            },
        )
        .is_none());
    state.grants.push(
        Grant::from_consent(
            GrantId::new(),
            subject,
            Capability::ListRoots,
            Scope::Subject,
            consent.clone(),
        )
        .unwrap(),
    );
    state.grants.push(
        Grant::from_consent(
            GrantId::new(),
            subject,
            Capability::ReadFiles,
            Scope::Root {
                root_id: alias_root_id,
            },
            consent,
        )
        .unwrap(),
    );
    state
        .attachments
        .push(RootAttachment::new(conversation_id, alias_root_id).unwrap());
    assert!(state
        .mutations
        .insert(
            operation_id,
            MutationRecord::Register {
                request: RegisterFingerprint {
                    subject,
                    conversation_id,
                    path: selected_path,
                    consent_method: ConsentMethod::FolderPicker,
                },
                outcome: MutationOutcome::Complete(Ok(RegisterRootResult {
                    root: RootSummary {
                        root_id: alias_root_id,
                        display_name,
                    },
                })),
            },
        )
        .is_none());
    if let Some(state_file) = broker.shared.state_file.as_ref() {
        state_file.save(&state).unwrap();
    }
}

fn unwrap_response<T>(envelope: ResponseEnvelope<T>) -> Result<T, ErrorResponse> {
    match envelope.response {
        Response::Ok(result) => Ok(result),
        Response::Error(error) => Err(error),
    }
}

fn grant_statements(controller: &Controller) -> Vec<GrantStatementSummary> {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::ListGrantStatements,
    }))
    .unwrap();
    let ControlResult::ListGrantStatements { grants } = result else {
        panic!("unexpected control result")
    };
    grants
}

fn revoke_grant(controller: &Controller, subject: GrantSubject, grant_id: GrantId) -> bool {
    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeGrant(RevokeGrantRequest { subject, grant_id }),
    }))
    .unwrap();
    let ControlResult::RevokeGrant(result) = result else {
        panic!("unexpected control result")
    };
    result.revoked
}

/// The statement-level boundary derivation: revoking one grant removes
/// exactly that authority — plus what depends on it — and enforcement follows
/// because `authorize()` reads the same rows the statements project.
#[test]
fn revoking_read_takes_exec_with_it_and_revoking_exec_leaves_read() {
    let (_temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let root_id = register(&controller, subject, conversation, path, OperationId::new())
        .root
        .root_id;
    let context = ExecutionContext::standalone(conversation).unwrap();
    let find = |capability: Capability| {
        grant_statements(&controller)
            .into_iter()
            .find(|grant| grant.capability == capability)
    };

    // Revoking exec alone leaves the folder readable.
    let exec_id = find(Capability::ExecuteCommands).unwrap().grant_id;
    // Another subject's revocation touches nothing and cannot probe.
    assert!(!revoke_grant(
        &controller,
        GrantSubject::conversation(Uuid::new_v4()).unwrap(),
        exec_id,
    ));
    assert!(revoke_grant(&controller, subject, exec_id));
    assert!(!revoke_grant(&controller, subject, exec_id));
    assert!(find(Capability::ExecuteCommands).is_none());
    assert!(operate(
        &broker.operator(),
        context,
        OperationRequest::ListDirectory(PathRequest {
            root_id,
            path: RelativePath::parse("reports").unwrap(),
        }),
    )
    .is_ok());

    // Revoking read takes nothing else — except that nothing depends on it
    // anymore — and enforcement denies the next read outright.
    let read_id = find(Capability::ReadFiles).unwrap().grant_id;
    assert!(revoke_grant(&controller, subject, read_id));
    assert!(find(Capability::ReadFiles).is_none());
    assert!(find(Capability::WriteFiles).is_some());
    assert_eq!(
        operate(
            &broker.operator(),
            context,
            OperationRequest::ListDirectory(PathRequest {
                root_id,
                path: RelativePath::parse("reports").unwrap(),
            }),
        )
        .unwrap_err()
        .code,
        ErrorCode::Denied
    );
}

/// Read carries exec with it when both stand: exec reach is only ever
/// additional on top of read.
#[test]
fn revoking_read_cascades_to_exec() {
    let (_temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    register(&controller, subject, conversation, path, OperationId::new());
    let read_id = grant_statements(&controller)
        .into_iter()
        .find(|grant| grant.capability == Capability::ReadFiles)
        .unwrap()
        .grant_id;

    assert!(revoke_grant(&controller, subject, read_id));
    let remaining = grant_statements(&controller)
        .into_iter()
        .map(|grant| grant.capability)
        .collect::<Vec<_>>();
    assert!(!remaining.contains(&Capability::ReadFiles));
    assert!(!remaining.contains(&Capability::ExecuteCommands));
    assert!(remaining.contains(&Capability::WriteFiles));
    assert!(remaining.contains(&Capability::ListRoots));
}

/// The widening boundary: an attached read-only folder can regain write
/// through a fresh permission-dialog consent — and only through one. The
/// request cannot reach an unattached conversation, an unknown root, or claim
/// a consent interaction the picker methods describe, and a retry after the
/// grant stands is a no-op rather than a duplicate statement.
#[test]
fn granting_write_to_an_attached_root_requires_fresh_dialog_consent() {
    let (_temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let root_id = register(&controller, subject, conversation, path, OperationId::new())
        .root
        .root_id;
    let context = ExecutionContext::standalone(conversation).unwrap();
    let write_grant = |grants: Vec<GrantStatementSummary>| {
        grants
            .into_iter()
            .find(|grant| grant.capability == Capability::WriteFiles)
    };
    let widen = |subject, conversation_id, root_id, consent_method| {
        unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::GrantRootCapability(GrantRootCapabilityRequest {
                subject,
                conversation_id,
                root_id,
                capability: Capability::WriteFiles,
                consent_method,
            }),
        }))
    };

    // The pre-v3 shape: write consent was never recorded for this root.
    let original = write_grant(grant_statements(&controller)).unwrap();
    assert!(revoke_grant(&controller, subject, original.grant_id));
    let listed_write = || {
        let OperationResult::ListRoots { roots } =
            operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap()
        else {
            panic!("unexpected operation result")
        };
        roots[0].capabilities.contains(&Capability::WriteFiles)
    };
    assert!(!listed_write());

    // Wrong consent vocabulary, wrong conversation, wrong root: all refused.
    assert!(widen(subject, conversation, root_id, ConsentMethod::FolderPicker).is_err());
    let stranger = Uuid::new_v4();
    assert_eq!(
        widen(
            GrantSubject::conversation(stranger).unwrap(),
            stranger,
            root_id,
            ConsentMethod::PermissionDialog,
        )
        .unwrap_err()
        .code,
        ErrorCode::Denied
    );
    assert!(widen(
        subject,
        conversation,
        RootId::new(),
        ConsentMethod::PermissionDialog
    )
    .is_err());
    assert!(!listed_write());

    // The real widening mints one statement with dialog provenance, and the
    // same `authorize()` rows the listing reads now allow writing.
    assert_eq!(
        widen(
            subject,
            conversation,
            root_id,
            ConsentMethod::PermissionDialog
        )
        .unwrap(),
        ControlResult::GrantRootCapability(GrantRootCapabilityResult { granted: true })
    );
    assert!(listed_write());
    let minted = write_grant(grant_statements(&controller)).unwrap();
    assert_eq!(minted.consent_method, ConsentMethod::PermissionDialog);

    // A retry observes the standing grant instead of minting a second row.
    assert_eq!(
        widen(
            subject,
            conversation,
            root_id,
            ConsentMethod::PermissionDialog
        )
        .unwrap(),
        ControlResult::GrantRootCapability(GrantRootCapabilityResult { granted: false })
    );
    assert_eq!(
        grant_statements(&controller)
            .into_iter()
            .filter(|grant| grant.capability == Capability::WriteFiles)
            .count(),
        1
    );
}

/// Read is the capability whose revocation had no way back. It takes exec
/// with it and it hides the folder from the listing the panels read, so once
/// it was gone there was nothing left on screen to widen. The permission-
/// dialog widening covers it like any other capability: it restores exactly
/// read, leaves the exec reach that was cascaded away withdrawn, and a retry
/// mints nothing.
#[test]
fn granting_read_back_restores_an_attached_folder_without_its_exec_reach() {
    let (_temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let root_id = register(&controller, subject, conversation, path, OperationId::new())
        .root
        .root_id;
    let context = ExecutionContext::standalone(conversation).unwrap();
    let listed = || {
        let OperationResult::ListRoots { roots } =
            operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap()
        else {
            panic!("unexpected operation result")
        };
        roots
    };
    let grant_read = || {
        unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::GrantRootCapability(GrantRootCapabilityRequest {
                subject,
                conversation_id: conversation,
                root_id,
                capability: Capability::ReadFiles,
                consent_method: ConsentMethod::PermissionDialog,
            }),
        }))
    };

    let read_id = grant_statements(&controller)
        .into_iter()
        .find(|grant| grant.capability == Capability::ReadFiles)
        .unwrap()
        .grant_id;
    assert!(revoke_grant(&controller, subject, read_id));
    // Gone from the agent listing entirely, exec included: the folder is still
    // attached, it just allows nothing the agent can act on.
    assert!(listed().is_empty());

    assert_eq!(
        grant_read().unwrap(),
        ControlResult::GrantRootCapability(GrantRootCapabilityResult { granted: true })
    );
    let restored = listed();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].capabilities.contains(&Capability::ReadFiles));
    assert!(
        !restored[0]
            .capabilities
            .contains(&Capability::ExecuteCommands),
        "restoring read must not resurrect the exec reach revoking it withdrew"
    );

    // Idempotent, and it disturbs nothing else the subject holds here.
    assert_eq!(
        grant_read().unwrap(),
        ControlResult::GrantRootCapability(GrantRootCapabilityResult { granted: false })
    );
    let mut held = grant_statements(&controller)
        .into_iter()
        .filter(|grant| {
            grant.subject == subject
                && matches!(grant.scope, Scope::Root { root_id: granted } if granted == root_id)
        })
        .map(|grant| grant.capability)
        .collect::<Vec<_>>();
    held.sort_by_key(|capability| format!("{capability:?}"));
    assert_eq!(
        held,
        vec![Capability::ReadFiles, Capability::WriteFiles],
        "the write grant the user never touched has to survive untouched"
    );
}

#[test]
fn an_empty_conversation_lists_no_roots_without_needing_a_grant() {
    let (_temp, broker, _path) = setup();
    let context = ExecutionContext::standalone(Uuid::new_v4()).unwrap();
    assert_eq!(
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots: Vec::new() }
    );
    assert!(list_approved(&broker.controller()).is_empty());
}

#[test]
fn grant_statements_report_every_minted_consent_with_safe_folder_identity() {
    let (_temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    register(&controller, subject, conversation, path, OperationId::new());

    let result = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::ListGrantStatements,
    }))
    .unwrap();
    let ControlResult::ListGrantStatements { grants } = result else {
        panic!("unexpected control result")
    };

    // One picker consent mints exactly the four-capability bundle, and every
    // statement carries the provenance the consent surface renders.
    let mut capabilities = grants
        .iter()
        .map(|grant| grant.capability)
        .collect::<Vec<_>>();
    capabilities.sort_by_key(|capability| format!("{capability:?}"));
    assert_eq!(
        capabilities,
        vec![
            Capability::ExecuteCommands,
            Capability::ListRoots,
            Capability::ReadFiles,
            Capability::WriteFiles,
        ]
    );
    for grant in &grants {
        assert_eq!(grant.subject, subject);
        assert_eq!(grant.consent_method, ConsentMethod::FolderPicker);
        match &grant.scope {
            // This registration mints only folder grants; computer-use scopes
            // never appear here.
            Scope::Subject | Scope::App { .. } | Scope::Screen => {
                assert_eq!(grant.root_display_name, None)
            }
            Scope::Root { .. } | Scope::PathSubtree { .. } => {
                // The safe identity is the registered folder's basename, the
                // same name the approved-roots listing exposes — never a path.
                assert_eq!(grant.root_display_name.as_deref(), Some("Documents"));
            }
        }
    }
}

#[test]
fn approved_roots_can_be_explicitly_attached_to_another_standalone_conversation() {
    let (_temp, broker, path) = setup();
    let first_conversation = Uuid::new_v4();
    let first_subject = GrantSubject::conversation(first_conversation).unwrap();
    let registered = register(
        &broker.controller(),
        first_subject,
        first_conversation,
        path,
        OperationId::new(),
    );
    let root_id = registered.root.root_id;
    mutate_attachment(
        &broker.controller(),
        OperationId::new(),
        first_subject,
        first_conversation,
        root_id,
        RootAttachmentMutationKind::Detach,
    )
    .unwrap();

    assert_eq!(
        list_approved(&broker.controller()),
        vec![registered.root.clone()]
    );
    assert_eq!(
        operate(
            &broker.operator(),
            ExecutionContext::standalone(first_conversation).unwrap(),
            OperationRequest::ListRoots,
        )
        .unwrap(),
        OperationResult::ListRoots { roots: Vec::new() }
    );

    let second_conversation = Uuid::new_v4();
    let second_subject = GrantSubject::conversation(second_conversation).unwrap();
    let attach_id = OperationId::new();
    mutate_attachment(
        &broker.controller(),
        attach_id,
        second_subject,
        second_conversation,
        root_id,
        RootAttachmentMutationKind::Attach,
    )
    .unwrap();
    assert!(broker
        .shared
        .state
        .lock()
        .unwrap()
        .grants
        .iter()
        .any(|grant| {
            grant.subject() == second_subject
                && grant.capability() == Capability::ReadFiles
                && matches!(grant.scope(), Scope::Root { root_id: granted } if *granted == root_id)
                && grant.consent().method() == ConsentMethod::PermissionDialog
        }));
    let conflicting_consent = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::AttachRoot(RootAttachmentMutationRequest {
            operation_id: attach_id,
            subject: second_subject,
            conversation_id: second_conversation,
            root_id,
            consent_method: Some(ConsentMethod::OperatorConfig),
        }),
    });
    assert!(matches!(
        conflicting_consent.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));
    let missing_consent = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::AttachRoot(RootAttachmentMutationRequest {
            operation_id: attach_id,
            subject: second_subject,
            conversation_id: second_conversation,
            root_id,
            consent_method: None,
        }),
    });
    assert!(matches!(
        missing_consent.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));
    let second_context = ExecutionContext::standalone(second_conversation).unwrap();
    assert_eq!(
        operate(
            &broker.operator(),
            second_context,
            OperationRequest::ListRoots,
        )
        .unwrap(),
        OperationResult::ListRoots {
            roots: vec![picker_access(registered.root)]
        }
    );
    assert!(matches!(
        operate(
            &broker.operator(),
            second_context,
            OperationRequest::ReadFile(PathRequest {
                root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            }),
        )
        .unwrap(),
        OperationResult::ReadFile(_)
    ));

    let unrelated = ExecutionContext::standalone(Uuid::new_v4()).unwrap();
    assert_eq!(
        operate(&broker.operator(), unrelated, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots: Vec::new() }
    );
}

#[test]
fn reused_standalone_approval_and_chat_attachment_survive_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    let first_conversation = Uuid::new_v4();
    let first_subject = GrantSubject::conversation(first_conversation).unwrap();
    let registered = register(
        &broker.controller(),
        first_subject,
        first_conversation,
        path,
        OperationId::new(),
    );
    mutate_attachment(
        &broker.controller(),
        OperationId::new(),
        first_subject,
        first_conversation,
        registered.root.root_id,
        RootAttachmentMutationKind::Detach,
    )
    .unwrap();
    let second_conversation = Uuid::new_v4();
    mutate_attachment(
        &broker.controller(),
        OperationId::new(),
        GrantSubject::conversation(second_conversation).unwrap(),
        second_conversation,
        registered.root.root_id,
        RootAttachmentMutationKind::Attach,
    )
    .unwrap();
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert_eq!(
        list_approved(&broker.controller()),
        vec![registered.root.clone()]
    );
    assert_eq!(
        operate(
            &broker.operator(),
            ExecutionContext::standalone(second_conversation).unwrap(),
            OperationRequest::ListRoots,
        )
        .unwrap(),
        OperationResult::ListRoots {
            roots: vec![picker_access(registered.root)]
        }
    );
}

#[test]
fn choosing_the_same_approved_folder_again_reuses_its_host_identity() {
    let (_temp, broker, path) = setup();
    let first_conversation = Uuid::new_v4();
    let first = register(
        &broker.controller(),
        GrantSubject::conversation(first_conversation).unwrap(),
        first_conversation,
        path.clone(),
        OperationId::new(),
    );
    let second_conversation = Uuid::new_v4();
    let second = register(
        &broker.controller(),
        GrantSubject::conversation(second_conversation).unwrap(),
        second_conversation,
        path,
        OperationId::new(),
    );

    assert_eq!(second.root, first.root);
    assert_eq!(
        list_approved(&broker.controller()),
        vec![first.root.clone()]
    );
    assert_eq!(
        operate(
            &broker.operator(),
            ExecutionContext::standalone(second_conversation).unwrap(),
            OperationRequest::ListRoots,
        )
        .unwrap(),
        OperationResult::ListRoots {
            roots: vec![picker_access(first.root)]
        }
    );
}

#[test]
fn legacy_physical_aliases_are_hidden_and_reused_deterministically_after_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    let first_conversation = Uuid::new_v4();
    let first_subject = GrantSubject::conversation(first_conversation).unwrap();
    let first = register(
        &broker.controller(),
        first_subject,
        first_conversation,
        path.clone(),
        OperationId::new(),
    );
    let alias_root_id = RootId::from_uuid(Uuid::from_u128(1)).unwrap();
    let alias_conversation = Uuid::new_v4();
    let alias_subject = GrantSubject::conversation(alias_conversation).unwrap();
    install_legacy_alias(
        &broker,
        first.root.root_id,
        alias_root_id,
        alias_subject,
        alias_conversation,
        path.clone(),
        OperationId::new(),
    );
    assert!(list_approved(&broker.controller()).is_empty());
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert!(list_approved(&broker.controller()).is_empty());

    let first_again = register(
        &broker.controller(),
        first_subject,
        first_conversation,
        path.clone(),
        OperationId::new(),
    );
    assert_eq!(first_again.root.root_id, first.root.root_id);

    let new_conversation = Uuid::new_v4();
    let new_subject = GrantSubject::conversation(new_conversation).unwrap();
    let new_registration = register(
        &broker.controller(),
        new_subject,
        new_conversation,
        path,
        OperationId::new(),
    );
    assert_eq!(new_registration.root.root_id, alias_root_id);
}

#[test]
fn legacy_physical_aliases_block_global_revoke_and_remain_valid_after_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    let first_conversation = Uuid::new_v4();
    let first_subject = GrantSubject::conversation(first_conversation).unwrap();
    let first = register(
        &broker.controller(),
        first_subject,
        first_conversation,
        path.clone(),
        OperationId::new(),
    );
    let alias_root_id = RootId::from_uuid(Uuid::from_u128(1)).unwrap();
    let alias_conversation = Uuid::new_v4();
    let alias_subject = GrantSubject::conversation(alias_conversation).unwrap();
    install_legacy_alias(
        &broker,
        first.root.root_id,
        alias_root_id,
        alias_subject,
        alias_conversation,
        path,
        OperationId::new(),
    );

    let revoke_id = OperationId::new();
    assert_eq!(
        revoke(
            &broker.controller(),
            revoke_id,
            alias_subject,
            alias_root_id,
        ),
        RevokeRootResult { revoked: false }
    );
    assert!(operate(
        &broker.operator(),
        ExecutionContext::standalone(first_conversation).unwrap(),
        OperationRequest::ReadFile(PathRequest {
            root_id: first.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .is_ok());
    assert!(operate(
        &broker.operator(),
        ExecutionContext::standalone(alias_conversation).unwrap(),
        OperationRequest::ReadFile(PathRequest {
            root_id: alias_root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .is_ok());
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert_eq!(
        revoke(
            &broker.controller(),
            revoke_id,
            alias_subject,
            alias_root_id,
        ),
        RevokeRootResult { revoked: false }
    );
    assert!(operate(
        &broker.operator(),
        ExecutionContext::standalone(alias_conversation).unwrap(),
        OperationRequest::ReadFile(PathRequest {
            root_id: alias_root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .is_ok());
}

#[test]
fn basename_collisions_are_not_offered_as_approved_roots() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("home/first/Documents");
    let second_path = temp.path().join("home/second/Documents");
    let unique_path = temp.path().join("home/third/Reports");
    for path in [&first_path, &second_path, &unique_path] {
        std::fs::create_dir_all(path).unwrap();
    }
    let broker = Broker::new(test_policy(&temp));
    let first_conversation = Uuid::new_v4();
    let first = register(
        &broker.controller(),
        GrantSubject::conversation(first_conversation).unwrap(),
        first_conversation,
        first_path,
        OperationId::new(),
    );
    let second_conversation = Uuid::new_v4();
    register(
        &broker.controller(),
        GrantSubject::conversation(second_conversation).unwrap(),
        second_conversation,
        second_path,
        OperationId::new(),
    );
    let unique_conversation = Uuid::new_v4();
    let unique = register(
        &broker.controller(),
        GrantSubject::conversation(unique_conversation).unwrap(),
        unique_conversation,
        unique_path,
        OperationId::new(),
    );

    assert_eq!(list_approved(&broker.controller()), vec![unique.root]);
    assert_eq!(
        operate(
            &broker.operator(),
            ExecutionContext::standalone(first_conversation).unwrap(),
            OperationRequest::ListRoots,
        )
        .unwrap(),
        OperationResult::ListRoots {
            roots: vec![picker_access(first.root)],
        }
    );
}

#[test]
fn register_list_read_and_revoke_are_one_live_authority_boundary() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();

    let roots = operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap();
    assert_eq!(
        roots,
        OperationResult::ListRoots {
            roots: vec![picker_access(registered.root.clone())]
        }
    );
    let listing = operate(
        &broker.operator(),
        context,
        OperationRequest::ListDirectory(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::root(),
        }),
    )
    .unwrap();
    let OperationResult::ListDirectory { entries } = listing else {
        panic!("unexpected listing result")
    };
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["note.txt", "reports"]
    );
    let read = operate(
        &broker.operator(),
        context,
        OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .unwrap();
    assert_eq!(
        read,
        OperationResult::ReadFile(ReadFileResult {
            content: "hello from broker".to_owned(),
            bytes: 17,
        })
    );

    let revoked = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id: OperationId::new(),
            subject,
            root_id: registered.root.root_id,
        }),
    });
    assert_eq!(
        unwrap_response(revoked).unwrap(),
        ControlResult::RevokeRoot(RevokeRootResult { revoked: true })
    );
    assert!(matches!(
        operate(
            &broker.operator(),
            context,
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            })
        ),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));
}

#[test]
fn registration_receipt_lookup_never_starts_or_resumes_a_mutation() {
    let (temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation_id = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation_id).unwrap();
    let unknown_id = OperationId::new();
    assert_eq!(
        lookup_register_receipt(&controller, unknown_id, subject, conversation_id).unwrap(),
        LookupRegisterRootReceiptResult {
            operation_id: unknown_id,
            receipt: RegisterRootReceipt::Unknown,
        }
    );

    let operation_id = OperationId::new();
    broker.shared.state.lock().unwrap().mutations.insert(
        operation_id,
        MutationRecord::Register {
            request: RegisterFingerprint {
                subject,
                conversation_id,
                path: path.clone(),
                consent_method: ConsentMethod::FolderPicker,
            },
            outcome: MutationOutcome::Pending,
        },
    );
    assert_eq!(
        lookup_register_receipt(&controller, operation_id, subject, conversation_id).unwrap(),
        LookupRegisterRootReceiptResult {
            operation_id,
            receipt: RegisterRootReceipt::Pending,
        }
    );
    let other_conversation = Uuid::new_v4();
    assert!(matches!(
        lookup_register_receipt(
            &controller,
            operation_id,
            GrantSubject::conversation(other_conversation).unwrap(),
            other_conversation,
        ),
        Err(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));
    let state = broker.shared.state.lock().unwrap();
    assert!(state.roots.is_empty());
    assert!(!state.active_mutations.contains(&operation_id));
    drop(state);

    let completed = register(&controller, subject, conversation_id, path, operation_id);
    let completed_root = completed.root.clone();
    assert_eq!(
        lookup_register_receipt(&controller, operation_id, subject, conversation_id).unwrap(),
        LookupRegisterRootReceiptResult {
            operation_id,
            receipt: RegisterRootReceipt::Completed {
                root: completed_root.clone(),
            },
        }
    );
    let revoke = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id: OperationId::new(),
            subject,
            root_id: completed_root.root_id,
        }),
    }))
    .unwrap();
    assert_eq!(
        revoke,
        ControlResult::RevokeRoot(RevokeRootResult { revoked: true })
    );
    assert_eq!(
        lookup_register_receipt(&controller, operation_id, subject, conversation_id).unwrap(),
        LookupRegisterRootReceiptResult {
            operation_id,
            receipt: RegisterRootReceipt::Disconnected {
                root: completed_root,
            },
        }
    );

    let failed_id = OperationId::new();
    let failure = unwrap_response(controller.handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id: failed_id,
            subject,
            conversation_id,
            path: temp.path().join("sensitive"),
            consent_method: ConsentMethod::FolderPicker,
        }),
    }))
    .unwrap_err();
    assert_eq!(
        lookup_register_receipt(&controller, failed_id, subject, conversation_id).unwrap(),
        LookupRegisterRootReceiptResult {
            operation_id: failed_id,
            receipt: RegisterRootReceipt::Failed { error: failure },
        }
    );

    let revoke_id = OperationId::new();
    broker.shared.state.lock().unwrap().mutations.insert(
        revoke_id,
        MutationRecord::Revoke {
            request: RevokeFingerprint {
                subject,
                root_id: RootId::new(),
            },
            outcome: MutationOutcome::Pending,
        },
    );
    assert!(matches!(
        lookup_register_receipt(&controller, revoke_id, subject, conversation_id),
        Err(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));
}

#[test]
fn registration_receipt_lookup_is_a_de_sensitized_audited_control_read() {
    let (_temp, broker, _path, audit) = audited_setup();
    let conversation_id = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation_id).unwrap();
    let operation_id = OperationId::new();
    lookup_register_receipt(&broker.controller(), operation_id, subject, conversation_id).unwrap();

    let events = audit.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].operation,
        AuditOperation::LookupRegisterRootReceipt
    );
    assert_eq!(events[0].operation_id, Some(operation_id));
    assert_eq!(
        events[0].actor,
        AuditActor::Control {
            subject,
            conversation_id: Some(conversation_id),
        }
    );
    assert_eq!(events[0].target, AuditTarget::Subject);
}

#[test]
fn repeated_registration_reuses_the_same_root_and_attaches_new_project_chat() {
    let (_temp, broker, path) = setup();
    let project = Uuid::new_v4();
    let first_chat = Uuid::new_v4();
    let second_chat = Uuid::new_v4();
    let subject = GrantSubject::project(project).unwrap();

    let first = register(
        &broker.controller(),
        subject,
        first_chat,
        path.clone(),
        OperationId::new(),
    );
    let repeated = register(
        &broker.controller(),
        subject,
        first_chat,
        path.clone(),
        OperationId::new(),
    );
    let attached = register(
        &broker.controller(),
        subject,
        second_chat,
        path,
        OperationId::new(),
    );

    assert_eq!(repeated.root, first.root);
    assert_eq!(attached.root, first.root);
    for chat in [first_chat, second_chat] {
        let roots = operate(
            &broker.operator(),
            ExecutionContext::project_chat(chat, project).unwrap(),
            OperationRequest::ListRoots,
        )
        .unwrap();
        assert_eq!(
            roots,
            OperationResult::ListRoots {
                roots: vec![picker_access(first.root.clone())]
            }
        );
    }
}

#[test]
fn broker_audits_control_reads_denials_and_authorizing_grants() {
    let (_temp, broker, path, audit) = audited_setup();
    let hello = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::Hello,
    });
    assert!(matches!(hello.response, Response::Ok(_)));
    assert!(audit.events.lock().unwrap().is_empty());

    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let request = OperationRequest::ReadFile(PathRequest {
        root_id: registered.root.root_id,
        path: RelativePath::parse("note.txt").unwrap(),
    });
    operate(
        &broker.operator(),
        ExecutionContext::standalone(conversation).unwrap(),
        request.clone(),
    )
    .unwrap();
    assert!(matches!(
        operate(
            &broker.operator(),
            ExecutionContext::standalone(Uuid::new_v4()).unwrap(),
            request,
        ),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));

    let events = audit.events.lock().unwrap();
    assert_eq!(events.len(), 4);
    // Registration is a mutation, so it is recorded as an intent before it runs
    // and as a completion afterwards, correlated by request identity.
    assert_eq!(events[0].operation, AuditOperation::RegisterRoot);
    assert_eq!(events[0].outcome, AuditOutcome::Attempted);
    assert_eq!(events[1].operation, AuditOperation::RegisterRoot);
    assert_eq!(events[1].outcome, AuditOutcome::Allowed);
    assert_eq!(events[0].request_id, events[1].request_id);
    assert!(matches!(
        &events[0].target,
        AuditTarget::SelectedFolder { display_name } if display_name.as_str() == "Documents"
    ));
    assert_eq!(events[2].operation, AuditOperation::ReadFile);
    assert_eq!(events[2].outcome, AuditOutcome::Allowed);
    assert!(events[2].grant_id.is_some());
    assert_eq!(events[2].bytes, Some(17));
    assert!(matches!(
        &events[2].target,
        AuditTarget::Path { root_id, relative }
            if *root_id == registered.root.root_id && relative.as_str() == "note.txt"
    ));
    assert_eq!(events[3].outcome, AuditOutcome::Denied);
    assert_eq!(events[3].error_code, Some(ErrorCode::Denied));
    assert!(events[3].grant_id.is_none());
    let encoded = serde_json::to_string(&*events).unwrap();
    assert!(!encoded.contains("home/Documents"));
    assert!(!encoded.contains("hello from broker"));
}

#[test]
fn an_unrecordable_mutation_is_refused_while_reads_still_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("home/Documents");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
    let audit = Arc::new(BreakableAudit::default());
    let broker = Broker::with_audit_sink(test_policy(&temp), audit.clone());
    let conversation = Uuid::new_v4();
    let context = ExecutionContext::standalone(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        root.clone(),
        OperationId::new(),
    );
    audit.broken.store(true, Ordering::SeqCst);

    let refused = operate(
        &broker.operator(),
        context,
        write_request(
            OperationId::new(),
            registered.root.root_id,
            "unrecorded.txt",
            WriteFileMode::Create,
            None,
            b"never written",
        ),
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::AuditUnavailable);
    assert!(!root.join("unrecorded.txt").exists());

    let refused = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id: OperationId::new(),
            subject: GrantSubject::conversation(conversation).unwrap(),
            root_id: registered.root.root_id,
        }),
    }))
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::AuditUnavailable);

    // Losing the audit log must not cost the user access to folders they
    // already approved; an unrecorded read is the lesser failure.
    assert!(operate(
        &broker.operator(),
        context,
        OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .is_ok());

    // Recovery needs no restart: the next mutation goes through.
    audit.broken.store(false, Ordering::SeqCst);
    assert!(operate(
        &broker.operator(),
        context,
        write_request(
            OperationId::new(),
            registered.root.root_id,
            "recorded.txt",
            WriteFileMode::Create,
            None,
            b"written",
        ),
    )
    .is_ok());
}

#[test]
fn project_grant_still_requires_an_exact_conversation_attachment() {
    let (_temp, broker, path) = setup();
    let project = Uuid::new_v4();
    let attached = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::project(project).unwrap(),
        attached,
        path,
        OperationId::new(),
    );
    let request = OperationRequest::ReadFile(PathRequest {
        root_id: registered.root.root_id,
        path: RelativePath::parse("note.txt").unwrap(),
    });
    assert!(operate(
        &broker.operator(),
        ExecutionContext::project_chat(attached, project).unwrap(),
        request.clone(),
    )
    .is_ok());
    assert!(matches!(
        operate(
            &broker.operator(),
            ExecutionContext::project_chat(Uuid::new_v4(), project).unwrap(),
            request,
        ),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn pinned_root_rejects_a_symlink_escape_at_operation_time() {
    use std::os::unix::fs::symlink;

    let (temp, broker, path) = setup();
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "not connected").unwrap();
    symlink(&outside, path.join("escape")).unwrap();

    let conversation = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path,
        OperationId::new(),
    );
    let result = operate(
        &broker.operator(),
        ExecutionContext::standalone(conversation).unwrap(),
        OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("escape/secret.txt").unwrap(),
        }),
    );
    assert!(matches!(
        result,
        Err(ErrorResponse {
            code: ErrorCode::HostIo,
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn directory_bound_counts_unaddressable_entries_examined() {
    let (_temp, broker, path) = setup();
    for index in 0..=MAX_LIST_DIR_ENTRIES {
        std::fs::write(path.join(format!("skip:{index}")), b"").unwrap();
    }
    let conversation = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path,
        OperationId::new(),
    );
    let result = operate(
        &broker.operator(),
        ExecutionContext::standalone(conversation).unwrap(),
        OperationRequest::ListDirectory(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::root(),
        }),
    );
    assert!(matches!(
        result,
        Err(ErrorResponse {
            code: ErrorCode::TooLarge,
            ..
        })
    ));
}

#[test]
fn connected_root_results_are_bounded_before_transport_serialization() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let consent = ConsentRecord::new(ConsentMethod::FolderPicker, Utc::now());
    let pinned = Arc::new(broker.shared.policy.open_root(&path).unwrap());
    let mut state = State::default();
    state.grants.push(
        Grant::from_consent(
            GrantId::new(),
            subject,
            Capability::ListRoots,
            Scope::Subject,
            consent.clone(),
        )
        .unwrap(),
    );
    for _ in 0..=MAX_LIST_ROOTS {
        let root_id = RootId::new();
        state.roots.insert(
            root_id,
            RegisteredRoot {
                owner: subject,
                display_name: "Documents".to_owned(),
                root: pinned.clone(),
            },
        );
        state
            .attachments
            .push(RootAttachment::new(conversation, root_id).unwrap());
        state.grants.push(
            Grant::from_consent(
                GrantId::new(),
                subject,
                Capability::ReadFiles,
                Scope::Root { root_id },
                consent.clone(),
            )
            .unwrap(),
        );
    }
    assert!(matches!(
        list_roots(
            &state,
            ExecutionContext::standalone(conversation).unwrap(),
            true,
        ),
        Err(BrokerError::RootListTooLarge)
    ));
}

#[test]
fn connected_root_display_names_are_bounded_on_utf8_boundaries() {
    let component = "é".repeat(MAX_ROOT_DISPLAY_BYTES);
    let display = root_display_name(Path::new(&component));
    assert!(display.len() <= MAX_ROOT_DISPLAY_BYTES);
    assert!(display.is_char_boundary(display.len()));
}

#[test]
fn control_mutations_are_idempotent_and_reject_identity_reuse() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let operation_id = OperationId::new();
    let first = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        operation_id,
    );
    let retry = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        operation_id,
    );
    assert_eq!(retry, first);
    let conflict = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id,
            subject,
            root_id: first.root.root_id,
        }),
    });
    assert!(matches!(
        conflict.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));
}

#[test]
fn attach_and_detach_are_exact_conversation_mutations() {
    let (_temp, broker, path) = setup();
    let project_id = Uuid::new_v4();
    let first_conversation = Uuid::new_v4();
    let second_conversation = Uuid::new_v4();
    let subject = GrantSubject::project(project_id).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        first_conversation,
        path,
        OperationId::new(),
    );
    let root_id = registered.root.root_id;

    let attach_id = OperationId::new();
    let attached = mutate_attachment(
        &broker.controller(),
        attach_id,
        subject,
        second_conversation,
        root_id,
        RootAttachmentMutationKind::Attach,
    )
    .unwrap();
    assert!(attached.changed);
    assert_eq!(
        mutate_attachment(
            &broker.controller(),
            attach_id,
            subject,
            second_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap(),
        attached
    );
    assert!(matches!(
        mutate_attachment(
            &broker.controller(),
            attach_id,
            subject,
            second_conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        ),
        Err(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));

    let second_context = ExecutionContext::project_chat(second_conversation, project_id).unwrap();
    assert!(matches!(
        operate(&broker.operator(), second_context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots } if roots == vec![picker_access(registered.root.clone())]
    ));

    let detach_id = OperationId::new();
    let detached = mutate_attachment(
        &broker.controller(),
        detach_id,
        subject,
        first_conversation,
        root_id,
        RootAttachmentMutationKind::Detach,
    )
    .unwrap();
    assert!(detached.changed);
    let first_context = ExecutionContext::project_chat(first_conversation, project_id).unwrap();
    assert!(matches!(
        operate(&broker.operator(), first_context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots } if roots.is_empty()
    ));
    assert!(matches!(
        operate(&broker.operator(), second_context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots } if roots == vec![picker_access(registered.root)]
    ));
    assert!(
        !mutate_attachment(
            &broker.controller(),
            OperationId::new(),
            subject,
            first_conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        )
        .unwrap()
        .changed
    );
}

#[test]
fn attachment_receipts_report_historical_result_and_current_state() {
    let (_temp, broker, path) = setup();
    let project_id = Uuid::new_v4();
    let registered_conversation = Uuid::new_v4();
    let attached_conversation = Uuid::new_v4();
    let subject = GrantSubject::project(project_id).unwrap();
    let root_id = register(
        &broker.controller(),
        subject,
        registered_conversation,
        path,
        OperationId::new(),
    )
    .root
    .root_id;
    let attach_id = OperationId::new();
    let attach = mutate_attachment(
        &broker.controller(),
        attach_id,
        subject,
        attached_conversation,
        root_id,
        RootAttachmentMutationKind::Attach,
    )
    .unwrap();
    let lookup = LookupRootAttachmentReceiptRequest {
        operation_id: attach_id,
        subject,
        conversation_id: attached_conversation,
        root_id,
        mutation: RootAttachmentMutationKind::Attach,
    };
    assert_eq!(
        lookup_attachment_receipt(&broker.controller(), lookup).unwrap(),
        RootAttachmentMutationReceipt::Completed {
            result: attach,
            currently_attached: true,
        }
    );

    let detach_id = OperationId::new();
    mutate_attachment(
        &broker.controller(),
        detach_id,
        subject,
        attached_conversation,
        root_id,
        RootAttachmentMutationKind::Detach,
    )
    .unwrap();
    assert!(matches!(
        lookup_attachment_receipt(&broker.controller(), lookup).unwrap(),
        RootAttachmentMutationReceipt::Completed {
            currently_attached: false,
            ..
        }
    ));
    let conflicting_lookup = LookupRootAttachmentReceiptRequest {
        mutation: RootAttachmentMutationKind::Detach,
        ..lookup
    };
    assert!(matches!(
        lookup_attachment_receipt(&broker.controller(), conflicting_lookup),
        Err(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));
}

#[test]
fn failed_attachment_mutation_is_durable_and_cannot_widen_authority() {
    let (_temp, broker, _path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let operation_id = OperationId::new();
    let root_id = RootId::new();
    let first = mutate_attachment(
        &broker.controller(),
        operation_id,
        subject,
        conversation,
        root_id,
        RootAttachmentMutationKind::Attach,
    )
    .unwrap_err();
    let retry = mutate_attachment(
        &broker.controller(),
        operation_id,
        subject,
        conversation,
        root_id,
        RootAttachmentMutationKind::Attach,
    )
    .unwrap_err();
    assert_eq!(first, retry);
    assert_eq!(first.code, ErrorCode::InvalidRoot);
    assert!(matches!(
        lookup_attachment_receipt(
            &broker.controller(),
            LookupRootAttachmentReceiptRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                root_id,
                mutation: RootAttachmentMutationKind::Attach,
            },
        )
        .unwrap(),
        RootAttachmentMutationReceipt::Failed { error, currently_attached: false }
            if error == first
    ));
}

/// A rejected mutation changed nothing, but the broker still knows what it
/// holds. Saying so is what lets a caller tell "nothing is attached" apart from
/// "cannot say" — the product records that observation durably, and an
/// unknowable one can never be settled afterwards.
#[test]
fn a_failed_mutation_reports_the_attachment_it_could_not_change() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let owner = GrantSubject::project(Uuid::new_v4()).unwrap();
    let registered = register(
        &broker.controller(),
        owner,
        conversation,
        path,
        OperationId::new(),
    );
    let root_id = registered.root.root_id;

    // A detach that carries a consent method is malformed — detaching answers
    // no question — so it is refused while the attachment plainly still exists.
    let subject = GrantSubject::conversation(conversation).unwrap();
    let operation_id = OperationId::new();
    assert_eq!(
        unwrap_response(broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::DetachRoot(RootAttachmentMutationRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                root_id,
                consent_method: Some(ConsentMethod::PermissionDialog),
            }),
        }))
        .unwrap_err()
        .code,
        ErrorCode::InvalidRequest
    );
    assert!(matches!(
        lookup_attachment_receipt(
            &broker.controller(),
            LookupRootAttachmentReceiptRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                root_id,
                mutation: RootAttachmentMutationKind::Detach,
            },
        )
        .unwrap(),
        RootAttachmentMutationReceipt::Failed {
            currently_attached: true,
            ..
        }
    ));
}

#[test]
fn failed_control_mutation_still_binds_its_operation_identity() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let operation_id = OperationId::new();
    let invalid = ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id,
            subject,
            conversation_id: conversation,
            path: path.clone(),
            consent_method: ConsentMethod::PermissionDialog,
        }),
    };
    let first = broker.controller().handle(invalid.clone());
    let retry = broker.controller().handle(invalid);
    assert_eq!(first.response, retry.response);
    assert!(matches!(
        first.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            ..
        })
    ));

    let conflict = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id,
            subject,
            conversation_id: conversation,
            path,
            consent_method: ConsentMethod::FolderPicker,
        }),
    });
    assert!(matches!(
        conflict.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::OperationIdConflict,
            ..
        })
    ));
}

#[test]
fn in_flight_mutation_identity_cannot_be_reused() {
    let operation_id = OperationId::new();
    let subject = GrantSubject::conversation(Uuid::new_v4()).unwrap();
    let request = RegisterFingerprint {
        subject,
        conversation_id: subject.id(),
        path: PathBuf::from("/selected/folder"),
        consent_method: ConsentMethod::FolderPicker,
    };
    let mut state = State::default();
    assert!(matches!(
        claim_register(&mut state, operation_id, &request).unwrap(),
        Claim::Start
    ));
    assert!(matches!(
        claim_register(&mut state, operation_id, &request),
        Err(BrokerError::OperationInProgress)
    ));
    let mut different = request.clone();
    different.path = PathBuf::from("/other/folder");
    assert!(matches!(
        claim_register(&mut state, operation_id, &different),
        Err(BrokerError::OperationIdConflict)
    ));
}

#[test]
fn completed_revocation_fences_an_in_flight_read_result() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let relative = RelativePath::parse("note.txt").unwrap();
    let operator = broker.operator();
    let (directory, _) = operator
        .authorized_root(context, registered.root.root_id, &relative)
        .unwrap();
    let buffered = read_file(&directory, &relative).unwrap();

    let revoked = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id: OperationId::new(),
            subject,
            root_id: registered.root.root_id,
        }),
    });
    assert!(matches!(revoked.response, Response::Ok(_)));
    assert!(matches!(
        operator.reauthorize(context, registered.root.root_id, &relative),
        Err(BrokerError::Denied)
    ));
    drop(buffered);
}

#[test]
fn hello_negotiates_across_version_skew_but_operations_do_not() {
    let (_temp, broker, _path) = setup();
    let hello_request_id = crate::RequestId::new();
    let hello = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION + 1,
        request_id: hello_request_id,
        request: ControlRequest::Hello,
    });
    assert_eq!(hello.request_id, hello_request_id);
    assert_eq!(
        unwrap_response(hello).unwrap(),
        ControlResult::Hello(super::hello(false))
    );

    let error = broker.operator().handle(OperationEnvelope {
        protocol_version: PROTOCOL_VERSION + 1,
        request_id: crate::RequestId::new(),
        context: ExecutionContext::standalone(Uuid::new_v4()).unwrap(),
        request: OperationRequest::ListRoots,
    });
    assert!(matches!(
        error.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::ProtocolVersion,
            retryable: false,
            ..
        })
    ));
}

#[test]
fn transport_retryability_is_an_explicit_transient_allowlist() {
    for kind in [
        io::ErrorKind::Interrupted,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::TimedOut,
    ] {
        let response = error_response(BrokerError::Io(io::Error::from(kind)));
        assert_eq!(response.code, ErrorCode::HostIo);
        assert!(response.retryable, "{kind:?}");
    }
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidInput] {
        let response = error_response(BrokerError::Io(io::Error::from(kind)));
        assert_eq!(response.code, ErrorCode::HostIo);
        assert!(!response.retryable, "{kind:?}");
    }
    let poisoned = error_response(BrokerError::StatePoisoned);
    assert_eq!(poisoned.code, ErrorCode::Internal);
    assert!(!poisoned.retryable);
}

#[test]
fn durable_registry_receipts_and_revocation_survive_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let register_id = OperationId::new();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        register_id,
    );
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let retry = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        register_id,
    );
    assert_eq!(retry, registered);
    let context = ExecutionContext::standalone(conversation).unwrap();
    assert!(operate(
        &broker.operator(),
        context,
        OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .is_ok());

    let revoke_id = OperationId::new();
    let revoke = ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id: revoke_id,
            subject,
            root_id: registered.root.root_id,
        }),
    };
    let first = unwrap_response(broker.controller().handle(revoke.clone())).unwrap();
    assert_eq!(
        first,
        ControlResult::RevokeRoot(RevokeRootResult { revoked: true })
    );
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let retry = unwrap_response(broker.controller().handle(revoke)).unwrap();
    assert_eq!(retry, first);
    assert_eq!(
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots: Vec::new() }
    );
    assert!(matches!(
        operate(
            &broker.operator(),
            context,
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            }),
        ),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));
}

#[test]
fn conversation_attachments_and_receipts_survive_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    let project_id = Uuid::new_v4();
    let first_conversation = Uuid::new_v4();
    let second_conversation = Uuid::new_v4();
    let subject = GrantSubject::project(project_id).unwrap();
    let root_id = register(
        &broker.controller(),
        subject,
        first_conversation,
        path,
        OperationId::new(),
    )
    .root
    .root_id;
    let attach_id = OperationId::new();
    let attach = mutate_attachment(
        &broker.controller(),
        attach_id,
        subject,
        second_conversation,
        root_id,
        RootAttachmentMutationKind::Attach,
    )
    .unwrap();
    let detach_id = OperationId::new();
    let detach = mutate_attachment(
        &broker.controller(),
        detach_id,
        subject,
        first_conversation,
        root_id,
        RootAttachmentMutationKind::Detach,
    )
    .unwrap();
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert_eq!(
        mutate_attachment(
            &broker.controller(),
            attach_id,
            subject,
            second_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap(),
        attach
    );
    assert_eq!(
        mutate_attachment(
            &broker.controller(),
            detach_id,
            subject,
            first_conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        )
        .unwrap(),
        detach
    );
    let first_context = ExecutionContext::project_chat(first_conversation, project_id).unwrap();
    let second_context = ExecutionContext::project_chat(second_conversation, project_id).unwrap();
    assert_eq!(
        operate(
            &broker.operator(),
            first_context,
            OperationRequest::ListRoots
        )
        .unwrap(),
        OperationResult::ListRoots { roots: Vec::new() }
    );
    assert!(matches!(
        operate(&broker.operator(), second_context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots } if roots.len() == 1 && roots[0].root_id == root_id
    ));
    assert!(matches!(
        lookup_attachment_receipt(
            &broker.controller(),
            LookupRootAttachmentReceiptRequest {
                operation_id: detach_id,
                subject,
                conversation_id: first_conversation,
                root_id,
                mutation: RootAttachmentMutationKind::Detach,
            }
        )
        .unwrap(),
        RootAttachmentMutationReceipt::Completed {
            result,
            currently_attached: false,
        } if result == detach
    ));
}

#[test]
fn unavailable_audit_does_not_block_restart_or_read_access() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path,
        OperationId::new(),
    );
    drop(broker);
    std::fs::write(
        state_dir.join("host-broker-audit.previous.jsonl"),
        vec![b'x'; 8 * 1024 * 1024 + 1],
    )
    .unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let result = operate(
        &broker.operator(),
        ExecutionContext::standalone(conversation).unwrap(),
        OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    );
    assert!(result.is_ok());
}

#[test]
fn pending_registration_resumes_after_completion_save_failure_and_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    broker
        .shared
        .state_file
        .as_ref()
        .unwrap()
        .fail_after_saves(1);
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let operation_id = OperationId::new();
    let request = ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id,
            subject,
            conversation_id: conversation,
            path: path.clone(),
            consent_method: ConsentMethod::FolderPicker,
        }),
    };
    let failed = broker.controller().handle(request);
    assert!(matches!(
        failed.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::HostIo,
            ..
        })
    ));
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert_eq!(
        lookup_register_receipt(&broker.controller(), operation_id, subject, conversation,)
            .unwrap(),
        LookupRegisterRootReceiptResult {
            operation_id,
            receipt: RegisterRootReceipt::Pending,
        }
    );
    assert!(broker.shared.state.lock().unwrap().roots.is_empty());
    let completed = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        operation_id,
    );
    assert_eq!(completed.root.display_name, "Documents");
}

#[test]
fn ambiguous_state_publication_fails_closed_until_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    broker
        .shared
        .state_file
        .as_ref()
        .unwrap()
        .fail_once_after_publish();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let operation_id = OperationId::new();
    let request = ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id,
            subject,
            conversation_id: conversation,
            path: path.clone(),
            consent_method: ConsentMethod::FolderPicker,
        }),
    };
    let ambiguous = broker.controller().handle(request);
    assert!(matches!(
        ambiguous.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::Internal,
            retryable: false,
            ..
        })
    ));
    let unavailable = broker.operator().handle(OperationEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        context: ExecutionContext::standalone(conversation).unwrap(),
        request: OperationRequest::ListRoots,
    });
    assert!(matches!(
        unavailable.response,
        Response::Error(ErrorResponse {
            code: ErrorCode::Internal,
            retryable: false,
            ..
        })
    ));
    assert!(matches!(
        lookup_register_receipt(&broker.controller(), operation_id, subject, conversation,),
        Err(ErrorResponse {
            code: ErrorCode::Internal,
            retryable: false,
            ..
        })
    ));
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert_eq!(
        lookup_register_receipt(&broker.controller(), operation_id, subject, conversation,)
            .unwrap(),
        LookupRegisterRootReceiptResult {
            operation_id,
            receipt: RegisterRootReceipt::Pending,
        }
    );
    let completed = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        operation_id,
    );
    assert_eq!(completed.root.display_name, "Documents");
}

#[test]
fn attachment_publication_failures_recover_from_the_durable_boundary() {
    let (temp, broker, path, state_dir) = durable_setup();
    let project_id = Uuid::new_v4();
    let registered_conversation = Uuid::new_v4();
    let attached_conversation = Uuid::new_v4();
    let subject = GrantSubject::project(project_id).unwrap();
    let root_id = register(
        &broker.controller(),
        subject,
        registered_conversation,
        path,
        OperationId::new(),
    )
    .root
    .root_id;

    let unpublished_id = OperationId::new();
    broker
        .shared
        .state_file
        .as_ref()
        .unwrap()
        .fail_after_saves(0);
    assert!(matches!(
        mutate_attachment(
            &broker.controller(),
            unpublished_id,
            subject,
            attached_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        ),
        Err(ErrorResponse {
            code: ErrorCode::HostIo,
            ..
        })
    ));
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert_eq!(
        lookup_attachment_receipt(
            &broker.controller(),
            LookupRootAttachmentReceiptRequest {
                operation_id: unpublished_id,
                subject,
                conversation_id: attached_conversation,
                root_id,
                mutation: RootAttachmentMutationKind::Attach,
            },
        )
        .unwrap(),
        RootAttachmentMutationReceipt::Unknown
    );
    let context = ExecutionContext::project_chat(attached_conversation, project_id).unwrap();
    assert!(matches!(
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots } if roots.is_empty()
    ));

    let published_id = OperationId::new();
    broker
        .shared
        .state_file
        .as_ref()
        .unwrap()
        .fail_once_after_publish();
    assert!(matches!(
        mutate_attachment(
            &broker.controller(),
            published_id,
            subject,
            attached_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        ),
        Err(ErrorResponse {
            code: ErrorCode::Internal,
            retryable: false,
            ..
        })
    ));
    assert!(matches!(
        lookup_attachment_receipt(
            &broker.controller(),
            LookupRootAttachmentReceiptRequest {
                operation_id: published_id,
                subject,
                conversation_id: attached_conversation,
                root_id,
                mutation: RootAttachmentMutationKind::Attach,
            },
        ),
        Err(ErrorResponse {
            code: ErrorCode::Internal,
            retryable: false,
            ..
        })
    ));
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let receipt = lookup_attachment_receipt(
        &broker.controller(),
        LookupRootAttachmentReceiptRequest {
            operation_id: published_id,
            subject,
            conversation_id: attached_conversation,
            root_id,
            mutation: RootAttachmentMutationKind::Attach,
        },
    )
    .unwrap();
    assert!(matches!(
        receipt,
        RootAttachmentMutationReceipt::Completed {
            result: RootAttachmentMutationResult { changed: true, .. },
            currently_attached: true,
        }
    ));
    assert!(
        mutate_attachment(
            &broker.controller(),
            published_id,
            subject,
            attached_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap()
        .changed
    );
    assert!(matches!(
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots } if roots.len() == 1
    ));
}

#[test]
fn one_process_exclusively_owns_a_durable_broker_directory() {
    let (temp, broker, _path, state_dir) = durable_setup();
    assert!(matches!(
        Broker::open(test_policy(&temp), &state_dir),
        Err(BrokerError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock
    ));
    drop(broker);
    assert!(Broker::open(test_policy(&temp), &state_dir).is_ok());
}

#[cfg(unix)]
#[test]
fn durable_state_uses_private_directory_and_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let (_temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path,
        OperationId::new(),
    );
    assert_eq!(
        std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(state_dir.join("host-broker-state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn an_unreachable_root_is_set_aside_rather_than_blocking_the_others() {
    let (temp, broker, offline_path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let reachable_path = temp.path().join("home/Reports");
    std::fs::create_dir_all(&reachable_path).unwrap();
    let offline = register(
        &broker.controller(),
        subject,
        conversation,
        offline_path.clone(),
        OperationId::new(),
    );
    let reachable = register(
        &broker.controller(),
        subject,
        conversation,
        reachable_path,
        OperationId::new(),
    );
    drop(broker);
    // Renaming stands in for the volume going away: the directory keeps its
    // host identity, so it is the same folder when it comes back.
    let stashed = offline_path.with_file_name("Documents-offline");
    std::fs::rename(&offline_path, &stashed).unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let context = ExecutionContext::standalone(conversation).unwrap();
    assert_eq!(
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots {
            roots: vec![picker_access(reachable.root)]
        }
    );
    assert!(
        std::fs::read_to_string(state_dir.join("host-broker-audit.jsonl"))
            .unwrap()
            .contains("prune_unavailable_root")
    );

    // A later mutation rewrites the state file. The offline approval has to
    // survive that, or an unplugged drive would silently cost the user their
    // consent.
    let another = temp.path().join("home/Archive");
    std::fs::create_dir_all(&another).unwrap();
    register(
        &broker.controller(),
        subject,
        conversation,
        another,
        OperationId::new(),
    );
    drop(broker);
    std::fs::rename(&stashed, &offline_path).unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let OperationResult::ListRoots { roots } =
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap()
    else {
        panic!("unexpected operation result")
    };
    assert!(roots
        .iter()
        .any(|root| root.root_id == offline.root.root_id));
}

/// The set-aside product surface: while a root's directory is gone the
/// listing names it safely — reason, owner, and the attachments riding out
/// the outage — where the approved-roots listing deliberately omits it, and a
/// deliberate revocation by the owner forgets it for good rather than letting
/// the approval linger forever.
#[test]
fn set_aside_roots_are_listed_for_the_product_and_forgettable() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let root_id = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    )
    .root
    .root_id;
    drop(broker);
    let stashed = path.with_file_name("Documents-offline");
    std::fs::rename(&path, &stashed).unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let controller = broker.controller();
    assert!(list_approved(&controller).is_empty());
    assert_eq!(
        list_unavailable(&controller),
        vec![UnavailableRootSummary {
            root_id,
            display_name: "Documents".to_owned(),
            reason: UnavailableRootReason::Missing,
            owner: subject,
            attached_conversations: vec![conversation],
        }]
    );

    // Forgetting is the owner's deliberate instruction, and it reaches the
    // set-aside registration: grants, attachment, and listing all go.
    assert_eq!(
        revoke(&controller, OperationId::new(), subject, root_id),
        RevokeRootResult { revoked: true }
    );
    assert!(list_unavailable(&controller).is_empty());
    // The subject-wide ListRoots grant is not the root's; everything scoped to
    // the forgotten root is gone.
    assert!(grant_statements(&controller)
        .iter()
        .all(|grant| matches!(grant.scope, Scope::Subject)));
    drop(controller);
    drop(broker);

    // The directory coming back does not resurrect the forgotten approval.
    std::fs::rename(&stashed, &path).unwrap();
    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert!(list_approved(&broker.controller()).is_empty());
    assert!(list_unavailable(&broker.controller()).is_empty());
}

#[test]
fn restart_refuses_to_rebind_a_grant_to_a_replaced_folder() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    );
    drop(broker);
    let original = path.with_file_name("Documents-original");
    std::fs::rename(&path, original).unwrap();
    std::fs::create_dir(&path).unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let context = ExecutionContext::standalone(conversation).unwrap();
    assert_eq!(
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots { roots: Vec::new() }
    );
}

#[test]
fn persisted_receipts_must_match_authoritative_state() {
    let (_temp, broker, path, _state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let state = broker.shared.state.lock().unwrap().clone();

    let mut inconsistent_revoke = state.clone();
    inconsistent_revoke.mutations.insert(
        OperationId::new(),
        MutationRecord::Revoke {
            request: RevokeFingerprint {
                subject,
                root_id: registered.root.root_id,
            },
            outcome: MutationOutcome::Complete(Ok(RevokeRootResult { revoked: true })),
        },
    );
    assert!(state_file::validate_loaded_state(&inconsistent_revoke).is_err());

    let mut inconsistent_register = state;
    let record = inconsistent_register
        .mutations
        .values_mut()
        .find(|record| matches!(record, MutationRecord::Register { .. }))
        .unwrap();
    let MutationRecord::Register {
        outcome: MutationOutcome::Complete(Ok(result)),
        ..
    } = record
    else {
        panic!("expected successful registration receipt")
    };
    result.root.root_id = RootId::new();
    assert!(state_file::validate_loaded_state(&inconsistent_register).is_err());
}

#[test]
fn persisted_attachments_must_be_unique_and_match_subject_grants() {
    let (_temp, broker, path, _state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path,
        OperationId::new(),
    );
    let state = broker.shared.state.lock().unwrap().clone();

    let mut duplicate = state.clone();
    duplicate.attachments.push(duplicate.attachments[0]);
    assert!(state_file::validate_loaded_state(&duplicate).is_err());

    let mut wrong_conversation = state;
    wrong_conversation.attachments[0] =
        RootAttachment::new(Uuid::new_v4(), wrong_conversation.attachments[0].root_id()).unwrap();
    assert!(state_file::validate_loaded_state(&wrong_conversation).is_err());

    let mut pending = broker.shared.state.lock().unwrap().clone();
    let root_id = pending.attachments[0].root_id();
    pending.mutations.insert(
        OperationId::new(),
        MutationRecord::Attachment {
            request: AttachmentFingerprint {
                subject: GrantSubject::conversation(conversation).unwrap(),
                conversation_id: conversation,
                root_id,
                mutation: RootAttachmentMutationKind::Detach,
                consent_method: None,
            },
            outcome: MutationOutcome::Pending,
        },
    );
    assert!(state_file::validate_loaded_state(&pending).is_err());
}

#[test]
fn persisted_attachment_ledger_rejects_unknown_nested_fields() {
    let conversation_id = Uuid::new_v4();
    let record = MutationRecord::Attachment {
        request: AttachmentFingerprint {
            subject: GrantSubject::conversation(conversation_id).unwrap(),
            conversation_id,
            root_id: RootId::new(),
            mutation: RootAttachmentMutationKind::Attach,
            consent_method: Some(ConsentMethod::PermissionDialog),
        },
        outcome: MutationOutcome::Pending,
    };
    let mut encoded = serde_json::to_value(record).unwrap();
    let mut legacy = encoded.clone();
    legacy["Attachment"]["request"]
        .as_object_mut()
        .unwrap()
        .remove("consent_method");
    assert!(matches!(
        serde_json::from_value::<MutationRecord>(legacy).unwrap(),
        MutationRecord::Attachment {
            request: AttachmentFingerprint {
                consent_method: None,
                ..
            },
            ..
        }
    ));
    encoded["Attachment"]["request"]["unexpected"] = serde_json::Value::Bool(true);
    assert!(serde_json::from_value::<MutationRecord>(encoded).is_err());
}

#[test]
fn negative_revoke_receipt_is_valid_when_the_subject_did_not_own_the_root() {
    let mut state = State::default();
    state.mutations.insert(
        OperationId::new(),
        MutationRecord::Revoke {
            request: RevokeFingerprint {
                subject: GrantSubject::conversation(Uuid::new_v4()).unwrap(),
                root_id: RootId::new(),
            },
            outcome: MutationOutcome::Complete(Ok(RevokeRootResult { revoked: false })),
        },
    );
    assert!(state_file::validate_loaded_state(&state).is_ok());
}

#[test]
fn transient_root_open_failures_remain_retryable() {
    let error = BrokerError::RootPolicy(RootPolicyError::Io(io::Error::from(
        io::ErrorKind::WouldBlock,
    )));
    assert!(retryable_registration_error(&error));
    let response = error_response(error);
    assert_eq!(response.code, ErrorCode::HostIo);
    assert!(response.retryable);
}

#[test]
fn restart_rejects_an_oversized_state_file_before_parsing_it() {
    let (temp, broker, _path, state_dir) = durable_setup();
    drop(broker);
    std::fs::write(
        state_dir.join("host-broker-state.json"),
        vec![b' '; state_file::MAX_STATE_FILE_BYTES + 1],
    )
    .unwrap();

    assert!(matches!(
        Broker::open(test_policy(&temp), &state_dir),
        Err(BrokerError::StateTooLarge)
    ));
}

#[test]
fn version_two_read_grants_migrate_without_gaining_write() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    drop(broker);

    // Reshape the persisted file into what a version 2 install left behind:
    // read grants only, before write grants existed.
    let state_path = state_dir.join("host-broker-state.json");
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    persisted["version"] = serde_json::json!(2);
    persisted["grants"]
        .as_array_mut()
        .unwrap()
        .retain(|grant| grant["capability"] != serde_json::json!("write_files"));
    std::fs::write(&state_path, serde_json::to_vec(&persisted).unwrap()).unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let state = broker.shared.state.lock().unwrap();
    assert!(state
        .grants
        .iter()
        .any(|grant| grant.capability() == Capability::ReadFiles));
    assert!(!state
        .grants
        .iter()
        .any(|grant| grant.capability() == Capability::WriteFiles));
}

/// Folders attached before exec had its own capability keep working, and say
/// how they got it.
///
/// Under version 3 a folder was resolved for commands off its read grant, so
/// every attached folder was already exec-reachable. Dropping that on upgrade
/// would break folders people are working in, and the record the migration
/// writes must not pretend they approved a prompt they never saw.
#[test]
fn version_three_read_grants_carry_their_exec_reach_forward() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let root_id = registered.root.root_id;
    drop(broker);

    // Reshape the persisted file into what a version 3 install left behind:
    // list, read, and write grants, before exec had a capability of its own.
    let state_path = state_dir.join("host-broker-state.json");
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    persisted["version"] = serde_json::json!(3);
    let grants = persisted["grants"].as_array_mut().unwrap();
    grants.retain(|grant| grant["capability"] != serde_json::json!("execute_commands"));
    let read_granted_at = grants
        .iter()
        .find(|grant| grant["capability"] == serde_json::json!("read_files"))
        .map(|grant| grant["consent"]["granted_at"].clone())
        .unwrap();
    std::fs::write(&state_path, serde_json::to_vec(&persisted).unwrap()).unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let context = ExecutionContext::standalone(conversation).unwrap();
    assert_eq!(
        resolve_exec(&broker.controller(), context, vec![root_id])
            .unwrap()
            .len(),
        1,
        "a folder that commands could already reach still reaches them"
    );

    let state = broker.shared.state.lock().unwrap();
    let carried = state
        .grants
        .iter()
        .find(|grant| grant.capability() == Capability::ExecuteCommands)
        .expect("the migration named the reach the read grant already carried");
    assert_eq!(carried.consent().method(), ConsentMethod::CarriedForward);
    assert_eq!(
        serde_json::to_value(carried.consent().granted_at()).unwrap(),
        read_granted_at,
        "the carried grant keeps the source consent's moment instead of claiming a new one"
    );
}

/// Every install on disk today wrote a version 4 file, and none of them
/// mentions a settled position. Refusing that file is a broker that will not
/// start and a user with no folders at all, so the accepted set has to widen
/// rather than shift — and the record has to be recovered from what such a file
/// does carry, including for the folder narrowed to nothing that this whole
/// change exists for. That folder has no grant left to recover from; its
/// attachment and its registration are the evidence, and they are the same
/// evidence the loader's own validation accepts.
#[test]
fn version_four_files_load_and_recover_their_settled_positions() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let root_id = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    )
    .root
    .root_id;
    drop(broker);

    // Reshape the persisted file into what a version 4 install left behind.
    let state_path = state_dir.join("host-broker-state.json");
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    persisted["version"] = serde_json::json!(4);
    persisted.as_object_mut().unwrap().remove("settled");
    std::fs::write(&state_path, serde_json::to_vec(&persisted).unwrap()).unwrap();

    // An ordinary version 4 file — the one every install has — still loads and
    // still reaches its folder.
    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    assert!(operate(
        &broker.operator(),
        ExecutionContext::standalone(conversation).unwrap(),
        OperationRequest::ReadFile(PathRequest {
            root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .is_ok());
    drop(broker);

    // Now the folder a version 4 install had revoked down to nothing: the
    // attachment and the registration stand, every grant naming the folder is
    // gone, and no position was ever recorded because the field did not exist.
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    persisted["version"] = serde_json::json!(4);
    persisted.as_object_mut().unwrap().remove("settled");
    persisted["grants"] = serde_json::json!([]);
    std::fs::write(&state_path, serde_json::to_vec(&persisted).unwrap()).unwrap();

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let controller = broker.controller();
    assert!(
        mutate_attachment(
            &controller,
            OperationId::new(),
            subject,
            conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .is_ok(),
        "the folder is still approved, so attaching it is not an error"
    );
    assert!(
        grant_statements(&controller)
            .into_iter()
            .all(|grant| !matches!(
                grant.scope,
                Scope::Root { root_id: granted } if granted == root_id
            )),
        "an upgraded install must not treat an emptied position as a first arrival"
    );
}

#[test]
fn a_current_version_file_is_not_reinterpreted_on_load() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    drop(broker);

    // A current-version file says for itself which positions are settled. The
    // reconstruction that reads positions out of attachments and registrations
    // applies only to files that predate the record; if it ran here it would
    // stand in for the evidence the validation rules look for, and this file —
    // an attachment with no grant behind it and no recorded position — would be
    // accepted instead of refused.
    let state_path = state_dir.join("host-broker-state.json");
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    persisted["grants"] = serde_json::json!([]);
    persisted["settled"] = serde_json::json!([]);
    std::fs::write(&state_path, serde_json::to_vec(&persisted).unwrap()).unwrap();

    assert!(
        Broker::open(test_policy(&temp), &state_dir).is_err(),
        "a current-version file must be validated as written, not reinterpreted"
    );
}

#[test]
fn binary_reads_return_bytes_that_text_reads_refuse() {
    let (_temp, broker, path, audit) = audited_setup();
    // A minimal PDF header followed by a byte sequence that is not valid UTF-8.
    let document: Vec<u8> = [b"%PDF-1.7\n".as_slice(), &[0xff, 0xfe, 0x00, 0x80]].concat();
    std::fs::write(path.join("report.pdf"), &document).unwrap();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let request = || PathRequest {
        root_id: registered.root.root_id,
        path: RelativePath::parse("report.pdf").unwrap(),
    };

    assert!(matches!(
        operate(
            &broker.operator(),
            context,
            OperationRequest::ReadFile(request()),
        ),
        Err(ErrorResponse {
            code: ErrorCode::UnsupportedContent,
            ..
        })
    ));
    let result = operate(
        &broker.operator(),
        context,
        OperationRequest::ReadFileBinary(request()),
    )
    .unwrap();
    let OperationResult::ReadFileBinary(result) = result else {
        panic!("expected binary content")
    };
    assert_eq!(result.bytes, document.len());
    assert_eq!(BASE64.decode(&result.content_base64).unwrap(), document);
    // Content must not leak through Debug, which is where audit and log
    // formatting would otherwise pick it up.
    assert!(!format!("{result:?}").contains(&result.content_base64));

    let events = audit.events.lock().unwrap();
    let recorded = events
        .iter()
        .find(|event| event.operation == AuditOperation::ReadFileBinary)
        .expect("binary reads are audited under their own operation name");
    assert_eq!(recorded.capability, Some(Capability::ReadFiles));
    assert_eq!(recorded.outcome, AuditOutcome::Allowed);
    assert_eq!(recorded.bytes, Some(document.len()));
}

#[test]
fn binary_reads_are_bounded_and_fenced_by_revocation() {
    let (_temp, broker, path) = setup();
    std::fs::write(
        path.join("huge.bin"),
        vec![0u8; MAX_READ_FILE_BINARY_BYTES + 1],
    )
    .unwrap();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let binary_request = |name: &str| {
        OperationRequest::ReadFileBinary(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse(name).unwrap(),
        })
    };

    assert!(matches!(
        operate(&broker.operator(), context, binary_request("huge.bin")),
        Err(ErrorResponse {
            code: ErrorCode::TooLarge,
            ..
        })
    ));
    // A directory is not a document, and the larger bound must not relax that.
    assert!(matches!(
        operate(&broker.operator(), context, binary_request("reports")),
        Err(ErrorResponse { .. })
    ));
    assert!(operate(&broker.operator(), context, binary_request("note.txt")).is_ok());

    revoke(
        &broker.controller(),
        OperationId::new(),
        subject,
        registered.root.root_id,
    );
    assert!(matches!(
        operate(&broker.operator(), context, binary_request("note.txt")),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));
}

#[test]
fn writes_create_without_clobber_and_retry_from_the_terminal_receipt() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let operation_id = OperationId::new();
    let request = write_request(
        operation_id,
        registered.root.root_id,
        "published/report.txt",
        WriteFileMode::Create,
        None,
        b"authoritative revision",
    );
    std::fs::create_dir(path.join("published")).unwrap();

    let first = operate(&broker.operator(), context, request.clone()).unwrap();
    assert_eq!(
        first,
        OperationResult::WriteFile(WriteFileResult {
            operation_id,
            bytes: 22,
            replaced: false,
        })
    );
    assert_eq!(
        std::fs::read(path.join("published/report.txt")).unwrap(),
        b"authoritative revision"
    );
    assert_eq!(
        operate(&broker.operator(), context, request).unwrap(),
        first,
        "the exact retry returns the durable receipt"
    );

    assert!(matches!(
        operate(
            &broker.operator(),
            context,
            write_request(
                OperationId::new(),
                registered.root.root_id,
                "published/report.txt",
                WriteFileMode::Create,
                None,
                b"different bytes",
            ),
        ),
        Err(ErrorResponse {
            code: ErrorCode::AlreadyExists,
            ..
        })
    ));
    assert_eq!(
        std::fs::read(path.join("published/report.txt")).unwrap(),
        b"authoritative revision",
        "create mode never clobbers the destination"
    );
}

/// The capability set a listing reports is what the broker will actually allow.
///
/// The desktop renders this set as a folder's access state, so the two must not
/// be able to drift: reporting a capability the next operation refuses, or
/// hiding one it would permit, both mislead the person deciding what the agent
/// can reach. Dropping the write grant is the only way to reach a read-only
/// folder today — registration always mints both — so it stands in for a
/// narrower grant the ladder may later offer.
#[test]
fn a_listing_reports_the_capabilities_the_broker_would_authorize() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let root_id = registered.root.root_id;
    std::fs::create_dir(path.join("published")).unwrap();

    assert_eq!(
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
        OperationResult::ListRoots {
            roots: vec![picker_access(registered.root)]
        },
        "a folder connected through the picker allows reading and writing"
    );

    broker
        .shared
        .state
        .lock()
        .unwrap()
        .grants
        .retain(|grant| grant.capability() != Capability::WriteFiles);

    let OperationResult::ListRoots { roots } =
        operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap()
    else {
        panic!("unexpected listing result")
    };
    assert_eq!(
        roots.first().map(|root| root.capabilities.as_slice()),
        Some([Capability::ReadFiles, Capability::ExecuteCommands].as_slice()),
        "write is not reported once the grant behind it is gone"
    );
    assert!(matches!(
        operate(
            &broker.operator(),
            context,
            write_request(
                OperationId::new(),
                root_id,
                "published/report.txt",
                WriteFileMode::Create,
                None,
                b"unauthorized revision",
            ),
        ),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));
    assert!(!path.join("published/report.txt").exists());
}

#[test]
fn exec_root_resolution_intersects_product_ids_with_live_grants() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let root_id = registered.root.root_id;

    assert_eq!(
        resolve_exec(&broker.controller(), context, vec![root_id]).unwrap(),
        vec![ResolvedExecRoot {
            root_id,
            path: std::fs::canonicalize(&path).unwrap(),
            writable: true,
        }]
    );

    broker
        .shared
        .state
        .lock()
        .unwrap()
        .grants
        .retain(|grant| grant.capability() != Capability::WriteFiles);
    assert!(!resolve_exec(&broker.controller(), context, vec![root_id]).unwrap()[0].writable);

    broker
        .shared
        .state
        .lock()
        .unwrap()
        .grants
        .retain(|grant| grant.capability() != Capability::ExecuteCommands);
    assert!(
        resolve_exec(&broker.controller(), context, vec![root_id])
            .unwrap()
            .is_empty(),
        "a readable folder is not reachable by commands on its own"
    );

    // Restore exec reach alone: revoking read has to take the shell with it,
    // because a command in the folder can read everything the read grant
    // covered.
    broker
        .shared
        .state
        .lock()
        .unwrap()
        .grants
        .push(exec_grant(subject, root_id));
    assert_eq!(
        resolve_exec(&broker.controller(), context, vec![root_id])
            .unwrap()
            .len(),
        1
    );
    broker
        .shared
        .state
        .lock()
        .unwrap()
        .grants
        .retain(|grant| grant.capability() != Capability::ReadFiles);
    assert!(
        resolve_exec(&broker.controller(), context, vec![root_id])
            .unwrap()
            .is_empty(),
        "a product root id is not authority after its live read grant is gone"
    );
}

fn exec_grant(subject: GrantSubject, root_id: RootId) -> Grant {
    Grant::from_consent(
        GrantId::new(),
        subject,
        Capability::ExecuteCommands,
        Scope::Root { root_id },
        ConsentRecord::new(ConsentMethod::PermissionDialog, Utc::now()),
    )
    .unwrap()
}

/// A registered root must never hand its path to exec once something else has
/// taken that path. Unix permits the rename that stages this, so the broker
/// closes the gap itself by re-confirming the directory's identity before the
/// path leaves it.
#[cfg(unix)]
#[test]
fn exec_root_resolution_rejects_a_replaced_registered_path() {
    let (temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path.clone(),
        OperationId::new(),
    );
    let moved = temp.path().join("moved-documents");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::create_dir(&path).unwrap();

    let error = resolve_exec(
        &broker.controller(),
        ExecutionContext::standalone(conversation).unwrap(),
        vec![registered.root.root_id],
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::HostIo);
}

/// The same invariant on Windows, enforced a layer lower: the rename that the
/// Unix test performs cannot happen at all while the root is pinned, because
/// cap-std opens directories without `FILE_SHARE_DELETE` precisely so a
/// registered directory cannot be renamed or deleted underneath it. Asserting
/// the refusal is what keeps that guarantee from being lost in a dependency
/// bump — if the share mode ever widened, the rename would start succeeding
/// here and the identity re-check would become load-bearing on Windows too.
#[cfg(windows)]
#[test]
fn a_registered_root_cannot_be_replaced_underneath_its_pinned_handle() {
    let (temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path.clone(),
        OperationId::new(),
    );

    let moved = temp.path().join("moved-documents");
    let denied = std::fs::rename(&path, &moved).unwrap_err();
    // ERROR_SHARING_VIOLATION
    assert_eq!(denied.raw_os_error(), Some(32));

    let roots = resolve_exec(
        &broker.controller(),
        ExecutionContext::standalone(conversation).unwrap(),
        vec![registered.root.root_id],
    )
    .unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].root_id, registered.root.root_id);
}

#[test]
fn replacement_requires_native_approval_and_fails_closed_on_symlinks_and_revoke() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let root_id = registered.root.root_id;

    assert!(matches!(
        operate(
            &broker.operator(),
            context,
            write_request(
                OperationId::new(),
                root_id,
                "note.txt",
                WriteFileMode::Replace,
                None,
                b"replacement",
            ),
        ),
        Err(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            ..
        })
    ));
    assert_eq!(
        std::fs::read(path.join("note.txt")).unwrap(),
        b"hello from broker"
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(path.join("note.txt"), path.join("link.txt")).unwrap();
        assert!(operate(
            &broker.operator(),
            context,
            write_request(
                OperationId::new(),
                root_id,
                "link.txt",
                WriteFileMode::Replace,
                Some(WriteApproval {
                    approval_id: Uuid::new_v4(),
                }),
                b"replacement",
            ),
        )
        .is_err());
        assert_eq!(
            std::fs::read(path.join("note.txt")).unwrap(),
            b"hello from broker"
        );
    }

    revoke(&broker.controller(), OperationId::new(), subject, root_id);
    assert!(matches!(
        operate(
            &broker.operator(),
            context,
            write_request(
                OperationId::new(),
                root_id,
                "new.txt",
                WriteFileMode::Create,
                None,
                b"never written",
            ),
        ),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));
    assert!(!path.join("new.txt").exists());
}

#[test]
fn pending_write_recovery_never_replays_an_ambiguous_native_result() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    );
    let context = ExecutionContext::standalone(conversation).unwrap();
    let operation_id = OperationId::new();
    let request = WriteFileRequest {
        operation_id,
        root_id: registered.root.root_id,
        path: RelativePath::parse("note.txt").unwrap(),
        mode: WriteFileMode::Replace,
        approval: Some(WriteApproval {
            approval_id: Uuid::new_v4(),
        }),
        content_base64: BASE64.encode(b"expected replacement"),
        bytes: 20,
        sha256: Sha256::digest(b"expected replacement").into(),
    };
    let fingerprint = WriteFingerprint {
        context,
        root_id: request.root_id,
        path: request.path.clone(),
        mode: request.mode,
        approval_id: request.approval.map(|approval| approval.approval_id),
        byte_len: request.bytes,
        sha256: request.sha256,
    };
    broker.shared.state.lock().unwrap().mutations.insert(
        operation_id,
        MutationRecord::Write {
            request: fingerprint,
            outcome: MutationOutcome::Pending,
        },
    );

    for _ in 0..2 {
        assert!(matches!(
            operate(
                &broker.operator(),
                context,
                OperationRequest::WriteFile(request.clone()),
            ),
            Err(ErrorResponse {
                code: ErrorCode::AmbiguousWrite,
                ..
            })
        ));
    }
}

/// The app-folder trio (docs/folder-bindings.md in the product repo) is a
/// trusted-host surface with the broker's host-level half intact: only a
/// live registration answers, transfers keep the byte bounds, and writes are
/// digest-bound and mode-checked — while no conversation context applies at
/// all, because consent lives in the caller's app grant. Every operation
/// lands in the audit trail under the app actor, with writes recording a
/// durable intent first.
#[test]
fn app_folder_trio_serves_live_registrations_and_dies_with_the_registration() {
    let (_temp, broker, root, audit) = audited_setup();
    std::fs::create_dir(root.join("reports")).unwrap();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let app_id = crate::AppId::new();
    let registered = register(
        &controller,
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        root,
        OperationId::new(),
    );
    let root_id = registered.root.root_id;
    let control = |request: ControlRequest| {
        unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request,
        }))
    };

    // Listing the root and reading a file need no conversation context.
    let ControlResult::ListAppFolder { entries } =
        control(ControlRequest::ListAppFolder(AppFolderPathRequest {
            app_id,
            root_id,
            path: RelativePath::root(),
        }))
        .unwrap()
    else {
        panic!("unexpected control result")
    };
    assert!(entries
        .iter()
        .any(|entry| entry.name == "note.txt" && entry.kind == EntryKind::File));
    assert!(entries
        .iter()
        .any(|entry| entry.name == "reports" && entry.kind == EntryKind::Directory));

    let ControlResult::ReadAppFolderFile(read) =
        control(ControlRequest::ReadAppFolderFile(AppFolderPathRequest {
            app_id,
            root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }))
        .unwrap()
    else {
        panic!("unexpected control result")
    };
    assert_eq!(
        BASE64.decode(read.content_base64).unwrap(),
        b"hello from broker"
    );

    // Writes are digest-bound and mode-checked: create lands, a same-content
    // create retry reconciles, a different-content create refuses, and
    // replace replaces.
    let write = |mode: WriteFileMode, bytes: &[u8]| {
        control(ControlRequest::WriteAppFolderFile(AppFolderWriteRequest {
            app_id,
            root_id,
            path: RelativePath::parse("app-state.json").unwrap(),
            mode,
            content_base64: BASE64.encode(bytes),
            bytes: bytes.len(),
            sha256: Sha256::digest(bytes).into(),
        }))
    };
    let ControlResult::WriteAppFolderFile { bytes, replaced } =
        write(WriteFileMode::Create, b"{\"cards\":1}").unwrap()
    else {
        panic!("unexpected control result")
    };
    assert_eq!((bytes, replaced), (11, false));
    let ControlResult::WriteAppFolderFile { replaced, .. } =
        write(WriteFileMode::Create, b"{\"cards\":1}").unwrap()
    else {
        panic!("unexpected control result")
    };
    assert!(!replaced, "a same-content create retry reconciles");
    assert_eq!(
        write(WriteFileMode::Create, b"{\"cards\":2}")
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );
    let ControlResult::WriteAppFolderFile { replaced, .. } =
        write(WriteFileMode::Replace, b"{\"cards\":2}").unwrap()
    else {
        panic!("unexpected control result")
    };
    assert!(replaced);

    // A mismatched digest refuses before any I/O.
    assert_eq!(
        control(ControlRequest::WriteAppFolderFile(AppFolderWriteRequest {
            app_id,
            root_id,
            path: RelativePath::parse("tampered.json").unwrap(),
            mode: WriteFileMode::Create,
            content_base64: BASE64.encode(b"x"),
            bytes: 1,
            sha256: [0; 32],
        }))
        .unwrap_err()
        .code,
        ErrorCode::InvalidRequest
    );

    // Revoking the registration closes the whole surface: the registration
    // is the host-level gate, and nothing else answers for it.
    revoke(
        &controller,
        OperationId::new(),
        GrantSubject::conversation(conversation).unwrap(),
        root_id,
    );
    assert_eq!(
        control(ControlRequest::ReadAppFolderFile(AppFolderPathRequest {
            app_id,
            root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }))
        .unwrap_err()
        .code,
        ErrorCode::Denied
    );

    // Every folder operation above is in the trail, attributed to the app
    // actor — reads as completions, each write as a durable intent paired
    // with its completion, and the post-revocation read as a denial.
    let events = audit.events.lock().unwrap();
    let app_events = events
        .iter()
        .filter(|event| event.actor == AuditActor::App { app_id })
        .map(|event| (event.operation, event.outcome))
        .collect::<Vec<_>>();
    assert_eq!(
        app_events,
        [
            (AuditOperation::ListAppFolder, AuditOutcome::Allowed),
            (AuditOperation::ReadAppFolderFile, AuditOutcome::Allowed),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Attempted),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Allowed),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Attempted),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Allowed),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Attempted),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Failed),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Attempted),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Allowed),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Attempted),
            (AuditOperation::WriteAppFolderFile, AuditOutcome::Failed),
            (AuditOperation::ReadAppFolderFile, AuditOutcome::Denied),
        ]
    );
    let write_target = events
        .iter()
        .find(|event| event.operation == AuditOperation::WriteAppFolderFile)
        .map(|event| event.target.clone());
    assert_eq!(
        write_target,
        Some(AuditTarget::Path {
            root_id,
            relative: RelativePath::parse("app-state.json").unwrap(),
        })
    );
}

#[test]
fn purge_conversation_subject_forgets_deleted_chat_authority() {
    let (_temp, broker, path) = setup();
    let conversation = Uuid::new_v4();
    let other = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let other_subject = GrantSubject::conversation(other).unwrap();
    let registered = register(
        &broker.controller(),
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    );
    // A second conversation attached to the same root must survive the purge.
    let attach = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::AttachRoot(RootAttachmentMutationRequest {
            operation_id: OperationId::new(),
            subject: other_subject,
            conversation_id: other,
            root_id: registered.root.root_id,
            consent_method: Some(ConsentMethod::PermissionDialog),
        }),
    }))
    .unwrap();
    assert!(matches!(
        attach,
        ControlResult::AttachRoot(RootAttachmentMutationResult { changed: true, .. })
    ));

    let purge = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::PurgeConversationSubject(PurgeConversationSubjectRequest {
            conversation_id: conversation,
        }),
    }))
    .unwrap();
    assert_eq!(
        purge,
        ControlResult::PurgeConversationSubject(PurgeConversationSubjectResult { changed: true })
    );

    let grants = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::ListGrantStatements,
    }))
    .unwrap();
    let ControlResult::ListGrantStatements { grants } = grants else {
        panic!("unexpected control result");
    };
    assert!(
        grants.iter().all(|grant| grant.subject != subject),
        "deleted conversation grants must be gone: {grants:?}"
    );
    assert!(
        grants.iter().any(|grant| grant.subject == other_subject),
        "surviving conversation grants must remain: {grants:?}"
    );

    // Root stays for the other conversation.
    let context = ExecutionContext::standalone(other).unwrap();
    assert!(operate(
        &broker.operator(),
        context,
        OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        }),
    )
    .is_ok());

    let dead = ExecutionContext::standalone(conversation).unwrap();
    assert!(matches!(
        operate(
            &broker.operator(),
            dead,
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            }),
        ),
        Err(ErrorResponse {
            code: ErrorCode::Denied,
            ..
        })
    ));

    let again = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::PurgeConversationSubject(PurgeConversationSubjectRequest {
            conversation_id: conversation,
        }),
    }))
    .unwrap();
    assert_eq!(
        again,
        ControlResult::PurgeConversationSubject(PurgeConversationSubjectResult { changed: false })
    );
}

/// Attaching a folder says where it may be used, not what may be done in it.
/// A capability withdrawn on the folders panel has to survive every route back
/// to the same folder — a redundant attach, a detach and re-attach, choosing
/// the same folder in the picker again, and a sibling chat under the same
/// project subject connecting it for the first time — because none of those
/// asks the user the question the revocation answered. A subject that has
/// never held the folder still gets what a fresh pick grants.
#[test]
fn attaching_a_folder_never_restores_a_revoked_capability() {
    let (_temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let root_id = register(
        &controller,
        subject,
        conversation,
        path.clone(),
        OperationId::new(),
    )
    .root
    .root_id;
    let capabilities = |held_by: GrantSubject| {
        let mut capabilities = grant_statements(&controller)
            .into_iter()
            .filter(|grant| {
                grant.subject == held_by
                    && matches!(grant.scope, Scope::Root { root_id: granted } if granted == root_id)
            })
            .map(|grant| grant.capability)
            .collect::<Vec<_>>();
        capabilities.sort_by_key(|capability| format!("{capability:?}"));
        capabilities
    };
    let attach = |operation_id| {
        mutate_attachment(
            &controller,
            operation_id,
            subject,
            conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap()
    };

    let write_id = grant_statements(&controller)
        .into_iter()
        .find(|grant| grant.capability == Capability::WriteFiles)
        .unwrap()
        .grant_id;
    assert!(revoke_grant(&controller, subject, write_id));
    let narrowed = capabilities(subject);
    assert!(!narrowed.contains(&Capability::WriteFiles));

    // A redundant attach reports no change and changes no authority.
    assert!(!attach(OperationId::new()).changed);
    assert_eq!(capabilities(subject), narrowed);

    // Nor does taking the folder out of the chat and putting it back.
    assert!(
        mutate_attachment(
            &controller,
            OperationId::new(),
            subject,
            conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        )
        .unwrap()
        .changed
    );
    assert!(attach(OperationId::new()).changed);
    assert_eq!(capabilities(subject), narrowed);

    // Nor does choosing the same folder in the picker again, which lands on
    // the existing registration.
    assert_eq!(
        register(
            &controller,
            subject,
            conversation,
            path.clone(),
            OperationId::new()
        )
        .root
        .root_id,
        root_id
    );
    assert_eq!(capabilities(subject), narrowed);

    // Revoking the rest leaves no statement behind — a withdrawn grant is
    // indistinguishable from one that never existed — so the folder still
    // being in this chat is what has to carry the decision. Picking it again
    // lands on the same registration and mints nothing, exec least of all.
    let read_id = grant_statements(&controller)
        .into_iter()
        .find(|grant| grant.subject == subject && grant.capability == Capability::ReadFiles)
        .unwrap()
        .grant_id;
    assert!(revoke_grant(&controller, subject, read_id));
    assert!(capabilities(subject).is_empty());
    assert_eq!(
        register(
            &controller,
            subject,
            conversation,
            path.clone(),
            OperationId::new()
        )
        .root
        .root_id,
        root_id
    );
    assert!(
        capabilities(subject).is_empty(),
        "re-picking an attached folder must not refill an emptied position: {:?}",
        capabilities(subject)
    );
    assert!(!attach(OperationId::new()).changed);
    assert!(capabilities(subject).is_empty());

    // Taking the emptied folder out of the chat and putting it back is the
    // same non-question. Disconnecting no longer needs read — a folder
    // narrowed to nothing must still be removable — so this route is open in
    // a way it was not when the read check stood in for the rule.
    assert!(
        mutate_attachment(
            &controller,
            OperationId::new(),
            subject,
            conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        )
        .unwrap()
        .changed,
        "a folder that allows nothing must still detach"
    );
    assert!(attach(OperationId::new()).changed);
    assert!(
        capabilities(subject).is_empty(),
        "detaching and re-attaching must not refill an emptied position: {:?}",
        capabilities(subject)
    );

    // A conversation with no standing position on this folder is a first
    // grant, not a widening, and still gets the access a pick describes.
    let newcomer = Uuid::new_v4();
    let newcomer_subject = GrantSubject::conversation(newcomer).unwrap();
    assert!(
        mutate_attachment(
            &controller,
            OperationId::new(),
            newcomer_subject,
            newcomer,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap()
        .changed
    );
    assert!(capabilities(newcomer_subject).contains(&Capability::WriteFiles));

    // The same rule holds for a project subject, where the chat that connects
    // the folder and the subject that holds the access are different entities.
    // A sibling chat attaching for the first time is a first arrival for that
    // chat and not for the project, so it must not restore what the project's
    // other chat revoked.
    let project = GrantSubject::project(Uuid::new_v4()).unwrap();
    let first_chat = Uuid::new_v4();
    let sibling_chat = Uuid::new_v4();
    assert!(
        mutate_attachment(
            &controller,
            OperationId::new(),
            project,
            first_chat,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap()
        .changed
    );
    assert!(capabilities(project).contains(&Capability::WriteFiles));
    for grant_id in grant_statements(&controller)
        .into_iter()
        .filter(|grant| {
            grant.subject == project
                && matches!(grant.scope, Scope::Root { root_id: granted } if granted == root_id)
        })
        .map(|grant| grant.grant_id)
        .collect::<Vec<_>>()
    {
        revoke_grant(&controller, project, grant_id);
    }
    assert!(capabilities(project).is_empty());
    assert!(
        mutate_attachment(
            &controller,
            OperationId::new(),
            project,
            sibling_chat,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap()
        .changed,
        "the sibling chat still gets the folder, it just gets no access with it"
    );
    assert!(
        capabilities(project).is_empty(),
        "a sibling chat's attach must not restore the project's revoked access: {:?}",
        capabilities(project)
    );
}

/// A revoked position has to outlive the process that revoked it. The record
/// that says "this subject has already been given this folder" is the only
/// thing standing between an emptied folder and a re-mint, so a restart that
/// dropped it would hand the access back on the next attach.
#[test]
fn an_emptied_folder_position_survives_a_restart() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let root_id = register(
        &broker.controller(),
        subject,
        conversation,
        path,
        OperationId::new(),
    )
    .root
    .root_id;
    for grant_id in grant_statements(&broker.controller())
        .into_iter()
        .filter(
            |grant| matches!(grant.scope, Scope::Root { root_id: granted } if granted == root_id),
        )
        .map(|grant| grant.grant_id)
        .collect::<Vec<_>>()
    {
        revoke_grant(&broker.controller(), subject, grant_id);
    }
    drop(broker);

    let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let controller = broker.controller();
    assert!(
        mutate_attachment(
            &controller,
            OperationId::new(),
            subject,
            conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .is_ok(),
        "the folder is still approved, so attaching it is not an error"
    );
    assert!(
        grant_statements(&controller)
            .into_iter()
            .all(|grant| !matches!(
                grant.scope,
                Scope::Root { root_id: granted } if granted == root_id
            )),
        "a restart must not turn an emptied position back into a first arrival"
    );
}

/// Retry and receipt records are recovery state, not history: the broker keeps
/// a bounded window of the completed ones so a long-lived install cannot grow
/// its state file past the size that would stop it saving consent. Records the
/// loader reads against each other — registration and revocation — are never
/// dropped, and a record still inside the window answers a retry exactly as it
/// did before.
#[test]
fn completed_receipts_are_bounded_without_breaking_retry_or_receipt_lookup() {
    let (_temp, broker, path) = setup();
    let controller = broker.controller();
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let registration = OperationId::new();
    let root_id = register(&controller, subject, conversation, path, registration)
        .root
        .root_id;
    let cycle = |operation_id, mutation| {
        mutate_attachment(
            &controller,
            operation_id,
            subject,
            conversation,
            root_id,
            mutation,
        )
        .unwrap()
    };

    let oldest = OperationId::new();
    cycle(oldest, RootAttachmentMutationKind::Detach);
    let mut newest = oldest;
    for index in 0..MAX_RETAINED_MUTATION_RECEIPTS {
        newest = OperationId::new();
        cycle(
            newest,
            if index % 2 == 0 {
                RootAttachmentMutationKind::Detach
            } else {
                RootAttachmentMutationKind::Attach
            },
        );
    }

    let retained = broker.shared.state.lock().unwrap().mutations.len();
    assert!(
        retained <= MAX_RETAINED_MUTATION_RECEIPTS + 1,
        "mutation records must stay bounded, kept {retained}"
    );

    // The oldest attachment record is gone, and its receipt says so rather
    // than claiming an outcome the broker no longer holds.
    let receipt = |operation_id, mutation| {
        lookup_attachment_receipt(
            &controller,
            LookupRootAttachmentReceiptRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                root_id,
                mutation,
            },
        )
        .unwrap()
    };
    assert_eq!(
        receipt(oldest, RootAttachmentMutationKind::Detach),
        RootAttachmentMutationReceipt::Unknown
    );

    // The newest is still answerable, and retrying it replays the recorded
    // outcome instead of running a second mutation. The run ends attached, so
    // the last mutation is the attach half of the cycle.
    let mutation = RootAttachmentMutationKind::Attach;
    assert!(matches!(
        receipt(newest, mutation),
        RootAttachmentMutationReceipt::Completed { .. }
    ));
    assert!(cycle(newest, mutation).changed);

    // Registration receipts outlive any amount of later traffic.
    assert!(matches!(
        lookup_register_receipt(&controller, registration, subject, conversation)
            .unwrap()
            .receipt,
        RegisterRootReceipt::Completed { .. }
    ));
}

// ---------------------------------------------------------------------------
// Computer use: blocklist, grant implication at dispatch, the consequential
// gate, audit-before-act, capture handoff, and the bounded wait.
// ---------------------------------------------------------------------------

use crate::computer_use::{
    AxTree, CaptureMeta, ElementDescription, PermissionStatus, WindowFrame, WindowInfo,
};
use crate::{CuConfirmControlActionRequest, CuListAppGrantsRequest, CuResolveHandoffRequest};

/// A scripted backend: the broker's policy is what is under test, so the
/// "native" side records what it was asked to do and answers from canned
/// state. `describe` is what the target element currently reports.
#[derive(Default)]
struct StubCuBackend {
    available: bool,
    description: Mutex<ElementDescription>,
    clicked: Mutex<Vec<(String, Option<String>)>>,
    typed: Mutex<Vec<String>>,
    keys: Mutex<Vec<String>>,
    scrolled: Mutex<Vec<String>>,
    focused: Mutex<Vec<String>>,
    capture_png: Vec<u8>,
}

impl StubCuBackend {
    fn available() -> Arc<Self> {
        Arc::new(Self {
            available: true,
            description: Mutex::new(ElementDescription {
                role: Some("AXButton".to_owned()),
                label: Some("Cancel".to_owned()),
                fingerprint: None,
            }),
            capture_png: vec![0x89, 0x50, 0x4e, 0x47],
            ..Default::default()
        })
    }

    fn set_label(&self, label: &str) {
        self.description.lock().unwrap().label = Some(label.to_owned());
    }

    /// Give the described element a fingerprint, as if it resolved to a real
    /// AX element with a stable identity.
    fn set_fingerprint(&self, fingerprint: &str) {
        self.description.lock().unwrap().fingerprint = Some(fingerprint.to_owned());
    }

    fn clicks(&self) -> Vec<(String, Option<String>)> {
        self.clicked.lock().unwrap().clone()
    }

    fn keys(&self) -> Vec<String> {
        self.keys.lock().unwrap().clone()
    }

    fn scrolls(&self) -> Vec<String> {
        self.scrolled.lock().unwrap().clone()
    }

    fn focuses(&self) -> Vec<String> {
        self.focused.lock().unwrap().clone()
    }

    /// One interactive button in a tiny AX tree, so Set-of-Marks extraction
    /// finds exactly one mark.
    fn ax_tree(&self) -> AxTree {
        AxTree {
            app_name: Some("Example".to_owned()),
            tree: serde_json::json!({
                "role": "AXWindow",
                "id": "0",
                "fingerprint": "fp0",
                "children": [{
                    "role": "AXButton",
                    "id": "0.0",
                    "fingerprint": "fp1",
                    "title": "Send",
                    "frame": { "x": 10.0, "y": 10.0, "width": 60.0, "height": 24.0 },
                    "children": [],
                }],
            }),
            truncated: false,
        }
    }
}

impl ComputerUseBackend for StubCuBackend {
    fn permission_status(&self) -> Result<PermissionStatus, BackendError> {
        Ok(PermissionStatus {
            screen_recording: true,
            accessibility: true,
        })
    }

    fn request_permissions(&self) -> Result<PermissionStatus, BackendError> {
        self.permission_status()
    }

    fn capture(
        &self,
        _target: &CaptureTarget,
        out_path: &Path,
    ) -> Result<CaptureMeta, BackendError> {
        std::fs::write(out_path, &self.capture_png).unwrap();
        Ok(CaptureMeta {
            width: 800,
            height: 600,
            media_type: "image/png".to_owned(),
        })
    }

    fn read_ax_tree(
        &self,
        _bundle_id: &str,
        _max_depth: Option<u32>,
        _max_nodes: Option<u32>,
    ) -> Result<AxTree, BackendError> {
        Ok(self.ax_tree())
    }

    fn list_windows(&self, _bundle_id: Option<&str>) -> Result<Vec<WindowInfo>, BackendError> {
        Ok(vec![WindowInfo {
            window_id: 7,
            title: Some("Inbox".to_owned()),
            app_name: Some("Example".to_owned()),
            bundle_id: Some("com.example.app".to_owned()),
            pid: 4242,
            frame: WindowFrame {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
        }])
    }

    fn click(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        _button: Option<&str>,
        _click_count: Option<u32>,
    ) -> Result<ControlMeta, BackendError> {
        self.clicked
            .lock()
            .unwrap()
            .push((bundle_id.to_owned(), target.element_id.clone()));
        Ok(ControlMeta {
            success: true,
            used_fallback: false,
            detail: None,
        })
    }

    fn type_text(
        &self,
        _bundle_id: &str,
        text: &str,
        _target: &ElementTarget,
    ) -> Result<ControlMeta, BackendError> {
        self.typed.lock().unwrap().push(text.to_owned());
        Ok(ControlMeta {
            success: true,
            used_fallback: false,
            detail: None,
        })
    }

    fn key_press(
        &self,
        _bundle_id: &str,
        key: &str,
        _modifiers: Option<&[String]>,
    ) -> Result<ControlMeta, BackendError> {
        self.keys.lock().unwrap().push(key.to_owned());
        Ok(ControlMeta {
            success: true,
            used_fallback: false,
            detail: None,
        })
    }

    fn scroll(
        &self,
        bundle_id: &str,
        _target: &ElementTarget,
        _dx: Option<f64>,
        _dy: Option<f64>,
    ) -> Result<ControlMeta, BackendError> {
        self.scrolled.lock().unwrap().push(bundle_id.to_owned());
        Ok(ControlMeta {
            success: true,
            used_fallback: false,
            detail: None,
        })
    }

    fn focus_window(
        &self,
        bundle_id: &str,
        _window_id: Option<u32>,
    ) -> Result<ControlMeta, BackendError> {
        self.focused.lock().unwrap().push(bundle_id.to_owned());
        Ok(ControlMeta {
            success: true,
            used_fallback: false,
            detail: None,
        })
    }

    fn describe_element(
        &self,
        _bundle_id: &str,
        _target: &ElementTarget,
    ) -> Result<ElementDescription, BackendError> {
        Ok(self.description.lock().unwrap().clone())
    }

    fn is_available(&self) -> bool {
        self.available
    }
}

struct CuFixture {
    _temp: tempfile::TempDir,
    broker: Broker,
    backend: Arc<StubCuBackend>,
    audit: Arc<CollectingAudit>,
    subject: GrantSubject,
    context: ExecutionContext,
}

/// A broker with a stubbed-available backend, a collecting audit sink, and a
/// staging dir under the test tempdir. No folders are registered: computer
/// use needs none.
fn cu_setup() -> CuFixture {
    let temp = tempfile::tempdir().unwrap();
    let backend = StubCuBackend::available();
    let audit = Arc::new(CollectingAudit::default());
    let broker = Broker::test_with_computer_use(
        test_policy(&temp),
        audit.clone(),
        backend.clone(),
        temp.path().join("cu-staging"),
    );
    let conversation = Uuid::new_v4();
    CuFixture {
        _temp: temp,
        broker,
        backend,
        audit,
        subject: GrantSubject::conversation(conversation).unwrap(),
        context: ExecutionContext::standalone(conversation).unwrap(),
    }
}

impl CuFixture {
    fn control(&self, request: ControlRequest) -> Result<ControlResult, ErrorResponse> {
        unwrap_response(self.broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request,
        }))
    }

    fn operate(&self, request: OperationRequest) -> Result<OperationResult, ErrorResponse> {
        operate(&self.broker.operator(), self.context, request)
    }

    fn grant(&self, capability: Capability, bundle_id: Option<&str>) -> CuGrantAppResult {
        self.grant_with(capability, bundle_id, false)
    }

    fn grant_once(&self, capability: Capability, bundle_id: Option<&str>) -> CuGrantAppResult {
        self.grant_with(capability, bundle_id, true)
    }

    fn grant_with(
        &self,
        capability: Capability,
        bundle_id: Option<&str>,
        single_use: bool,
    ) -> CuGrantAppResult {
        let result = self
            .control(ControlRequest::CuGrantApp(CuGrantAppRequest {
                subject: self.subject,
                capability,
                bundle_id: bundle_id.map(str::to_owned),
                consent: ConsentMethod::PermissionDialog,
                single_use,
            }))
            .unwrap();
        let ControlResult::CuGrantApp(result) = result else {
            panic!("unexpected control result")
        };
        result
    }

    fn click(&self, bundle_id: &str) -> Result<OperationResult, ErrorResponse> {
        self.operate(OperationRequest::CuClick {
            bundle_id: bundle_id.to_owned(),
            target: ElementTargetWire {
                element_id: Some("0.0".to_owned()),
                element_fingerprint: Some("fp1".to_owned()),
                ..Default::default()
            },
            button: None,
            click_count: None,
        })
    }
}

#[test]
fn blocked_bundles_refuse_control_ops_and_grants_even_with_a_grant_present() {
    let fixture = cu_setup();
    // The blocklist outranks consent: mint a grant for a blocked bundle by
    // hand (the control surface refuses it, tested below) and the op still
    // refuses.
    for blocked in [
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        "com.apple.SecurityAgent",
        "io.brightwave.openwave",
        "io.brightwave.anything",
    ] {
        fixture.broker.shared.state.lock().unwrap().grants.push(
            Grant::from_consent(
                GrantId::new(),
                fixture.subject,
                Capability::ControlApp,
                Scope::App {
                    bundle_id: blocked.to_owned(),
                },
                ConsentRecord::new(ConsentMethod::PermissionDialog, Utc::now()),
            )
            .unwrap(),
        );
        let error = fixture.click(blocked).unwrap_err();
        assert_eq!(error.code, ErrorCode::Denied, "{blocked}");
        assert!(!error.retryable, "{blocked}");

        let grant_error = fixture
            .control(ControlRequest::CuGrantApp(CuGrantAppRequest {
                subject: fixture.subject,
                capability: Capability::ControlApp,
                bundle_id: Some(blocked.to_owned()),
                consent: ConsentMethod::PermissionDialog,
                single_use: false,
            }))
            .unwrap_err();
        assert_eq!(grant_error.code, ErrorCode::Denied, "{blocked}");
        assert!(!grant_error.retryable, "{blocked}");
    }
    assert!(fixture.backend.clicks().is_empty());
    // A lookalike suffix is not the blocked bundle.
    fixture.grant(Capability::ControlApp, Some("com.apple.Terminalized"));
    fixture.click("com.apple.Terminalized").unwrap();
}

#[test]
fn consequential_gate_holds_commit_labels_and_confirms_once() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));
    fixture.backend.set_label("Send");

    let result = fixture.click("com.example.app").unwrap();
    let OperationResult::CuNeedsConfirmation(held) = result else {
        panic!("expected a confirmation hold, got {result:?}")
    };
    assert_eq!(held.target_label.as_deref(), Some("Send"));
    assert!(fixture.backend.clicks().is_empty());

    // Confirming performs exactly the held action; the agent supplies no
    // parameters at confirm time.
    let confirmed = fixture
        .control(ControlRequest::CuConfirmControlAction(
            CuConfirmControlActionRequest {
                confirmation_id: held.confirmation_id,
            },
        ))
        .unwrap();
    let ControlResult::CuConfirmControlAction(meta) = confirmed else {
        panic!("unexpected control result")
    };
    assert!(meta.success);
    assert_eq!(fixture.backend.clicks().len(), 1);

    // The token is single-use.
    let replay = fixture
        .control(ControlRequest::CuConfirmControlAction(
            CuConfirmControlActionRequest {
                confirmation_id: held.confirmation_id,
            },
        ))
        .unwrap_err();
    assert_eq!(replay.code, ErrorCode::NotFound);
    assert_eq!(fixture.backend.clicks().len(), 1);
}

#[test]
fn navigation_labels_proceed_without_a_confirmation() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));
    fixture.backend.set_label("Cancel");

    let result = fixture.click("com.example.app").unwrap();
    assert!(matches!(result, OperationResult::CuClick(_)));
    assert_eq!(fixture.backend.clicks().len(), 1);
}

#[test]
fn a_commit_shaped_key_press_is_held_for_confirmation() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));

    // A chorded key (Cmd+Shift+D, "send" in Mail) is commit-shaped: it parks
    // on a confirmation and nothing is pressed yet.
    let held = fixture
        .operate(OperationRequest::CuKeyPress {
            bundle_id: "com.example.app".to_owned(),
            key: "d".to_owned(),
            modifiers: Some(vec!["cmd".to_owned(), "shift".to_owned()]),
        })
        .unwrap();
    let OperationResult::CuNeedsConfirmation(confirmation) = held else {
        panic!("expected a confirmation hold for a chorded key, got {held:?}")
    };
    assert!(fixture.backend.keys().is_empty());

    // Confirming performs exactly the held key press.
    fixture
        .control(ControlRequest::CuConfirmControlAction(
            CuConfirmControlActionRequest {
                confirmation_id: confirmation.confirmation_id,
            },
        ))
        .unwrap();
    assert_eq!(fixture.backend.keys(), ["d"]);
}

#[test]
fn a_plain_navigation_key_proceeds_without_a_confirmation() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));

    // An unmodified navigation key is not commit-shaped.
    let result = fixture
        .operate(OperationRequest::CuKeyPress {
            bundle_id: "com.example.app".to_owned(),
            key: "left".to_owned(),
            modifiers: None,
        })
        .unwrap();
    assert!(matches!(result, OperationResult::CuKeyPress(_)));
    assert_eq!(fixture.backend.keys(), ["left"]);
}

#[test]
fn a_confirmation_is_void_when_the_label_changes_under_it() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));
    fixture.backend.set_label("Send");

    let OperationResult::CuNeedsConfirmation(held) = fixture.click("com.example.app").unwrap()
    else {
        panic!("expected a confirmation hold")
    };
    // The UI shifted while the prompt was up: "Send" became something else.
    fixture.backend.set_label("Send invoice");
    let error = fixture
        .control(ControlRequest::CuConfirmControlAction(
            CuConfirmControlActionRequest {
                confirmation_id: held.confirmation_id,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleElement);
    assert!(fixture.backend.clicks().is_empty());
}

#[test]
fn a_confirmation_is_void_when_the_element_is_swapped_under_an_identical_label() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));
    fixture.backend.set_label("Send");
    fixture.backend.set_fingerprint("fp-original");

    let OperationResult::CuNeedsConfirmation(held) = fixture.click("com.example.app").unwrap()
    else {
        panic!("expected a confirmation hold")
    };
    // The app swapped the button for a different element that happens to carry
    // the same label — the classic same-label substitution. The label still
    // matches, but the fingerprint changed, so the confirmation must not act.
    fixture.backend.set_fingerprint("fp-swapped");
    let error = fixture
        .control(ControlRequest::CuConfirmControlAction(
            CuConfirmControlActionRequest {
                confirmation_id: held.confirmation_id,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleElement);
    assert!(fixture.backend.clicks().is_empty());
}

#[test]
fn control_grants_cover_reads_but_read_grants_never_cover_control() {
    let fixture = cu_setup();
    // A ControlApp grant authorizes the read ops for the same app.
    fixture.grant(Capability::ControlApp, Some("com.example.app"));
    let read = fixture
        .operate(OperationRequest::CuReadAppContent {
            bundle_id: "com.example.app".to_owned(),
            max_depth: None,
            max_nodes: None,
        })
        .unwrap();
    assert!(matches!(read, OperationResult::CuReadAppContent(_)));

    // A read-only grant does not authorize input synthesis.
    let conversation = Uuid::new_v4();
    let reader = GrantSubject::conversation(conversation).unwrap();
    let reader_context = ExecutionContext::standalone(conversation).unwrap();
    let granted = fixture
        .control(ControlRequest::CuGrantApp(CuGrantAppRequest {
            subject: reader,
            capability: Capability::ReadAppContent,
            bundle_id: Some("com.example.app".to_owned()),
            consent: ConsentMethod::PermissionDialog,
            single_use: false,
        }))
        .unwrap();
    assert!(matches!(granted, ControlResult::CuGrantApp(_)));
    let error = operate(
        &fixture.broker.operator(),
        reader_context,
        OperationRequest::CuClick {
            bundle_id: "com.example.app".to_owned(),
            target: ElementTargetWire {
                element_id: Some("0.0".to_owned()),
                ..Default::default()
            },
            button: None,
            click_count: None,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
    assert!(fixture.backend.clicks().is_empty());
}

#[test]
fn an_unrecordable_control_op_never_reaches_the_backend() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));

    let broken = Arc::new(BreakableAudit::default());
    let broker = Broker::test_with_computer_use(
        test_policy(&fixture._temp),
        broken.clone(),
        fixture.backend.clone(),
        fixture._temp.path().join("cu-staging-broken"),
    );
    // Rebuild the grant on the broken-audit broker: the intent record must
    // still be writable, so grant first, break afterwards.
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let context = ExecutionContext::standalone(conversation).unwrap();
    let granted = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::CuGrantApp(CuGrantAppRequest {
            subject,
            capability: Capability::ControlApp,
            bundle_id: Some("com.example.app".to_owned()),
            consent: ConsentMethod::PermissionDialog,
            single_use: false,
        }),
    }))
    .unwrap();
    assert!(matches!(granted, ControlResult::CuGrantApp(_)));

    broken.broken.store(true, Ordering::SeqCst);
    let error = operate(
        &broker.operator(),
        context,
        OperationRequest::CuClick {
            bundle_id: "com.example.app".to_owned(),
            target: ElementTargetWire {
                element_id: Some("0.0".to_owned()),
                ..Default::default()
            },
            button: None,
            click_count: None,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::AuditUnavailable);
    assert!(error.retryable);
    assert!(fixture.backend.clicks().is_empty());

    // The confirm path is gated the same way: a held action cannot be
    // performed while its intent cannot be recorded.
    broken.broken.store(false, Ordering::SeqCst);
    fixture.backend.set_label("Send");
    let held = operate(
        &broker.operator(),
        context,
        OperationRequest::CuClick {
            bundle_id: "com.example.app".to_owned(),
            target: ElementTargetWire {
                element_id: Some("0.0".to_owned()),
                ..Default::default()
            },
            button: None,
            click_count: None,
        },
    )
    .unwrap();
    let OperationResult::CuNeedsConfirmation(held) = held else {
        panic!("expected a confirmation hold")
    };
    broken.broken.store(true, Ordering::SeqCst);
    let error = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::CuConfirmControlAction(CuConfirmControlActionRequest {
            confirmation_id: held.confirmation_id,
        }),
    }))
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::AuditUnavailable);
    assert!(fixture.backend.clicks().is_empty());
}

#[test]
fn captures_cross_the_wire_as_a_single_use_handoff() {
    let fixture = cu_setup();
    fixture.grant(Capability::CaptureScreen, Some("com.example.app"));

    let result = fixture
        .operate(OperationRequest::CuCaptureScreen {
            target: CaptureTargetWire::App {
                bundle_id: "com.example.app".to_owned(),
            },
        })
        .unwrap();
    let OperationResult::CuCaptureScreen(capture) = result else {
        panic!("expected a capture result, got {result:?}")
    };
    assert_eq!((capture.width, capture.height), (800, 600));
    // The annotated capture carried the one interactive element as a mark.
    assert_eq!(capture.marks.len(), 1);
    assert_eq!(capture.marks[0].label, "Send");
    // The agent-channel result must not carry the pixels.
    let wire = serde_json::to_value(&capture).unwrap();
    assert!(wire.get("content_base64").is_none());
    assert!(wire.get("bytes").is_none());

    // The trusted desktop redeems the handoff once, getting the exact bytes
    // the helper wrote.
    let resolved = fixture
        .control(ControlRequest::CuResolveHandoff(CuResolveHandoffRequest {
            handoff_id: capture.handoff_id,
        }))
        .unwrap();
    let ControlResult::CuResolveHandoff(resolved) = resolved else {
        panic!("unexpected control result")
    };
    assert_eq!(
        BASE64.decode(&resolved.content_base64).unwrap(),
        vec![0x89, 0x50, 0x4e, 0x47]
    );
    assert_eq!(resolved.bytes, 4);

    let second = fixture
        .control(ControlRequest::CuResolveHandoff(CuResolveHandoffRequest {
            handoff_id: capture.handoff_id,
        }))
        .unwrap_err();
    assert_eq!(second.code, ErrorCode::NotFound);
    assert!(!second.retryable);
}

#[test]
fn computer_use_grants_list_and_revoke_roundtrip() {
    let fixture = cu_setup();
    let granted = fixture.grant(Capability::ControlApp, Some("com.example.app"));
    assert!(granted.granted);
    // Re-granting the same tuple is an idempotent no-op naming the same grant.
    let again = fixture.grant(Capability::ControlApp, Some("com.example.app"));
    assert!(!again.granted);
    assert_eq!(again.grant_id, granted.grant_id);
    // A whole-display grant is capture-only.
    let screen = fixture.grant(Capability::CaptureScreen, None);
    assert!(screen.granted);

    let listed = fixture
        .control(ControlRequest::CuListAppGrants(CuListAppGrantsRequest {
            subject: fixture.subject,
        }))
        .unwrap();
    let ControlResult::CuListAppGrants { grants } = listed else {
        panic!("unexpected control result")
    };
    assert_eq!(grants.len(), 2);
    assert!(grants.iter().any(|grant| {
        grant.capability == Capability::ControlApp
            && matches!(&grant.scope, Scope::App { bundle_id } if bundle_id == "com.example.app")
    }));
    assert!(
        grants
            .iter()
            .any(|grant| grant.capability == Capability::CaptureScreen
                && grant.scope == Scope::Screen)
    );

    // A screen grant authorizes a display capture; an app grant cannot.
    let display = fixture
        .operate(OperationRequest::CuCaptureScreen {
            target: CaptureTargetWire::Display { display_id: None },
        })
        .unwrap();
    assert!(matches!(display, OperationResult::CuCaptureScreen(_)));

    let revoked = fixture
        .control(ControlRequest::CuRevokeApp(CuRevokeAppRequest {
            subject: fixture.subject,
            capability: Capability::ControlApp,
            bundle_id: Some("com.example.app".to_owned()),
        }))
        .unwrap();
    assert!(matches!(
        revoked,
        ControlResult::CuRevokeApp { revoked: true }
    ));
    // Idempotent: the same withdrawal reports nothing left to withdraw.
    let revoked = fixture
        .control(ControlRequest::CuRevokeApp(CuRevokeAppRequest {
            subject: fixture.subject,
            capability: Capability::ControlApp,
            bundle_id: Some("com.example.app".to_owned()),
        }))
        .unwrap();
    assert!(matches!(
        revoked,
        ControlResult::CuRevokeApp { revoked: false }
    ));
    // Enforcement follows the withdrawal.
    let error = fixture.click("com.example.app").unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
}

#[test]
fn wait_is_bounded_and_never_touches_the_backend() {
    let fixture = cu_setup();
    // No grant required; a wild value clamps to the cap rather than sleeping
    // it — assert the clamp itself so the test suite never actually waits.
    assert_eq!(bounded_wait_seconds(Some(9999.0)), MAX_CU_WAIT_SECONDS);
    assert_eq!(bounded_wait_seconds(Some(-3.0)), 0.0);
    assert_eq!(bounded_wait_seconds(Some(f64::NAN)), 0.0);
    assert_eq!(bounded_wait_seconds(None), 0.0);
    let result = fixture
        .operate(OperationRequest::CuWait { seconds: Some(0.0) })
        .unwrap();
    let OperationResult::CuWait { seconds } = result else {
        panic!("expected a wait result")
    };
    assert_eq!(seconds, 0.0);
}

#[test]
fn hello_advertises_computer_use_only_when_a_backend_is_available() {
    let fixture = cu_setup();
    let hello = fixture.control(ControlRequest::Hello).unwrap();
    let ControlResult::Hello(hello) = hello else {
        panic!("unexpected control result")
    };
    for op in [
        "cu_list_windows",
        "cu_capture_screen",
        "cu_read_app_content",
        "cu_click",
        "cu_type_text",
        "cu_key_press",
        "cu_scroll",
        "cu_focus_window",
        "cu_wait",
    ] {
        assert!(
            hello.operations.iter().any(|advertised| advertised == op),
            "{op}"
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let unsupported = Broker::test_with_computer_use(
        test_policy(&temp),
        Arc::new(MemoryAuditSink::new()),
        Arc::new(UnsupportedBackend),
        temp.path().join("cu-staging"),
    );
    let hello = unwrap_response(unsupported.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::Hello,
    }))
    .unwrap();
    let ControlResult::Hello(hello) = hello else {
        panic!("unexpected control result")
    };
    assert!(!hello.operations.iter().any(|op| op.starts_with("cu_")));
}

#[test]
fn every_computer_use_op_lands_in_the_audit_trail_desensitized() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));
    fixture.backend.set_label("Cancel");
    fixture.click("com.example.app").unwrap();
    fixture
        .operate(OperationRequest::CuKeyPress {
            bundle_id: "com.example.app".to_owned(),
            key: "return".to_owned(),
            modifiers: None,
        })
        .unwrap();
    fixture
        .operate(OperationRequest::CuCaptureScreen {
            target: CaptureTargetWire::App {
                bundle_id: "com.example.app".to_owned(),
            },
        })
        .unwrap();

    let events = fixture.audit.events.lock().unwrap();
    let click_intent = events
        .iter()
        .find(|event| {
            event.operation == AuditOperation::CuClick && event.outcome == AuditOutcome::Attempted
        })
        .expect("a durable intent record precedes input synthesis");
    let click_completion = events
        .iter()
        .find(|event| {
            event.operation == AuditOperation::CuClick && event.outcome == AuditOutcome::Allowed
        })
        .expect("the click records its completion");
    // Intent precedes completion in the trail, paired by request id.
    assert_eq!(click_intent.request_id, click_completion.request_id);
    // The target is the de-sensitized app identity — never screen content.
    assert!(matches!(
        &click_completion.target,
        AuditTarget::App { bundle_id, element_label: None }
            if bundle_id.as_str() == "com.example.app"
    ));
    assert!(events.iter().any(|event| {
        event.operation == AuditOperation::CuKeyPress && event.outcome == AuditOutcome::Attempted
    }));
    assert!(events.iter().any(|event| {
        event.operation == AuditOperation::CuCaptureScreen && event.outcome == AuditOutcome::Allowed
    }));
}

#[test]
fn a_once_grant_authorizes_exactly_one_terminal_op() {
    let fixture = cu_setup();
    assert!(
        fixture
            .grant_once(Capability::ControlApp, Some("com.example.app"))
            .granted
    );
    // Default stub label is "Cancel" — benign, so the click fires now.
    fixture.click("com.example.app").unwrap();
    assert_eq!(fixture.backend.clicks().len(), 1);
    let error = fixture.click("com.example.app").unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
    assert!(fixture.backend.clicks().len() == 1);
}

#[test]
fn a_once_grant_covers_a_confirmation_hold_then_is_consumed() {
    let fixture = cu_setup();
    fixture.grant_once(Capability::ControlApp, Some("com.example.app"));
    fixture.backend.set_label("Send");
    fixture.backend.set_fingerprint("fp1");

    let OperationResult::CuNeedsConfirmation(held) = fixture.click("com.example.app").unwrap()
    else {
        panic!("expected a confirmation hold")
    };
    // One-shots stay out of the management listing even while they still
    // authorize the held confirm.
    let listed = fixture
        .control(ControlRequest::CuListAppGrants(CuListAppGrantsRequest {
            subject: fixture.subject,
        }))
        .unwrap();
    let ControlResult::CuListAppGrants { grants } = listed else {
        panic!("unexpected control result")
    };
    assert!(grants.is_empty());

    fixture
        .control(ControlRequest::CuConfirmControlAction(
            CuConfirmControlActionRequest {
                confirmation_id: held.confirmation_id,
            },
        ))
        .unwrap();
    assert_eq!(fixture.backend.clicks().len(), 1);

    let error = fixture.click("com.example.app").unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
}

#[test]
fn a_standing_grant_replaces_a_leftover_once_grant() {
    let fixture = cu_setup();
    fixture.grant_once(Capability::ControlApp, Some("com.example.app"));
    let standing = fixture.grant(Capability::ControlApp, Some("com.example.app"));
    assert!(standing.granted);
    let listed = fixture
        .control(ControlRequest::CuListAppGrants(CuListAppGrantsRequest {
            subject: fixture.subject,
        }))
        .unwrap();
    let ControlResult::CuListAppGrants { grants } = listed else {
        panic!("unexpected control result")
    };
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].grant_id, standing.grant_id);
    fixture.click("com.example.app").unwrap();
    fixture.click("com.example.app").unwrap();
    assert_eq!(fixture.backend.clicks().len(), 2);
}

#[test]
fn a_once_grant_does_not_survive_broker_reload() {
    let (temp, broker, _, state_dir) = durable_setup();
    let subject = GrantSubject::conversation(Uuid::new_v4()).unwrap();
    let standing = Grant::from_consent(
        GrantId::new(),
        subject,
        Capability::ControlApp,
        Scope::App {
            bundle_id: "com.example.mail".to_owned(),
        },
        ConsentRecord::new(ConsentMethod::PermissionDialog, Utc::now()),
    )
    .unwrap();
    let once = Grant::from_consent(
        GrantId::new(),
        subject,
        Capability::ControlApp,
        Scope::App {
            bundle_id: "com.example.notes".to_owned(),
        },
        ConsentRecord::new(ConsentMethod::PermissionDialog, Utc::now()),
    )
    .unwrap()
    .into_single_use();
    let standing_id = standing.id();
    {
        let mut state = broker.shared.state.lock().unwrap();
        state.grants.push(standing);
        state.grants.push(once);
        broker
            .shared
            .state_file
            .as_ref()
            .expect("durable broker")
            .save(&state)
            .unwrap();
    }
    drop(broker);
    let reloaded = Broker::open(test_policy(&temp), &state_dir).unwrap();
    let grants = reloaded.shared.state.lock().unwrap().grants.clone();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].id(), standing_id);
    assert!(!grants[0].is_single_use());
}

#[test]
fn a_held_click_is_audited_as_held_not_allowed() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));
    fixture.backend.set_label("Send");

    let result = fixture.click("com.example.app").unwrap();
    let OperationResult::CuNeedsConfirmation(held) = result else {
        panic!("expected a confirmation hold, got {result:?}")
    };
    assert!(fixture.backend.clicks().is_empty());

    {
        let events = fixture.audit.events.lock().unwrap();
        let click: Vec<_> = events
            .iter()
            .filter(|event| event.operation == AuditOperation::CuClick)
            .map(|event| event.outcome)
            .collect();
        assert_eq!(click, [AuditOutcome::Attempted, AuditOutcome::Held]);
        assert!(!events.iter().any(|event| {
            event.operation == AuditOperation::CuClick && event.outcome == AuditOutcome::Allowed
        }));
    }

    fixture
        .control(ControlRequest::CuConfirmControlAction(
            CuConfirmControlActionRequest {
                confirmation_id: held.confirmation_id,
            },
        ))
        .unwrap();
    assert_eq!(fixture.backend.clicks().len(), 1);

    let events = fixture.audit.events.lock().unwrap();
    let click: Vec<_> = events
        .iter()
        .filter(|event| event.operation == AuditOperation::CuClick)
        .map(|event| event.outcome)
        .collect();
    assert_eq!(
        click,
        [
            AuditOutcome::Attempted,
            AuditOutcome::Held,
            AuditOutcome::Attempted,
            AuditOutcome::Allowed,
        ]
    );
}

#[test]
fn scroll_and_focus_record_intent_before_act() {
    let fixture = cu_setup();
    fixture.grant(Capability::ControlApp, Some("com.example.app"));

    fixture
        .operate(OperationRequest::CuScroll {
            bundle_id: "com.example.app".to_owned(),
            target: ElementTargetWire::default(),
            dx: None,
            dy: Some(40.0),
        })
        .unwrap();
    fixture
        .operate(OperationRequest::CuFocusWindow {
            bundle_id: "com.example.app".to_owned(),
            window_id: Some(7),
        })
        .unwrap();
    assert_eq!(fixture.backend.scrolls(), ["com.example.app"]);
    assert_eq!(fixture.backend.focuses(), ["com.example.app"]);

    let events = fixture.audit.events.lock().unwrap();
    for operation in [AuditOperation::CuScroll, AuditOperation::CuFocusWindow] {
        let intent = events
            .iter()
            .position(|event| {
                event.operation == operation && event.outcome == AuditOutcome::Attempted
            })
            .expect("a durable intent record precedes input synthesis");
        let completion = events
            .iter()
            .position(|event| {
                event.operation == operation && event.outcome == AuditOutcome::Allowed
            })
            .expect("the op records its completion");
        assert!(intent < completion);
        assert_eq!(events[intent].request_id, events[completion].request_id);
    }
}

#[test]
fn an_unrecordable_scroll_or_focus_never_reaches_the_backend() {
    let fixture = cu_setup();
    let broken = Arc::new(BreakableAudit::default());
    let broker = Broker::test_with_computer_use(
        test_policy(&fixture._temp),
        broken.clone(),
        fixture.backend.clone(),
        fixture._temp.path().join("cu-staging-broken"),
    );
    let conversation = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation).unwrap();
    let context = ExecutionContext::standalone(conversation).unwrap();
    let granted = unwrap_response(broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: crate::RequestId::new(),
        request: ControlRequest::CuGrantApp(CuGrantAppRequest {
            subject,
            capability: Capability::ControlApp,
            bundle_id: Some("com.example.app".to_owned()),
            consent: ConsentMethod::PermissionDialog,
            single_use: false,
        }),
    }))
    .unwrap();
    assert!(matches!(granted, ControlResult::CuGrantApp(_)));

    broken.broken.store(true, Ordering::SeqCst);
    for request in [
        OperationRequest::CuScroll {
            bundle_id: "com.example.app".to_owned(),
            target: ElementTargetWire::default(),
            dx: None,
            dy: Some(40.0),
        },
        OperationRequest::CuFocusWindow {
            bundle_id: "com.example.app".to_owned(),
            window_id: Some(7),
        },
    ] {
        let error = operate(&broker.operator(), context, request).unwrap_err();
        assert_eq!(error.code, ErrorCode::AuditUnavailable);
        assert!(error.retryable);
    }
    assert!(fixture.backend.scrolls().is_empty());
    assert!(fixture.backend.focuses().is_empty());
}
