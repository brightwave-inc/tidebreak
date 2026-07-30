use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};

use super::private_scratch::{read_utf8_file, write_utf8_file, MAX_READ_FILE_BYTES};
use super::{CreateDeliverable, ListDir, ReadFile, WriteFile};
use crate::id::{CallId, ChatId, OutputId, OutputRevisionId, TurnId};
use crate::model::{Chat, ToolCallExecution, ToolCallRecord, ToolCallStatus};
use crate::storage::Store;
use crate::tool::{Tool, ToolCtx};
use crate::DbStore;

fn ctx(dir: &Path) -> ToolCtx {
    ToolCtx::try_new_legacy_workspace(ChatId::new(), None, dir.to_path_buf()).unwrap()
}

async fn output_fixture() -> (tempfile::TempDir, Arc<DbStore>, ChatId) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("outputs.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    (directory, store, chat.id)
}

async fn accept_deliverable_call(store: &DbStore, chat_id: ChatId, call_id: CallId) -> TurnId {
    let turn_id = TurnId::new();
    store
        .accept_tool_call(&ToolCallRecord {
            id: call_id,
            chat_id,
            turn_id,
            provider_id: format!("provider-{call_id}"),
            name: "create_deliverable".into(),
            arguments: json!({}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        })
        .await
        .unwrap();
    turn_id
}

fn durable_ctx(directory: &Path, chat_id: ChatId, call_id: CallId) -> ToolCtx {
    ToolCtx::try_new_legacy_workspace(chat_id, None, directory.to_path_buf())
        .unwrap()
        .with_call_id(call_id)
}

fn published_ids(output: &crate::ToolOutput) -> (OutputId, OutputRevisionId) {
    let data = output.data.as_ref().expect("published output data");
    (
        serde_json::from_value(data["output_id"].clone()).unwrap(),
        serde_json::from_value(data["revision_id"].clone()).unwrap(),
    )
}

#[tokio::test]
async fn every_file_tool_fails_closed_without_private_scratch() {
    let (_directory, store, chat_id) = output_fixture().await;
    let ctx = ToolCtx::without_private_scratch(chat_id, None);

    let read = ReadFile
        .execute(&ctx, json!({"path": "note.txt"}))
        .await
        .unwrap();
    let list = ListDir.execute(&ctx, json!({})).await.unwrap();
    let write = WriteFile
        .execute(&ctx, json!({"path": "note.txt", "content": "nope"}))
        .await
        .unwrap();
    let deliverable = CreateDeliverable::new(store)
        .execute(&ctx, json!({"filename": "brief.md", "content": "nope"}))
        .await
        .unwrap();

    assert!(read.is_error);
    assert!(list.is_error);
    assert!(write.is_error);
    assert!(deliverable.is_error);
}

#[tokio::test]
async fn built_in_tool_schemas_preserve_their_provider_contracts() {
    let (_directory, store, _chat_id) = output_fixture().await;
    assert_eq!(
        ReadFile.spec().input_schema,
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Private-scratch-relative file path."
                }
            },
            "required": ["path"]
        })
    );
    assert_eq!(
        ListDir.spec().input_schema,
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Private-scratch-relative directory (optional)."
                }
            }
        })
    );
    assert_eq!(
        WriteFile.spec().input_schema,
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Private-scratch-relative file path."
                },
                "content": {
                    "type": "string",
                    "description": "File contents to write."
                }
            },
            "required": ["path", "content"]
        })
    );
    assert_eq!(
        CreateDeliverable::new(store).spec().input_schema,
        json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Portable output filename ending in .md, .txt, .csv, .json, or .html.",
                    "minLength": 1,
                    "maxLength": crate::MAX_DELIVERABLE_NAME_CHARS
                },
                "content": {
                    "type": "string",
                    "description": "Complete UTF-8 text contents of the output file (maximum 512 KiB).",
                    "minLength": 1,
                    "maxLength": crate::MAX_DELIVERABLE_BYTES
                },
                "output_id": {
                    "type": ["string", "null"],
                    "description": "Opaque output id returned by an earlier call. Omit this to create a new output."
                }
            },
            "required": ["filename", "content"],
            "additionalProperties": false
        })
    );
}

#[tokio::test]
async fn malformed_arguments_include_the_typed_schema() {
    let output = ReadFile
        .execute(
            &ToolCtx::without_private_scratch(ChatId::new(), None),
            json!({"path": 42}),
        )
        .await
        .unwrap();

    assert!(output.is_error);
    assert!(output.content.contains("invalid arguments:"));
    assert!(output.content.contains("Expected schema:"));
    assert!(output
        .content
        .contains("Private-scratch-relative file path."));
    assert!(output.content.contains("\"required\": ["));
}

#[tokio::test]
async fn deliverables_are_isolated_in_their_closed_directory() {
    let (dir, store, chat_id) = output_fixture().await;
    let first_call = CallId::new();
    let first_turn = accept_deliverable_call(&store, chat_id, first_call).await;
    let ctx = durable_ctx(dir.path(), chat_id, first_call);
    let tool = CreateDeliverable::new(store.clone());
    let spec = tool.spec();
    assert_eq!(
        spec.input_schema["properties"]["filename"]["maxLength"],
        crate::MAX_DELIVERABLE_NAME_CHARS
    );
    assert_eq!(
        spec.input_schema["properties"]["content"]["maxLength"],
        crate::MAX_DELIVERABLE_BYTES
    );
    assert_eq!(spec.input_schema["additionalProperties"], false);

    let output = tool
        .execute(
            &ctx,
            json!({"filename": "Research brief.md", "content": "# Findings\n\nGrounded."}),
        )
        .await
        .unwrap();
    assert!(!output.is_error, "{output:?}");
    let (output_id, first_revision_id) = published_ids(&output);
    assert_eq!(
        std::fs::read_to_string(
            dir.path()
                .join(crate::OUTPUTS_DIRECTORY)
                .join(output_id.to_string())
                .join(first_revision_id.to_string()),
        )
        .unwrap(),
        "# Findings\n\nGrounded."
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("artifacts/Research brief.md")).unwrap(),
        "# Findings\n\nGrounded."
    );
    let first = store.get_output(output_id).await.unwrap().unwrap();
    let first_revision = store
        .get_output_revision(first_revision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.chat_id, chat_id);
    assert_eq!(first.current_revision, first_revision_id);
    assert_eq!(first_revision.turn_id, Some(first_turn));
    assert_eq!(
        first_revision.byte_len,
        "# Findings\n\nGrounded.".len() as u64
    );
    let first_digest: [u8; 32] = Sha256::digest(b"# Findings\n\nGrounded.").into();
    assert_eq!(first_revision.sha256, first_digest);
    assert!(output.content.contains(&output_id.to_string()));
    assert!(output.content.contains(&first_revision_id.to_string()));
    assert!(!output.content.contains(dir.path().to_str().unwrap()));
    assert!(!output.content.contains("outputs/"));

    // The same canonical call is an exact retry, not a second publication.
    let retried = tool
        .execute(
            &ctx,
            json!({"filename": "Research brief.md", "content": "# Findings\n\nGrounded."}),
        )
        .await
        .unwrap();
    assert_eq!(published_ids(&retried), (output_id, first_revision_id));
    assert_eq!(
        store.list_output_revisions(output_id).await.unwrap().len(),
        1
    );

    let second_call = CallId::new();
    let second_turn = accept_deliverable_call(&store, chat_id, second_call).await;
    let update = tool
        .execute(
            &durable_ctx(dir.path(), chat_id, second_call),
            json!({
                "output_id": output_id,
                "filename": "Research brief.md",
                "content": "# Findings\n\nRevised."
            }),
        )
        .await
        .unwrap();
    assert!(!update.is_error, "{update:?}");
    let (updated_output_id, second_revision_id) = published_ids(&update);
    assert_eq!(updated_output_id, output_id);
    let updated = store.get_output(output_id).await.unwrap().unwrap();
    assert_eq!(updated.current_revision, second_revision_id);
    assert_eq!(updated.revision_count, 2);
    let revisions = store.list_output_revisions(output_id).await.unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].id, second_revision_id);
    assert_eq!(revisions[0].ordinal, 2);
    assert_eq!(revisions[0].turn_id, Some(second_turn));
    assert_eq!(revisions[1].id, first_revision_id);
    assert_eq!(
        std::fs::read_to_string(
            dir.path()
                .join(crate::OUTPUTS_DIRECTORY)
                .join(output_id.to_string())
                .join(first_revision_id.to_string()),
        )
        .unwrap(),
        "# Findings\n\nGrounded."
    );
    assert_eq!(
        std::fs::read_to_string(
            dir.path()
                .join(crate::OUTPUTS_DIRECTORY)
                .join(output_id.to_string())
                .join(second_revision_id.to_string()),
        )
        .unwrap(),
        "# Findings\n\nRevised."
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("artifacts/Research brief.md")).unwrap(),
        "# Findings\n\nRevised."
    );
    let retried_update = tool
        .execute(
            &durable_ctx(dir.path(), chat_id, second_call),
            json!({
                "output_id": output_id,
                "filename": "Research brief.md",
                "content": "# Findings\n\nRevised."
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        published_ids(&retried_update),
        (output_id, second_revision_id)
    );
    assert_eq!(
        store.list_output_revisions(output_id).await.unwrap().len(),
        2
    );

    for arguments in [
        json!({"filename": "../escape.md", "content": "nope"}),
        json!({"filename": "opaque.pdf", "content": "nope"}),
        json!({"filename": "empty.txt", "content": ""}),
        json!({"filename": "bad-id.txt", "content": "x", "output_id": "not-an-id"}),
        json!({"filename": "extra.txt", "content": "x", "path": "outside"}),
    ] {
        assert!(tool.execute(&ctx, arguments).await.unwrap().is_error);
    }
}

#[tokio::test]
async fn deliverable_size_is_bounded_before_writing() {
    let (dir, store, chat_id) = output_fixture().await;
    let call_id = CallId::new();
    accept_deliverable_call(&store, chat_id, call_id).await;
    let output = CreateDeliverable::new(store)
        .execute(
            &durable_ctx(dir.path(), chat_id, call_id),
            json!({
                "filename": "oversized.txt",
                "content": "x".repeat(crate::MAX_DELIVERABLE_BYTES + 1)
            }),
        )
        .await
        .unwrap();
    assert!(output.is_error);
    assert!(!dir.path().join(crate::OUTPUTS_DIRECTORY).exists());
}

#[tokio::test]
async fn deliverable_updates_fail_closed_across_conversations() {
    let (first_directory, store, first_chat) = output_fixture().await;
    let first_call = CallId::new();
    accept_deliverable_call(&store, first_chat, first_call).await;
    let tool = CreateDeliverable::new(store.clone());
    let created = tool
        .execute(
            &durable_ctx(first_directory.path(), first_chat, first_call),
            json!({"filename": "brief.md", "content": "private"}),
        )
        .await
        .unwrap();
    let (output_id, _) = published_ids(&created);

    let second_directory = tempfile::tempdir().unwrap();
    let second_chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&second_chat).await.unwrap();
    let second_call = CallId::new();
    accept_deliverable_call(&store, second_chat.id, second_call).await;
    let intruder = tool
        .execute(
            &durable_ctx(second_directory.path(), second_chat.id, second_call),
            json!({
                "output_id": output_id,
                "filename": "brief.md",
                "content": "stolen"
            }),
        )
        .await
        .unwrap();
    assert!(intruder.is_error, "{intruder:?}");
    assert!(intruder.content.contains("another conversation"));
    assert!(!second_directory
        .path()
        .join(crate::OUTPUTS_DIRECTORY)
        .exists());
    assert_eq!(
        store.list_output_revisions(output_id).await.unwrap().len(),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn deliverable_publication_rejects_a_symlinked_revision_directory() {
    let (directory, store, chat_id) = output_fixture().await;
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        directory.path().join(crate::OUTPUTS_DIRECTORY),
    )
    .unwrap();
    let call_id = CallId::new();
    accept_deliverable_call(&store, chat_id, call_id).await;

    let output = CreateDeliverable::new(store.clone())
        .execute(
            &durable_ctx(directory.path(), chat_id, call_id),
            json!({"filename": "brief.md", "content": "must stay private"}),
        )
        .await
        .unwrap();

    assert!(output.is_error, "{output:?}");
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    assert!(store.list_outputs(chat_id, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn write_then_read_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path());

    let out = WriteFile
        .execute(&ctx, json!({"path": "notes/todo.txt", "content": "hello"}))
        .await
        .unwrap();
    assert!(!out.is_error, "{out:?}");

    let read = ReadFile
        .execute(&ctx, json!({"path": "notes/todo.txt"}))
        .await
        .unwrap();
    assert_eq!(read.content, "hello");
    assert!(!read.is_error);

    let listing = ListDir
        .execute(&ctx, json!({"path": "notes"}))
        .await
        .unwrap();
    assert_eq!(listing.content, "todo.txt");
}

#[tokio::test]
async fn confinement_rejects_escaping_and_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path());

    let escape = ReadFile
        .execute(&ctx, json!({"path": "../secret"}))
        .await
        .unwrap();
    assert!(escape.is_error);

    let absolute = WriteFile
        .execute(&ctx, json!({"path": "/etc/passwd", "content": "x"}))
        .await
        .unwrap();
    assert!(absolute.is_error);
}

#[cfg(unix)]
#[tokio::test]
async fn confinement_rejects_symlink_escape() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("link")).unwrap();
    let ctx = ctx(workspace.path());

    let read = ReadFile
        .execute(&ctx, json!({"path": "link/secret.txt"}))
        .await
        .unwrap();
    assert!(
        read.is_error,
        "symlinked-dir read should be rejected: {read:?}"
    );

    let write = WriteFile
        .execute(&ctx, json!({"path": "link/pwn.txt", "content": "x"}))
        .await
        .unwrap();
    assert!(
        write.is_error,
        "symlinked-dir write should be rejected: {write:?}"
    );
    assert!(!outside.path().join("pwn.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_capability_survives_root_path_retargeting() {
    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("note.txt"), "original").unwrap();
    let ctx = ctx(&workspace);

    let original = parent.path().join("original");
    std::fs::rename(&workspace, &original).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("note.txt"), "replacement").unwrap();

    let read = ReadFile
        .execute(&ctx, json!({"path": "note.txt"}))
        .await
        .unwrap();
    assert_eq!(read.content, "original");
    assert!(!read.is_error);
}

#[cfg(unix)]
#[tokio::test]
async fn read_rejects_a_fifo_without_blocking() {
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(dir.path().join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());
    let workspace = ctx(dir.path()).workspace().unwrap();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = read_utf8_file(&workspace, Path::new("pipe"));
        let _ = send.send(result);
    });

    let result = receive
        .recv_timeout(Duration::from_secs(2))
        .expect("FIFO read must not block");
    assert!(result.unwrap_err().contains("regular file"));
    worker.join().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn write_rejects_a_fifo_without_blocking() {
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(dir.path().join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());
    let workspace = ctx(dir.path()).workspace().unwrap();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = write_utf8_file(&workspace, Path::new("pipe"), b"content");
        let _ = send.send(result);
    });

    let error = receive
        .recv_timeout(Duration::from_secs(2))
        .expect("FIFO write must not block")
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    worker.join().unwrap();
    assert!(dir.path().join("pipe").exists());
}

#[tokio::test]
async fn failed_atomic_write_preserves_the_target_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("target/keep.txt"), "keep").unwrap();

    let write = WriteFile
        .execute(
            &ctx(dir.path()),
            json!({"path": "target", "content": "replace"}),
        )
        .await
        .unwrap();
    assert!(write.is_error);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target/keep.txt")).unwrap(),
        "keep"
    );
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from("target")]);
}

#[tokio::test]
async fn atomic_write_replaces_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), "old").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(dir.path().join("note.txt"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(dir.path().join("note.txt"), permissions).unwrap();
    }

    let write = WriteFile
        .execute(
            &ctx(dir.path()),
            json!({"path": "note.txt", "content": "new"}),
        )
        .await
        .unwrap();
    assert!(!write.is_error, "{write:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "new"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.path().join("note.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[tokio::test]
async fn read_rejects_files_over_the_output_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("large.txt"),
        vec![b'x'; MAX_READ_FILE_BYTES + 1],
    )
    .unwrap();

    let read = ReadFile
        .execute(&ctx(dir.path()), json!({"path": "large.txt"}))
        .await
        .unwrap();
    assert!(read.is_error);
    assert!(read.content.contains("too large"), "{read:?}");
}

#[tokio::test]
async fn missing_file_is_a_model_facing_error_not_err() {
    let dir = tempfile::tempdir().unwrap();
    let output = ReadFile
        .execute(&ctx(dir.path()), json!({"path": "nope.txt"}))
        .await
        .unwrap();
    assert!(output.is_error);
}
