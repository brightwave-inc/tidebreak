use std::path::Path;
use std::sync::Arc;

use super::private_scratch::{read_utf8_file, write_utf8_file, MAX_READ_FILE_BYTES};
use super::{ListDir, ReadFile, WriteFile};
use crate::id::SessionId;
use crate::model::Chat;
use crate::storage::Store;
use crate::tool::{Tool, ToolCtx};
use crate::DbStore;
use serde_json::json;

fn ctx(dir: &Path) -> ToolCtx {
    ToolCtx::try_new_legacy_workspace(SessionId::new(), None, dir.to_path_buf()).unwrap()
}

async fn output_fixture() -> (tempfile::TempDir, Arc<DbStore>, SessionId) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("outputs.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    (directory, store, chat.id)
}

#[tokio::test]
async fn every_file_tool_fails_closed_without_private_scratch() {
    let (_directory, _store, chat_id) = output_fixture().await;
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

    assert!(read.is_error);
    assert!(list.is_error);
    assert!(write.is_error);
}

#[tokio::test]
async fn built_in_tool_schemas_preserve_their_provider_contracts() {
    let (_directory, _store, _chat_id) = output_fixture().await;
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
            "required": ["path"],
            "additionalProperties": false
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
            },
            "additionalProperties": false
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
                },
                // Display-only narration, required of the model so a card
                // can lead with it rather than with the path.
                "summary": {
                    "type": "string",
                    "maxLength": crate::preview::MAX_ACTION_SUMMARY_CHARS,
                    "description": crate::SUMMARY_ARGUMENT_DESCRIPTION
                }
            },
            "required": ["summary", "path", "content"],
            "additionalProperties": false
        })
    );
}

#[tokio::test]
async fn unknown_arguments_are_refused_not_ignored() {
    let output = ReadFile
        .execute(
            &ToolCtx::without_private_scratch(SessionId::new(), None),
            json!({"path": "note.txt", "encoding": "utf-8"}),
        )
        .await
        .unwrap();

    assert!(output.is_error);
    assert!(output.content.contains("invalid arguments:"));
    assert!(output.content.contains("unknown field"));
}

#[tokio::test]
async fn malformed_arguments_include_the_typed_schema() {
    let output = ReadFile
        .execute(
            &ToolCtx::without_private_scratch(SessionId::new(), None),
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

/// A direct write to `output/` must be refused rather than silently staged.
///
/// The published-output directory is scanned after an exec call, and every
/// revision it finds is attributed to that call and its turn. Bytes left there
/// by `write_file` publish nothing of their own and would be credited to the
/// next unrelated exec call, so the write has to fail at the boundary.
#[tokio::test]
async fn write_file_refuses_the_published_output_directory() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path());

    for path in ["output/report.md", "./output/nested/report.md", "output"] {
        let refused = WriteFile
            .execute(&ctx, json!({"path": path, "content": "published?"}))
            .await
            .unwrap();
        assert!(refused.is_error, "{path}: {refused:?}");
        assert!(refused.content.contains("exec"), "{path}: {refused:?}");
    }
    assert!(!dir.path().join("output").exists());
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

/// The write cap is a contract the model is told about and routes around, so
/// it has to refuse rather than truncate, and it has to name the exec/output
/// path that handles content this size.
#[tokio::test]
async fn write_rejects_content_over_the_shared_write_cap() {
    let dir = tempfile::tempdir().unwrap();
    let content = "x".repeat(crate::MAX_WRITE_FILE_BYTES + 1);

    let write = WriteFile
        .execute(
            &ctx(dir.path()),
            json!({"path": "big.txt", "content": content}),
        )
        .await
        .unwrap();

    assert!(write.is_error, "{write:?}");
    assert!(write.content.contains("output"), "{write:?}");
    assert!(!dir.path().join("big.txt").exists());
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
