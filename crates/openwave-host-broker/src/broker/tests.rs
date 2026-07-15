use super::*;

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

struct FailingAudit;

impl AuditSink for FailingAudit {
    fn record(&self, _event: &AuditEvent) -> Result<(), AuditError> {
        Err(AuditError::Io(io::Error::other("injected audit failure")))
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

fn unwrap_response<T>(envelope: ResponseEnvelope<T>) -> Result<T, ErrorResponse> {
    match envelope.response {
        Response::Ok(result) => Ok(result),
        Response::Error(error) => Err(error),
    }
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
            roots: vec![registered.root.clone()]
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
                roots: vec![first.root.clone()]
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
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].operation, AuditOperation::RegisterRoot);
    assert!(matches!(
        &events[0].target,
        AuditTarget::SelectedFolder { display_name } if display_name.as_str() == "Documents"
    ));
    assert_eq!(events[1].operation, AuditOperation::ReadFile);
    assert_eq!(events[1].outcome, AuditOutcome::Allowed);
    assert!(events[1].grant_id.is_some());
    assert_eq!(events[1].bytes, Some(17));
    assert!(matches!(
        &events[1].target,
        AuditTarget::Path { root_id, relative }
            if *root_id == registered.root.root_id && relative.as_str() == "note.txt"
    ));
    assert_eq!(events[2].outcome, AuditOutcome::Denied);
    assert_eq!(events[2].error_code, Some(ErrorCode::Denied));
    assert!(events[2].grant_id.is_none());
    let encoded = serde_json::to_string(&*events).unwrap();
    assert!(!encoded.contains("home/Documents"));
    assert!(!encoded.contains("hello from broker"));
}

#[test]
fn read_tier_audit_failure_does_not_block_user_access() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("home/Documents");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
    let broker = Broker::with_audit_sink(test_policy(&temp), Arc::new(FailingAudit));
    let conversation = Uuid::new_v4();
    let registered = register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        root,
        OperationId::new(),
    );
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
        list_roots(&state, ExecutionContext::standalone(conversation).unwrap()),
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
        OperationResult::ListRoots { roots } if roots == vec![registered.root.clone()]
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
        OperationResult::ListRoots { roots } if roots == vec![registered.root]
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
        RootAttachmentMutationReceipt::Failed { error } if error == first
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
        ControlResult::Hello(super::hello())
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
fn restart_revalidates_every_persisted_root_before_advertising_state() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path.clone(),
        OperationId::new(),
    );
    drop(broker);
    std::fs::remove_file(path.join("note.txt")).unwrap();
    std::fs::remove_dir(path).unwrap();

    assert!(matches!(
        Broker::open(test_policy(&temp), &state_dir),
        Err(BrokerError::RootPolicy(_))
    ));
}

#[test]
fn restart_refuses_to_rebind_a_grant_to_a_replaced_folder() {
    let (temp, broker, path, state_dir) = durable_setup();
    let conversation = Uuid::new_v4();
    register(
        &broker.controller(),
        GrantSubject::conversation(conversation).unwrap(),
        conversation,
        path.clone(),
        OperationId::new(),
    );
    drop(broker);
    let original = path.with_file_name("Documents-original");
    std::fs::rename(&path, original).unwrap();
    std::fs::create_dir(&path).unwrap();

    assert!(matches!(
        Broker::open(test_policy(&temp), &state_dir),
        Err(BrokerError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
    ));
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
fn persisted_attachments_must_be_unique_and_respect_conversation_ownership() {
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
        },
        outcome: MutationOutcome::Pending,
    };
    let mut encoded = serde_json::to_value(record).unwrap();
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
