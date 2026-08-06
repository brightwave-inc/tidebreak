use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use openwave_code_execution::{
    resolve_scratch_directory, sync, CodeExecutionError, CodeExecutionProvider,
    CodeExecutionProviderKind, CodeExecutionRequest, CodeExecutionResponse,
    CodeExecutionUnavailableReason, DaytonaCredential, DaytonaExecutionProvider, E2BCredential,
    E2BExecutionProvider, ExecFolderAccess, ExecFolderGrant, ExecutionId, ExecutionWorkspaceId,
    LocalExecutionProvider, MaterializationPrecondition, MaterializedChangeKind,
    OutputArtifactEntry, OutputArtifactScan, OutputArtifactStatus, PreviewScan,
    RejectedChangeReason, RemoteSessionPool, SharedPackageCache, StagedUpload, WorkspaceFilePath,
    WorkspaceLifecycle, WorkspaceListing, WriteOverlay, WriteSnapshotSink, DAYTONA_CREDENTIAL_KEY,
    DOCUMENT_SCRIPTS_DIR, DOCUMENT_SCRIPT_FILES, E2B_CREDENTIAL_KEY, PACKAGE_CACHE_DIR,
    PACKAGE_MANAGER_DOMAINS,
};
use openwave_core::{
    exec_attachment_file_name, BlobStore, CallId, Chat, ChatId, ExecFileRejectionReason,
    ExecFileRejectionRecord, HostRootId, MessageDocumentAttachment, NetworkPolicy, ProjectId,
    Result, RevisionProducer, SecretProvider, Store, TurnId, MAX_EXEC_WORKSPACE_FILE_BYTES,
};
use openwave_egress::{
    CidrBlock, DomainPattern, EgressAllowlist, EgressEnforcement, EgressError, EgressPolicy,
};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;
use crate::exec_write_snapshot::TurnSnapshotSink;
use crate::state::BlobWriteGuard;

use super::*;

use chrono::Utc;
use openwave_core::{
    AgentError, ChatRootAttachment, DbStore, DocumentId, DocumentSourceBlob, DocumentSourceUpsert,
    FsBlobStore, PermissionMode, RootAttachmentOrigin, TurnId,
};
use std::sync::Mutex;
use uuid::Uuid;

struct NoSecrets;

struct RecordingFolderResolver {
    queries: Mutex<Vec<ExecFolderGrantQuery>>,
    roots: Vec<ResolvedExecFolderGrant>,
}

#[async_trait]
impl ExecFolderGrantResolver for RecordingFolderResolver {
    async fn resolve(
        &self,
        query: ExecFolderGrantQuery,
    ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, String> {
        self.queries.lock().unwrap().push(query);
        Ok(self.roots.clone())
    }
}

#[async_trait]
impl SecretProvider for NoSecrets {
    async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
        Err(AgentError::Secret("read only test secrets".into()))
    }

    async fn delete_secret(&self, _key: &str) -> Result<()> {
        Err(AgentError::Secret("read only test secrets".into()))
    }
}

async fn test_store() -> (DbStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("code-execution.db").display()
    ))
    .await
    .unwrap();
    (store, dir)
}

#[tokio::test]
async fn folder_resolution_is_fenced_to_the_chat_projection() {
    let (store, _database) = test_store().await;
    let granted = HostRootId::from_uuid(Uuid::new_v4()).unwrap();
    let injected = HostRootId::from_uuid(Uuid::new_v4()).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let resolver = Arc::new(RecordingFolderResolver {
        queries: Mutex::new(Vec::new()),
        roots: vec![ResolvedExecFolderGrant {
            root_id: granted,
            path: folder.path().to_path_buf(),
            writable: false,
            overlay: None,
            staging_unavailable: false,
        }],
    });
    let provider = ConfiguredCodeExecutionProvider::new(
        Arc::new(store),
        Arc::new(NoSecrets),
        tempfile::tempdir().unwrap().path(),
    )
    .with_folder_grant_resolver(Some(resolver.clone()));
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: Some(PermissionMode::Ask),
        network_policy: Default::default(),
        attachment_revision: 1,
        root_attachments: vec![ChatRootAttachment {
            root_id: granted,
            origin: RootAttachmentOrigin::Conversation,
        }],
        created_at: Utc::now(),
    };

    let resolved = provider.resolve_chat_folder_grants(&chat).await.unwrap();
    assert_eq!(resolved[0].root_id, granted);
    assert_eq!(resolver.queries.lock().unwrap()[0].root_ids, vec![granted]);

    let bad_resolver = Arc::new(RecordingFolderResolver {
        queries: Mutex::new(Vec::new()),
        roots: vec![ResolvedExecFolderGrant {
            root_id: injected,
            path: folder.path().to_path_buf(),
            writable: true,
            overlay: None,
            staging_unavailable: false,
        }],
    });
    let (store, _database) = test_store().await;
    let bad_provider = ConfiguredCodeExecutionProvider::new(
        Arc::new(store),
        Arc::new(NoSecrets),
        tempfile::tempdir().unwrap().path(),
    )
    .with_folder_grant_resolver(Some(bad_resolver));
    assert!(bad_provider
        .resolve_chat_folder_grants(&chat)
        .await
        .is_err());
}

/// A tree outside the overlay's bounded contract must lose exec write
/// access rather than regaining direct access to the user's real folder.
#[tokio::test]
async fn a_folder_that_cannot_be_staged_fails_closed_and_stays_visible() {
    let (store, _database) = test_store().await;
    let scratch = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let mut nested = folder.path().to_path_buf();
    for depth in 0..30 {
        nested.push(format!("level-{depth}"));
        std::fs::create_dir(&nested).unwrap();
    }
    let root_id = HostRootId::from_uuid(Uuid::new_v4()).unwrap();
    let provider =
        ConfiguredCodeExecutionProvider::new(Arc::new(store), Arc::new(NoSecrets), scratch.path());
    let mut grants = vec![ResolvedExecFolderGrant {
        root_id,
        path: folder.path().to_path_buf(),
        writable: true,
        overlay: None,
        staging_unavailable: false,
    }];

    provider
        .open_write_overlay(ChatId::new(), TurnId::new(), &mut grants)
        .await;

    assert!(!grants[0].writable);
    assert!(grants[0].staging_unavailable);
    assert!(grants[0].overlay.is_none());
    let effective = exec_folder_grant_for_turn(grants.remove(0), &HashMap::new()).unwrap();
    assert_eq!(effective.access, ExecFolderAccess::ReadOnly);
    assert!(effective.writable_path().is_none());
}

/// The files-first creation path end to end at the provider seam: a file
/// written to `output/` becomes a turn-attributed output, an identical
/// rerun mints nothing, and changed bytes append a revision.
#[tokio::test]
async fn output_files_publish_as_turn_attributed_outputs() {
    use openwave_core::model::{ToolCallExecution, ToolCallRecord, ToolCallStatus};
    use openwave_core::{CallId, TurnId};

    let (store, _database) = test_store().await;
    let store = Arc::new(store);
    let scratch_root = tempfile::tempdir().unwrap();
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accept_exec_call = |store: Arc<DbStore>| async move {
        let call_id = CallId::new();
        store
            .accept_tool_call(&ToolCallRecord {
                id: call_id,
                chat_id: chat.id,
                turn_id,
                provider_id: format!("provider-{call_id}"),
                name: "exec".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
                execution: ToolCallExecution::Server,
                status: ToolCallStatus::Pending,
                result: None,
                result_preview: None,

                provider_replay: None,
                error_code: None,
                error_detail: None,
                client_executor_id: None,
                client_lease_expires_at: None,
                created_at: Utc::now(),
                resolved_at: None,
            })
            .await
            .unwrap();
        call_id
    };
    let call_id = accept_exec_call(store.clone()).await;

    let output_dir = scratch_root.path().join(chat.id.to_string()).join("output");
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(output_dir.join("report.md"), b"# Draft").unwrap();

    let provider = ConfiguredCodeExecutionProvider::new(
        store.clone(),
        Arc::new(NoSecrets),
        scratch_root.path(),
    );
    let workspace = ExecutionWorkspaceId::parse(chat.id.to_string()).unwrap();
    let execution = ExecutionId::parse(call_id.to_string()).unwrap();

    let scan = provider
        .collect_output_artifacts(&workspace, &execution)
        .await
        .unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].status, OutputArtifactStatus::Created);
    let outputs = store.list_outputs(chat.id, 10).await.unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].filename, "report.md");
    let revision = store
        .get_output_revision(outputs[0].current_revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revision.turn_id, Some(turn_id));

    // Identical rerun: nothing minted.
    let rerun = provider
        .collect_output_artifacts(&workspace, &execution)
        .await
        .unwrap();
    assert_eq!(rerun.entries[0].status, OutputArtifactStatus::Unchanged);
    assert_eq!(
        store.list_outputs(chat.id, 10).await.unwrap()[0].revision_count,
        1
    );

    // Changed bytes from a later call: a revision on the same output. (The
    // same call identity republishing different bytes is refused by the
    // write-once path, so each update rides its own call.)
    std::fs::write(output_dir.join("report.md"), b"# Final").unwrap();
    let later_call = accept_exec_call(store.clone()).await;
    let later_execution = ExecutionId::parse(later_call.to_string()).unwrap();
    let changed = provider
        .collect_output_artifacts(&workspace, &later_execution)
        .await
        .unwrap();
    assert_eq!(changed.entries[0].status, OutputArtifactStatus::Updated);
    let updated = store.list_outputs(chat.id, 10).await.unwrap();
    assert_eq!(updated[0].id, outputs[0].id);
    assert_eq!(updated[0].revision_count, 2);

    // A call identity the conversation does not own publishes nothing.
    let foreign = ExecutionId::parse(CallId::new().to_string()).unwrap();
    assert!(provider
        .collect_output_artifacts(&workspace, &foreign)
        .await
        .is_err());
}

#[tokio::test]
async fn exec_workspace_conventions_exist_before_a_command_runs() {
    let dir = tempfile::tempdir().unwrap();
    prepare_execution_directories(dir.path(), true, None, &[])
        .await
        .unwrap();

    for name in ["output", "preview", "documents"] {
        assert!(dir.path().join(name).is_dir());
        assert!(dir.path().join(name).join(".openwave-directory").is_file());
    }
}

#[tokio::test]
async fn attached_documents_backfill_lazily_with_collision_and_size_limits() {
    let (store, database) = test_store().await;
    let blobs = FsBlobStore::new(database.path().join("blobs"));
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let first_bytes = b"first opaque attachment";
    let first_blob = DocumentSourceBlob::from_bytes(first_bytes);
    blobs
        .put(first_blob.id, first_bytes.to_vec())
        .await
        .unwrap();
    let second_bytes = b"second opaque attachment";
    let second_blob = DocumentSourceBlob::from_bytes(second_bytes);
    blobs
        .put(second_blob.id, second_bytes.to_vec())
        .await
        .unwrap();
    let oversized_blob =
        DocumentSourceBlob::from_digest([7; 32], MAX_EXEC_WORKSPACE_FILE_BYTES as u64 + 1);
    let first_id = DocumentId::new();
    let second_id = DocumentId::new();
    let oversized_id = DocumentId::new();
    for source in [
        DocumentSourceUpsert {
            id: first_id,
            chat_id: Some(chat.id),
            project_id: None,
            source_uri: None,
            media_type: "application/pdf".into(),
            title: Some("report.pdf".into()),
            source_blob: first_blob,
            canonical_text: String::new(),
            updated_at: Utc::now(),
        },
        DocumentSourceUpsert {
            id: second_id,
            chat_id: Some(chat.id),
            project_id: None,
            source_uri: None,
            media_type: "application/pdf".into(),
            title: Some("report.pdf".into()),
            source_blob: second_blob,
            canonical_text: String::new(),
            updated_at: Utc::now(),
        },
        DocumentSourceUpsert {
            id: oversized_id,
            chat_id: Some(chat.id),
            project_id: None,
            source_uri: None,
            media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
            title: Some("large.xlsx".into()),
            source_blob: oversized_blob,
            canonical_text: String::new(),
            updated_at: Utc::now(),
        },
    ] {
        store.accept_document_source(&source).await.unwrap();
    }
    store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "inspect these",
            &[],
            &[first_id, second_id, oversized_id],
            &[],
        )
        .await
        .unwrap();

    let workspace = database.path().join("scratch").join(chat.id.to_string());
    prepare_execution_directories(&workspace, false, None, &[])
        .await
        .unwrap();
    materialize_chat_attachments(&store, &blobs, chat.id, &workspace)
        .await
        .unwrap();

    let first_path = workspace
        .join("documents")
        .join(exec_attachment_file_name(Some("report.pdf"), first_id));
    let second_path = workspace
        .join("documents")
        .join(exec_attachment_file_name(Some("report.pdf"), second_id));
    let oversized_path = workspace
        .join("documents")
        .join(exec_attachment_file_name(Some("large.xlsx"), oversized_id));
    assert_ne!(first_path, second_path);
    assert_eq!(std::fs::read(&first_path).unwrap(), first_bytes);
    assert_eq!(std::fs::read(&second_path).unwrap(), second_bytes);
    assert!(!oversized_path.exists());

    // Materialization runs at each invocation, so a later first exec or a
    // modified workspace still sees the immutable original attachment.
    std::fs::write(&first_path, b"workspace edit").unwrap();
    materialize_chat_attachments(&store, &blobs, chat.id, &workspace)
        .await
        .unwrap();
    assert_eq!(std::fs::read(first_path).unwrap(), first_bytes);
}

#[tokio::test]
async fn bundled_document_helpers_are_installed_as_one_library() {
    let source = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    for name in DOCUMENT_SCRIPT_FILES {
        std::fs::write(source.path().join(name), format!("helper:{name}")).unwrap();
    }

    let skill = openwave_code_execution::LoadedSkill {
        package: openwave_code_execution::SkillPackage {
            name: "pdf-documents".into(),
            description: "Produce PDFs.".into(),
            python_deps: vec!["fpdf2==2.8.3".into()],
            npm_deps: Vec::new(),
            host_deps: Vec::new(),
            origin: openwave_code_execution::SkillOrigin::Builtin,
        },
        manifest: "---\nname: pdf-documents\n---\nbody".into(),
        scripts: Vec::new(),
    };
    prepare_execution_directories(
        workspace.path(),
        false,
        Some(source.path()),
        std::slice::from_ref(&skill),
    )
    .await
    .unwrap();

    for name in DOCUMENT_SCRIPT_FILES {
        let installed = workspace.path().join(DOCUMENT_SCRIPTS_DIR).join(name);
        assert_eq!(
            std::fs::read_to_string(installed).unwrap(),
            format!("helper:{name}")
        );
    }
    let staged_skill = workspace
        .path()
        .join(openwave_code_execution::SKILLS_DIR)
        .join("pdf-documents")
        .join(openwave_code_execution::SKILL_MANIFEST_FILE);
    assert_eq!(
        std::fs::read_to_string(staged_skill).unwrap(),
        skill.manifest
    );
}

/// Turn-start staging pins the contract the prompt catalog relies on:
/// every advertised skill's `SKILL.md` — and the helper files it tells the
/// model to run — is readable in the chat's private scratch, the directory
/// the `read_file` surface resolves against, before any exec has run.
#[tokio::test]
async fn turn_start_staging_makes_skills_readable_before_any_exec() {
    let (store, _database) = test_store().await;
    let store = Arc::new(store);
    let scratch_root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = "---\n\
            name: presentations\n\
            description: Create PowerPoint decks.\n\
            ---\n\
            Body.\n";
    let skill_dir = source.path().join("presentations");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join(openwave_code_execution::SKILL_MANIFEST_FILE),
        manifest,
    )
    .unwrap();
    let scripts_dir = skill_dir.join(openwave_code_execution::SKILL_SCRIPTS_DIR);
    std::fs::create_dir(&scripts_dir).unwrap();
    std::fs::write(scripts_dir.join("build_deck.py"), "print('deck')\n").unwrap();

    let provider = ConfiguredCodeExecutionProvider::new(
        store.clone(),
        Arc::new(NoSecrets),
        scratch_root.path(),
    )
    .with_skills(Some(source.path().to_owned()))
    .with_user_skills(Some(scratch_root.path().join("user-skills")));
    let chat_id = ChatId::new();

    provider.stage_turn_workspace(chat_id).await;

    let staged = scratch_root
        .path()
        .join(chat_id.to_string())
        .join(openwave_code_execution::SKILLS_DIR)
        .join("presentations")
        .join(openwave_code_execution::SKILL_MANIFEST_FILE);
    assert_eq!(std::fs::read_to_string(&staged).unwrap(), manifest);
    let staged_script = scratch_root
        .path()
        .join(chat_id.to_string())
        .join(openwave_code_execution::SKILLS_DIR)
        .join("presentations")
        .join(openwave_code_execution::SKILL_SCRIPTS_DIR)
        .join("build_deck.py");
    assert_eq!(
        std::fs::read_to_string(&staged_script).unwrap(),
        "print('deck')\n"
    );

    // Idempotent: the first exec re-prepares the same tree.
    provider.stage_turn_workspace(chat_id).await;
    assert_eq!(std::fs::read_to_string(&staged).unwrap(), manifest);

    // A skill the user drops in after configuration is picked up by the
    // next staging without a restart, and the catalog attributes it.
    let user_manifest = "---\nname: meeting-notes\ndescription: My way.\n---\nBody.\n";
    let user_skill = scratch_root
        .path()
        .join("user-skills")
        .join("meeting-notes");
    std::fs::create_dir_all(&user_skill).unwrap();
    std::fs::write(
        user_skill.join(openwave_code_execution::SKILL_MANIFEST_FILE),
        user_manifest,
    )
    .unwrap();
    provider.stage_turn_workspace(chat_id).await;
    let staged_user = scratch_root
        .path()
        .join(chat_id.to_string())
        .join(openwave_code_execution::SKILLS_DIR)
        .join("meeting-notes")
        .join(openwave_code_execution::SKILL_MANIFEST_FILE);
    assert_eq!(
        std::fs::read_to_string(&staged_user).unwrap(),
        user_manifest
    );
    assert_eq!(
        provider
            .skill_catalog()
            .await
            .iter()
            .map(|skill| (skill.name.as_str(), skill.origin))
            .collect::<Vec<_>>(),
        [
            ("meeting-notes", openwave_code_execution::SkillOrigin::User),
            (
                "presentations",
                openwave_code_execution::SkillOrigin::Builtin
            ),
        ]
    );

    // A skill-less configuration (headless embeddings) stages nothing.
    let bare =
        ConfiguredCodeExecutionProvider::new(store, Arc::new(NoSecrets), scratch_root.path());
    let bare_chat = ChatId::new();
    bare.stage_turn_workspace(bare_chat).await;
    assert!(!scratch_root.path().join(bare_chat.to_string()).exists());
}

/// Contract: switching a component off has to bite in both places at once
/// — the staged workspace and the prompt catalog derived from it. A skill
/// the model is still told about but whose `SKILL.md` is gone is the exact
/// inconsistency the single filtered read exists to prevent.
#[tokio::test]
async fn disabling_drops_a_component_from_staging_and_the_catalog() {
    let (store, _database) = test_store().await;
    let store = Arc::new(store);
    let scratch_root = tempfile::tempdir().unwrap();
    let skills_dir = tempfile::tempdir().unwrap();
    for name in ["charts", "word-documents"] {
        let dir = skills_dir.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join(openwave_code_execution::SKILL_MANIFEST_FILE),
            format!("---\nname: {name}\ndescription: Does {name} work.\n---\nBody.\n"),
        )
        .unwrap();
    }
    let plugins_dir = tempfile::tempdir().unwrap();
    let plugin = plugins_dir.path().join("documents");
    std::fs::create_dir(&plugin).unwrap();
    std::fs::write(
        plugin.join(openwave_code_execution::PLUGIN_MANIFEST_FILE),
        "---\nname: documents\ndisplay-name: Documents\ndescription: Deliverables.\n\
             category: documents\nskills: [\"word-documents\"]\n---\n",
    )
    .unwrap();

    let provider = ConfiguredCodeExecutionProvider::new(
        store.clone(),
        Arc::new(NoSecrets),
        scratch_root.path(),
    )
    .with_skills(Some(skills_dir.path().to_owned()))
    .with_plugins(Some(plugins_dir.path().to_owned()));
    let chat_id = ChatId::new();
    let staged = |name: &str| {
        scratch_root
            .path()
            .join(chat_id.to_string())
            .join(openwave_code_execution::SKILLS_DIR)
            .join(name)
            .join(openwave_code_execution::SKILL_MANIFEST_FILE)
    };

    provider.stage_turn_workspace(chat_id).await;
    assert!(staged("charts").exists());
    assert!(staged("word-documents").exists());

    // Disabling the bundle takes its member out of both; the standalone
    // skill is untouched, and the bundle leaves the plugin catalog.
    let mut flags = provider.enable_state().await;
    flags.set_plugin("documents", false);
    crate::plugin_state::write_plugin_enable_state(&*store, &flags)
        .await
        .unwrap();
    provider.stage_turn_workspace(chat_id).await;
    assert!(!staged("word-documents").exists());
    assert!(staged("charts").exists());
    assert_eq!(
        provider
            .skill_catalog()
            .await
            .iter()
            .map(|skill| skill.name.clone())
            .collect::<Vec<_>>(),
        ["charts"]
    );
    assert!(provider.plugin_catalog().await.is_empty());

    // Disabling a standalone skill needs no bundle to go through.
    let mut flags = provider.enable_state().await;
    flags.set_skill("charts", false);
    crate::plugin_state::write_plugin_enable_state(&*store, &flags)
        .await
        .unwrap();
    provider.stage_turn_workspace(chat_id).await;
    assert!(!staged("charts").exists());
    assert!(provider.skill_catalog().await.is_empty());
}

/// The declaration-driven host-tool contract: staging a skill that
/// declares `host: ["libreoffice"]` warms the broker exactly once per
/// staging, and the prompt capability flag is the broker's status — never
/// a promise — omitted entirely when nothing declares the dependency.
#[tokio::test]
async fn declared_host_deps_warm_the_broker_and_gate_the_capability_flag() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingBroker {
        ensures: AtomicUsize,
        available: bool,
    }

    #[async_trait]
    impl openwave_code_execution::HostToolBroker for RecordingBroker {
        fn ensure(&self, tool: openwave_code_execution::HostDep) {
            assert_eq!(tool, openwave_code_execution::HostDep::LibreOffice);
            self.ensures.fetch_add(1, Ordering::SeqCst);
        }

        async fn status(
            &self,
            _tool: openwave_code_execution::HostDep,
        ) -> openwave_code_execution::HostToolStatus {
            if self.available {
                openwave_code_execution::HostToolStatus::Available
            } else {
                openwave_code_execution::HostToolStatus::Unavailable("not installed".into())
            }
        }

        async fn managed_root(
            &self,
            _tool: openwave_code_execution::HostDep,
        ) -> Option<std::path::PathBuf> {
            None
        }
    }

    let (store, _database) = test_store().await;
    let store = Arc::new(store);
    let scratch_root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let skill_dir = source.path().join("presentations");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join(openwave_code_execution::SKILL_MANIFEST_FILE),
        "---\n\
             name: presentations\n\
             description: Decks.\n\
             deps: { npm: [\"pptxgenjs@4.0.1\"], host: [\"libreoffice\"] }\n\
             ---\n\
             Body.\n",
    )
    .unwrap();

    let broker = Arc::new(RecordingBroker {
        ensures: AtomicUsize::new(0),
        available: true,
    });
    let provider = ConfiguredCodeExecutionProvider::new(
        store.clone(),
        Arc::new(NoSecrets),
        scratch_root.path(),
    )
    .with_skills(Some(source.path().to_owned()))
    .with_host_tool_broker(Some(broker.clone()));

    provider.stage_turn_workspace(ChatId::new()).await;
    assert_eq!(broker.ensures.load(Ordering::SeqCst), 1);
    assert_eq!(provider.office_rendering_available().await, Some(true));

    // An unavailable tool reports false — the prompt says so instead of
    // teaching a QA loop the host cannot run.
    let unavailable = Arc::new(RecordingBroker {
        ensures: AtomicUsize::new(0),
        available: false,
    });
    let provider = ConfiguredCodeExecutionProvider::new(
        store.clone(),
        Arc::new(NoSecrets),
        scratch_root.path(),
    )
    .with_skills(Some(source.path().to_owned()))
    .with_host_tool_broker(Some(unavailable));
    assert_eq!(provider.office_rendering_available().await, Some(false));

    // No declaration, no line; no broker, no promise.
    let no_deps = tempfile::tempdir().unwrap();
    let plain = no_deps.path().join("charts");
    std::fs::create_dir(&plain).unwrap();
    std::fs::write(
        plain.join(openwave_code_execution::SKILL_MANIFEST_FILE),
        "---\nname: charts\ndescription: Plots.\n---\nBody.\n",
    )
    .unwrap();
    let provider = ConfiguredCodeExecutionProvider::new(
        store.clone(),
        Arc::new(NoSecrets),
        scratch_root.path(),
    )
    .with_skills(Some(no_deps.path().to_owned()));
    assert_eq!(provider.office_rendering_available().await, None);

    let brokerless =
        ConfiguredCodeExecutionProvider::new(store, Arc::new(NoSecrets), scratch_root.path())
            .with_skills(Some(source.path().to_owned()));
    assert_eq!(brokerless.office_rendering_available().await, Some(false));
}

/// Local exec is confined to the scratch directory but can create entries
/// in it, including a symlink aimed at the host. Both preparation writes
/// run on that directory before the next command, unsandboxed.
#[cfg(unix)]
#[tokio::test]
async fn preparation_does_not_write_through_a_planted_symlink() {
    let outside = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    for name in DOCUMENT_SCRIPT_FILES {
        std::fs::write(source.path().join(name), format!("helper:{name}")).unwrap();
    }
    let workspace = tempfile::tempdir().unwrap();

    let skill = openwave_code_execution::LoadedSkill {
        package: openwave_code_execution::SkillPackage {
            name: "pdf-documents".into(),
            description: "Produce PDFs.".into(),
            python_deps: Vec::new(),
            npm_deps: Vec::new(),
            host_deps: Vec::new(),
            origin: openwave_code_execution::SkillOrigin::Builtin,
        },
        manifest: "---\nname: pdf-documents\n---\nbody".into(),
        scripts: Vec::new(),
    };
    let skills = std::slice::from_ref(&skill);

    std::os::unix::fs::symlink(outside.path(), workspace.path().join("output")).unwrap();
    assert!(
        prepare_execution_directories(workspace.path(), true, Some(source.path()), skills)
            .await
            .is_err()
    );
    assert!(!outside.path().join(".openwave-directory").exists());

    std::fs::remove_file(workspace.path().join("output")).unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join(".openwave")).unwrap();
    assert!(
        prepare_execution_directories(workspace.path(), true, Some(source.path()), skills)
            .await
            .is_err()
    );
    assert!(!outside.path().join("exec-scripts").exists());
    assert!(!outside.path().join("skills").exists());
}

#[test]
fn the_default_selection_is_never_a_provider_that_cannot_run() {
    let config = CodeExecutionConfig::default();
    // Local is the only unattended default, and only where its sandbox
    // exists: on any other host the honest default is no provider, so the
    // surface reports "not configured" instead of a selection that fails
    // every exec.
    assert_eq!(
        config.provider,
        LocalExecutionProvider::availability()
            .is_ok()
            .then_some(CodeExecutionProviderKind::Local)
    );
    assert_eq!(config.timeout_ms, DEFAULT_TIMEOUT_MS);
    assert!(config.validate().is_ok());
    assert!(CodeExecutionConfig {
        provider: Some(CodeExecutionProviderKind::Local),
        timeout_ms: MIN_TIMEOUT_MS - 1,
        egress: EgressConfig::Open,
        e2b_template: None,
        daytona_snapshot: None,
    }
    .validate()
    .is_err());
}

#[test]
fn selection_contains_no_endpoint_or_credential_reference() {
    let json = serde_json::to_value(CodeExecutionConfig {
        provider: Some(CodeExecutionProviderKind::Local),
        ..CodeExecutionConfig::default()
    })
    .unwrap();
    assert_eq!(json["provider"], "local");
    assert!(json.get("endpoint").is_none());
    assert!(json.get("credential").is_none());
}

#[test]
fn egress_defaults_to_open_and_compiles_no_policy() {
    let config = CodeExecutionConfig::default();
    assert_eq!(config.egress, EgressConfig::Open);
    // Open must leave the managed adapters on today's open-internet
    // creation: no policy is threaded into the create path.
    assert_eq!(config.egress.to_policy().unwrap(), None);

    // The egress config carries no secret or endpoint — only patterns.
    let json = serde_json::to_value(&config.egress).unwrap();
    assert_eq!(json, serde_json::json!({ "mode": "open" }));
}

#[test]
fn egress_allowlist_compiles_to_a_deny_by_default_decision_policy() {
    let config = EgressConfig::Allowlist {
        domains: vec!["*.pypi.org".to_owned(), "crates.io".to_owned()],
        cidrs: vec!["140.82.112.0/20".to_owned()],
    };
    let Some(EgressPolicy::Allowlist(allowlist)) = config.to_policy().unwrap() else {
        panic!("a non-empty allowlist compiles to an allowlist policy");
    };
    assert_eq!(allowlist.domains().len(), 2);
    assert_eq!(allowlist.cidrs().len(), 1);

    // An empty allowlist is a deny-all policy, not open egress.
    let empty = EgressConfig::Allowlist {
        domains: vec![],
        cidrs: vec![],
    };
    let Some(EgressPolicy::Allowlist(empty_list)) = empty.to_policy().unwrap() else {
        panic!("an empty allowlist still compiles to a policy");
    };
    assert!(empty_list.is_empty());

    // A malformed grant fails closed at validation rather than widening.
    let bad = EgressConfig::Allowlist {
        domains: vec!["not a host".to_owned()],
        cidrs: vec![],
    };
    assert!(bad.to_policy().is_err());
    assert!(CodeExecutionConfig {
        provider: Some(CodeExecutionProviderKind::E2b),
        timeout_ms: DEFAULT_TIMEOUT_MS,
        egress: bad,
        e2b_template: None,
        daytona_snapshot: None,
    }
    .validate()
    .is_err());
}

#[test]
fn chat_network_policy_compiles_package_class_and_deny_all_for_managed_providers() {
    let off = network_egress_config(&NetworkPolicy::Off)
        .to_policy()
        .unwrap();
    let Some(EgressPolicy::Allowlist(off)) = off else {
        panic!("off must compile to an explicit deny-all policy");
    };
    assert!(off.is_empty());

    let packages = network_egress_config(&NetworkPolicy::PackageManagers)
        .to_policy()
        .unwrap();
    let Some(EgressPolicy::Allowlist(packages)) = packages else {
        panic!("package managers must compile to an allowlist");
    };
    assert_eq!(packages.domains().len(), PACKAGE_MANAGER_DOMAINS.len());
    assert!(packages
        .domains()
        .iter()
        .any(|domain| domain.to_string() == "pypi.org"));
    assert!(packages.cidrs().is_empty());

    assert_eq!(
        network_egress_config(&NetworkPolicy::Open)
            .to_policy()
            .unwrap(),
        None
    );
}

#[test]
fn egress_enforcement_never_oversells_a_provider_past_its_model() {
    let status = egress_enforcement_status();
    let row = |provider| {
        status
            .iter()
            .find(|row| row.provider == provider)
            .unwrap_or_else(|| panic!("{provider} enforcement is disclosed"))
    };
    let e2b = row(CodeExecutionProviderKind::E2b);
    let daytona = row(CodeExecutionProviderKind::Daytona);

    // E2B is confirmed, but its own enforcement model says it is not a full
    // boundary — domain rules cover only HTTP/HTTPS and DNS stays open — so
    // the surface must report gaps, not a boundary. Reading it from the
    // model is what keeps the two from ever disagreeing.
    assert!(
        !E2BExecutionProvider::egress_enforcement().is_credential_boundary(),
        "the model itself does not treat E2B as a boundary"
    );
    assert_eq!(e2b.status, EgressEnforcementStatus::AppliedWithGaps);
    assert!(
        !e2b.gaps.is_empty(),
        "the ports/DNS holes that make E2B not-a-boundary must be surfaced"
    );

    // Daytona's per-sandbox policy is a strict, externally enforced
    // boundary — confirmed live in #888 — so the corrected model treats it
    // as a credential boundary with no phantom curated-service exceptions.
    assert!(
        DaytonaExecutionProvider::egress_enforcement().is_credential_boundary(),
        "the corrected Daytona model is a credential boundary"
    );
    assert!(
        daytona.gaps.is_empty(),
        "the phantom curated-service exceptions must be gone from the disclosure"
    );
    // But it stays honest about the one thing the host can't verify
    // statically: the per-sandbox override needs Daytona org tier 3+. So it
    // is a conditional boundary carrying that requirement inline, never an
    // unconditional green boundary.
    assert_eq!(
        daytona.status,
        EgressEnforcementStatus::ConditionalBoundary,
        "Daytona over-claims as an unconditional boundary"
    );
    assert_eq!(
        daytona.requirement.as_deref(),
        Some(DAYTONA_TIER_REQUIREMENT)
    );
    assert_ne!(
        daytona.status,
        EgressEnforcementStatus::Boundary,
        "the tier caveat must keep Daytona off the unconditional boundary status"
    );
}

#[test]
fn resolve_glue_applies_the_configured_policy_to_the_managed_providers() {
    // The catastrophic-but-silent regression is the resolve path dropping
    // the policy — a configured allowlist reverting to open egress. These
    // assert the exact wiring resolve uses carries the policy through to the
    // provider; the adapter tests then prove that policy compiles into the
    // create body.
    let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
    let pool = RemoteSessionPool::default();
    let allowlist = EgressConfig::Allowlist {
        domains: vec!["*.pypi.org".to_owned()],
        cidrs: vec!["140.82.112.0/20".to_owned()],
    };
    let expected = allowlist.to_policy().unwrap().unwrap();

    let e2b = configured_e2b(
        E2BCredential::parse("test-e2b-key").unwrap(),
        timeout,
        pool.clone(),
        &allowlist,
        None,
    )
    .unwrap();
    assert_eq!(e2b.egress_policy(), Some(&expected));

    let daytona = configured_daytona(
        DaytonaCredential::parse("test-daytona-key").unwrap(),
        timeout,
        pool.clone(),
        &allowlist,
        None,
        None,
    )
    .unwrap();
    assert_eq!(daytona.egress_policy(), Some(&expected));

    // Open leaves both providers on today's open-internet creation: no
    // policy is threaded in.
    let open_e2b = configured_e2b(
        E2BCredential::parse("test-e2b-key").unwrap(),
        timeout,
        pool.clone(),
        &EgressConfig::Open,
        None,
    )
    .unwrap();
    assert_eq!(open_e2b.egress_policy(), None);
    let open_daytona = configured_daytona(
        DaytonaCredential::parse("test-daytona-key").unwrap(),
        timeout,
        pool,
        &EgressConfig::Open,
        None,
        None,
    )
    .unwrap();
    assert_eq!(open_daytona.egress_policy(), None);
}

#[tokio::test]
async fn configuration_can_disable_and_reenable_local_execution() {
    let (store, _dir) = test_store().await;
    let host_config = openwave_core::Config::desktop(_dir.path());
    let secrets = NoSecrets;
    let disabled = update_config(
        &host_config,
        &store,
        &secrets,
        CodeExecutionConfigUpdate {
            provider: Some(None),
            timeout_ms: Some(MIN_TIMEOUT_MS),
            egress: None,
            e2b_template: None,
            daytona_snapshot: None,
        },
    )
    .await;
    let disabled = match disabled {
        Ok(info) => info,
        Err(_) => panic!("valid disabled code-execution configuration was rejected"),
    };
    assert_eq!(disabled.provider, None);
    assert!(!disabled.available);

    let local = update_config(
        &host_config,
        &store,
        &secrets,
        CodeExecutionConfigUpdate {
            provider: Some(Some(CodeExecutionProviderKind::Local)),
            timeout_ms: Some(MAX_TIMEOUT_MS),
            egress: None,
            e2b_template: None,
            daytona_snapshot: None,
        },
    )
    .await;
    let local = match local {
        Ok(info) => info,
        Err(_) => panic!("valid local code-execution configuration was rejected"),
    };
    assert_eq!(local.provider, Some(CodeExecutionProviderKind::Local));
    assert_eq!(local.timeout_ms, MAX_TIMEOUT_MS);
}

#[tokio::test]
async fn unavailable_providers_report_an_actionable_reason() {
    let (store, dir) = test_store().await;
    let host_config = openwave_core::Config::desktop(dir.path());
    let info = config_info(&host_config, &store, &NoSecrets).await.unwrap();
    let rows: HashMap<_, _> = info
        .providers
        .iter()
        .map(|row| (row.provider, *row))
        .collect();

    // No key is saved here, so a managed provider must say exactly that —
    // "paste a key" has to be readable off the report, not inferred from a
    // failed execution.
    for managed in CREDENTIAL_PROVIDERS {
        let row = rows[&managed];
        assert!(!row.available);
        assert_eq!(
            row.unavailable_reason,
            Some(CodeExecutionUnavailableReason::MissingCredential)
        );
    }
    let local = rows[&CodeExecutionProviderKind::Local];
    assert_eq!(
        local.unavailable_reason,
        LocalExecutionProvider::availability().err()
    );

    // The untouched host selects a provider only where one can actually
    // run: on a host without the local sandbox the report is "nothing
    // configured" plus the reasons above, never a dead Local selection.
    assert_eq!(info.provider.is_some(), local.available);
    assert_eq!(info.available, local.available);
}

#[tokio::test]
async fn workspace_capability_degrades_to_none_instead_of_failing() {
    let (store, dir) = test_store().await;
    let provider = ConfiguredCodeExecutionProvider::new(
        Arc::new(store),
        Arc::new(NoSecrets),
        dir.path().join("scratch"),
    );
    // Selected explicitly: the default no longer picks Local on hosts
    // without the native sandbox, and this case is about the workspace
    // surface, not about what a fresh host defaults to.
    provider
        .store
        .set_setting(
            CODE_EXECUTION_SETTING,
            &serde_json::json!({ "provider": "local", "timeout_ms": DEFAULT_TIMEOUT_MS }),
        )
        .await
        .unwrap();
    assert!(provider.workspace().await.unwrap().is_some());

    // Disabling execution and selecting a managed provider without a
    // credential must both report "no workspace", not an error.
    provider
        .store
        .set_setting(
            CODE_EXECUTION_SETTING,
            &serde_json::json!({ "provider": null, "timeout_ms": DEFAULT_TIMEOUT_MS }),
        )
        .await
        .unwrap();
    assert!(provider.workspace().await.unwrap().is_none());

    provider
        .store
        .set_setting(
            CODE_EXECUTION_SETTING,
            &serde_json::json!({ "provider": "e2b", "timeout_ms": DEFAULT_TIMEOUT_MS }),
        )
        .await
        .unwrap();
    assert!(provider.workspace().await.unwrap().is_none());
}

#[tokio::test]
async fn invalid_persisted_policy_fails_closed() {
    let (store, _dir) = test_store().await;
    store
        .set_setting(
            CODE_EXECUTION_SETTING,
            &serde_json::json!({
                "provider": "local",
                "timeout_ms": MAX_TIMEOUT_MS + 1,
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        read_config(&store).await.unwrap(),
        CodeExecutionConfig::disabled()
    );
}
