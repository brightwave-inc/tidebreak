use std::path::Path;

use serde_json::json;

use super::private_scratch::{read_utf8_file, write_utf8_file, MAX_READ_FILE_BYTES};
use super::{ListDir, ReadFile, WriteFile};
use crate::id::ChatId;
use crate::tool::{Tool, ToolCtx};

fn ctx(dir: &Path) -> ToolCtx {
    ToolCtx::try_new_legacy_workspace(ChatId::new(), None, dir.to_path_buf()).unwrap()
}

#[tokio::test]
async fn every_file_tool_fails_closed_without_private_scratch() {
    let ctx = ToolCtx::without_private_scratch(ChatId::new(), None);

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
