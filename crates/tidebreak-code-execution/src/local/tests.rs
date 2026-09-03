use super::*;
#[cfg(target_os = "macos")]
use crate::{ExecutionId, ExecutionWorkspaceId};

#[test]
fn sandbox_path_denied_message_teaches_attach_or_connect_folder_recovery() {
    let without_grants = sandbox_path_denied_message(false);
    assert!(
        without_grants.starts_with(SANDBOX_PATH_DENIED_CODE),
        "{without_grants}"
    );
    assert!(
        without_grants.contains("path/capability error"),
        "{without_grants}"
    );
    assert!(
        without_grants.contains("not a safety refusal"),
        "{without_grants}"
    );
    assert!(
        without_grants.contains("no connected folders are currently available"),
        "{without_grants}"
    );
    assert!(
        without_grants.contains("attach or copy"),
        "{without_grants}"
    );
    assert!(
        without_grants.contains("connect its containing folder"),
        "{without_grants}"
    );
    assert!(
        without_grants.contains("tell the user what access is missing"),
        "{without_grants}"
    );

    let with_grants = sandbox_path_denied_message(true);
    assert!(
        with_grants.contains("connected folders are available"),
        "{with_grants}"
    );
    assert!(
        !with_grants.contains("no connected folders are currently available"),
        "{with_grants}"
    );
    assert!(with_grants.contains("attach or copy"), "{with_grants}");
    assert!(
        with_grants.contains("connect its containing folder"),
        "{with_grants}"
    );
    assert!(
        with_grants.contains("tell the user what access is missing"),
        "{with_grants}"
    );
}

#[cfg(target_os = "macos")]
fn request(workspace: &str, execution: &str, script: &str) -> ExecRequest {
    ExecRequest::new(
        ExecutionId::parse(execution).unwrap(),
        ExecutionWorkspaceId::parse(workspace).unwrap(),
        "/bin/sh",
        vec!["-c".into(), script.into()],
        ".",
    )
    .unwrap()
}

#[cfg(target_os = "macos")]
fn fixture(timeout: Duration) -> (tempfile::TempDir, LocalExecutionProvider, String) {
    let root = tempfile::tempdir().unwrap();
    let workspace = "chat-1".to_string();
    fs::create_dir(root.path().join(&workspace)).unwrap();
    let provider = LocalExecutionProvider::new(root.path(), timeout).unwrap();
    (root, provider, workspace)
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn direct_host_path_denial_is_actionable_and_distinct_from_workspace_enoent() {
    let (root, provider, workspace) = fixture(Duration::from_secs(3));
    let connected_path = root.path().join("sentinel-connected-folder-do-not-leak");
    fs::create_dir(&connected_path).unwrap();
    let connected_path = fs::canonicalize(connected_path).unwrap();
    let grants = vec![ExecFolderGrant::new(&connected_path, ExecFolderAccess::ReadOnly).unwrap()];

    let denied_path = "/tmp/sentinel-denied-path-do-not-leak.md";
    let denied = ExecRequest::new(
        ExecutionId::parse("call-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/bin/cat",
        vec![denied_path.into()],
        ".",
    )
    .unwrap()
    .with_folder_grants(grants)
    .unwrap();

    let first = provider.execute(denied.clone()).await.unwrap();
    let replay = provider.execute(denied).await.unwrap();
    assert_eq!(first, replay);
    assert_eq!(first.provider, ExecProviderKind::Local);
    assert_eq!(first.exit_code, Some(126));
    assert!(first.stderr.contains(SANDBOX_PATH_DENIED_CODE));
    assert!(first.stderr.contains("available capabilities"));
    assert!(first.stderr.contains("connected folders are available"));
    assert!(first.stderr.contains("attach or copy"));
    assert!(!first.stderr.contains(denied_path));
    assert!(!first.stderr.contains("sentinel-denied-path-do-not-leak"));
    assert!(!first
        .stderr
        .contains("sentinel-connected-folder-do-not-leak"));
    assert!(!first.stderr.contains(&connected_path.display().to_string()));

    let changed = ExecRequest::new(
        ExecutionId::parse("call-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/bin/cat",
        vec!["/tmp/a-different-file".into()],
        ".",
    )
    .unwrap();
    assert!(matches!(
        provider.execute(changed).await.unwrap_err(),
        ExecError::IdentityConflict
    ));

    let missing = ExecRequest::new(
        ExecutionId::parse("call-missing-workspace-file").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/bin/cat",
        vec!["incident-notes.md".into()],
        ".",
    )
    .unwrap();
    let response = provider.execute(missing).await.unwrap();
    assert_ne!(response.exit_code, Some(0));
    assert!(response.stderr.contains("No such file or directory"));
    assert!(!response.stderr.contains(SANDBOX_PATH_DENIED_CODE));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn failed_shell_access_to_denied_host_path_gets_the_stable_result_code() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let response = provider
        .execute(request(
            &workspace,
            "call-shell-denied-path",
            "cat /tmp/tidebreak-incident-notes.md",
        ))
        .await
        .unwrap();

    assert_ne!(response.exit_code, Some(0));
    assert!(
        response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE),
        "unexpected stderr: {:?}",
        response.stderr
    );
    assert!(response.stderr.contains("available capabilities"));
    assert!(response
        .stderr
        .contains("no connected folders are currently available"));
    assert!(!response.stderr.contains("/tmp/tidebreak-incident-notes.md"));
    assert!(!response.stderr.contains("Operation not permitted"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn dynamically_constructed_host_path_denial_does_not_leak_process_diagnostics() {
    let (root, provider, workspace) = fixture(Duration::from_secs(3));
    let connected = root
        .path()
        .join("sentinel-dynamic-connected-folder-do-not-leak");
    fs::create_dir(&connected).unwrap();
    let connected = fs::canonicalize(connected).unwrap();
    let request = ExecRequest::new(
        ExecutionId::parse("call-dynamic-python-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/usr/bin/python3",
        vec![
            "-c".into(),
            "open('/' + 'tmp' + '/sentinel-dynamic-denied-path-do-not-leak').read()".into(),
        ],
        ".",
    )
    .unwrap()
    .with_folder_grants(vec![ExecFolderGrant::new(
        &connected,
        ExecFolderAccess::ReadOnly,
    )
    .unwrap()])
    .unwrap();

    let response = provider.execute(request).await.unwrap();

    assert_ne!(response.exit_code, Some(0));
    assert!(
        response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE),
        "unexpected stderr: {:?}",
        response.stderr
    );
    assert!(response.stderr.contains("available capabilities"));
    assert!(response.stderr.contains("connected folders are available"));
    assert!(!response.stderr.contains("/tmp/"));
    assert!(!response
        .stderr
        .contains("sentinel-dynamic-denied-path-do-not-leak"));
    assert!(!response
        .stderr
        .contains("sentinel-dynamic-connected-folder-do-not-leak"));
    assert!(!response.stderr.contains(&connected.display().to_string()));
    assert!(!response.stderr.contains("Operation not permitted"));
    assert!(!response.stderr.contains("PermissionError"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn redirected_shell_path_denial_does_not_leak_through_stdout() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let denied_path = "/tmp/sentinel-redirected-denied-path-do-not-leak";
    let response = provider
        .execute(request(
            &workspace,
            "call-redirected-shell-denied-path",
            &format!("cat {denied_path} 2>&1; exit 31"),
        ))
        .await
        .unwrap();

    assert_eq!(response.exit_code, Some(31));
    assert!(response.stdout.is_empty());
    assert!(response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE));
    assert!(!response.stderr.contains(denied_path));
    assert!(!response.stderr.contains("Operation not permitted"));
    assert!(!response.stderr.contains("cat:"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn caught_python_path_denial_does_not_leak_through_stdout() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let denied_path = "/tmp/sentinel-caught-python-denied-path-do-not-leak";
    let script = "try:\n    open('/' + 'tmp' + '/sentinel-caught-python-denied-path-do-not-leak').read()\nexcept PermissionError as error:\n    print(error)\n    raise SystemExit(19)";
    let request = ExecRequest::new(
        ExecutionId::parse("call-caught-python-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/usr/bin/python3",
        vec!["-c".into(), script.into()],
        ".",
    )
    .unwrap();

    let response = provider.execute(request).await.unwrap();

    assert_eq!(response.exit_code, Some(19));
    assert!(response.stdout.is_empty());
    assert!(response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE));
    assert!(!response.stderr.contains(denied_path));
    assert!(!response.stderr.contains("Operation not permitted"));
    assert!(!response.stderr.contains("PermissionError"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn caught_python_path_denial_does_not_leak_after_successful_exit() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let denied_path = "/tmp/sentinel-caught-success-denied-path-do-not-leak";
    let script = "path = '/' + 'tmp' + '/sentinel-caught-success-denied-path-do-not-leak'\ntry:\n    open(path).read()\nexcept PermissionError:\n    print(f'Operation not permitted: x{path}')";
    let request = ExecRequest::new(
        ExecutionId::parse("call-caught-success-python-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/usr/bin/python3",
        vec!["-c".into(), script.into()],
        ".",
    )
    .unwrap();

    let response = provider.execute(request).await.unwrap();

    assert_eq!(response.exit_code, Some(0));
    assert!(response.stdout.is_empty());
    assert!(response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE));
    assert!(!response.stderr.contains(denied_path));
    assert!(!response.stderr.contains("Operation not permitted"));
    assert!(!response.stderr.contains("PermissionError"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn malformed_url_shaped_denied_path_is_normalized_after_successful_exit() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let denied_path = "/tmp/sentinel-malformed-url-denied-path-do-not-leak";
    let script = r#"path = "/" + "tmp" + "/sentinel-malformed-url-denied-path-do-not-leak"
try:
open(path).read()
except PermissionError as error:
print("Operation not permitted: https://" + error.filename)"#;
    let request = ExecRequest::new(
        ExecutionId::parse("call-malformed-url-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/usr/bin/python3",
        vec!["-c".into(), script.into()],
        ".",
    )
    .unwrap();

    let response = provider.execute(request).await.unwrap();

    assert_eq!(response.exit_code, Some(0));
    assert!(response.stdout.is_empty());
    assert!(response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE));
    assert!(!response.stderr.contains(denied_path));
    assert!(!response.stderr.contains("https:///tmp"));
    assert!(!response.stderr.contains("Operation not permitted"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn escaped_denied_paths_are_normalized_across_output_channels() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let cases = [
        (
            "call-escaped-slash-denied-path",
            r#"import sys
path = "/" + "tmp" + "/sentinel-escaped-slash-denied-path-do-not-leak"
try:
open(path).read()
except PermissionError as error:
print("Operation not permitted")
print(error.filename.replace("/", "\\/"), file=sys.stderr)"#,
            "sentinel-escaped-slash-denied-path-do-not-leak",
        ),
        (
            "call-unicode-slash-denied-path",
            r#"path = "/" + "tmp" + "/sentinel-unicode-slash-denied-path-do-not-leak"
try:
open(path).read()
except PermissionError as error:
print("Operation not permitted: " + error.filename.replace("/", "\\u002F"))"#,
            "sentinel-unicode-slash-denied-path-do-not-leak",
        ),
    ];

    for (execution_id, script, sentinel) in cases {
        let request = ExecRequest::new(
            ExecutionId::parse(execution_id).unwrap(),
            ExecutionWorkspaceId::parse(&workspace).unwrap(),
            "/usr/bin/python3",
            vec!["-c".into(), script.into()],
            ".",
        )
        .unwrap();

        let response = provider.execute(request).await.unwrap();

        assert_eq!(response.exit_code, Some(0), "case {execution_id}");
        assert!(response.stdout.is_empty(), "case {execution_id}");
        assert!(
            response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE),
            "case {execution_id}: {:?}",
            response.stderr
        );
        assert!(!response.stderr.contains(sentinel), "case {execution_id}");
        assert!(
            !response.stderr.contains("Operation not permitted"),
            "case {execution_id}"
        );
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn file_url_denied_path_is_normalized_after_successful_exit() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let denied_path = "/tmp/sentinel-file-url-denied-path-do-not-leak";
    let script = r#"import sys
path = "/" + "tmp" + "/sentinel-file-url-denied-path-do-not-leak"
try:
open(path).read()
except PermissionError as error:
print("Operation not permitted: file://" + error.filename, file=sys.stderr)"#;
    let request = ExecRequest::new(
        ExecutionId::parse("call-file-url-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/usr/bin/python3",
        vec!["-c".into(), script.into()],
        ".",
    )
    .unwrap();

    let response = provider.execute(request).await.unwrap();

    assert_eq!(response.exit_code, Some(0));
    assert!(response.stdout.is_empty());
    assert!(response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE));
    assert!(!response.stderr.contains(denied_path));
    assert!(!response.stderr.contains("file:///tmp"));
    assert!(!response.stderr.contains("Operation not permitted"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn prefixed_denied_path_across_output_channels_is_normalized_on_failure() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let denied_path = "/tmp/sentinel-cross-channel-denied-path-do-not-leak";
    let script = "import sys\npath = '/' + 'tmp' + '/sentinel-cross-channel-denied-path-do-not-leak'\ntry:\n    open(path).read()\nexcept PermissionError:\n    print('Operation not permitted')\n    print(f'x{path}', file=sys.stderr)\n    raise SystemExit(29)";
    let request = ExecRequest::new(
        ExecutionId::parse("call-cross-channel-python-denied-path").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/usr/bin/python3",
        vec!["-c".into(), script.into()],
        ".",
    )
    .unwrap();

    let response = provider.execute(request).await.unwrap();

    assert_eq!(response.exit_code, Some(29));
    assert!(response.stdout.is_empty());
    assert!(response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE));
    assert!(!response.stderr.contains(denied_path));
    assert!(!response.stderr.contains("Operation not permitted"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn shell_tilde_expansion_denial_does_not_leak_the_expanded_host_path() {
    use std::os::unix::fs::symlink;

    let (root, provider, workspace) = fixture(Duration::from_secs(3));
    let connected = root
        .path()
        .join("sentinel-tilde-connected-folder-do-not-leak");
    fs::create_dir(&connected).unwrap();
    let connected = fs::canonicalize(connected).unwrap();
    let env_home = root.path().join(ENV_HOME_DIR).join(&workspace);
    fs::create_dir_all(&env_home).unwrap();
    let expanded_link = env_home.join("sentinel-tilde-link-do-not-leak");
    symlink("/tmp", &expanded_link).unwrap();
    let expanded_path = expanded_link.join("sentinel-tilde-denied-path-do-not-leak");
    let request = request(
        &workspace,
        "call-shell-tilde-denied-path",
        "cat ~/sentinel-tilde-link-do-not-leak/sentinel-tilde-denied-path-do-not-leak",
    )
    .with_folder_grants(vec![ExecFolderGrant::new(
        &connected,
        ExecFolderAccess::ReadOnly,
    )
    .unwrap()])
    .unwrap();

    let response = provider.execute(request).await.unwrap();

    assert_ne!(response.exit_code, Some(0));
    assert!(
        response.stderr.starts_with(SANDBOX_PATH_DENIED_CODE),
        "unexpected tilde stderr: {:?}",
        response.stderr
    );
    assert!(response.stderr.contains("available capabilities"));
    assert!(response.stderr.contains("connected folders are available"));
    assert!(!response.stderr.contains(&env_home.display().to_string()));
    assert!(!response
        .stderr
        .contains(&expanded_path.display().to_string()));
    assert!(!response.stderr.contains("sentinel-tilde-link-do-not-leak"));
    assert!(!response
        .stderr
        .contains("sentinel-tilde-denied-path-do-not-leak"));
    assert!(!response
        .stderr
        .contains("sentinel-tilde-connected-folder-do-not-leak"));
    assert!(!response.stderr.contains(&connected.display().to_string()));
    assert!(!response.stderr.contains("Operation not permitted"));
    assert!(!response.stderr.contains("cat:"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn ordinary_command_failure_is_not_reclassified_as_a_path_denial() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let response = provider
        .execute(request(
            &workspace,
            "call-ordinary-command-failure",
            "printf 'application validation failed\\n' >&2; exit 23",
        ))
        .await
        .unwrap();

    assert_eq!(response.exit_code, Some(23));
    assert_eq!(response.stderr, "application validation failed\n");
    assert!(!response.stderr.contains(SANDBOX_PATH_DENIED_CODE));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn permission_denial_phrase_without_a_denied_path_is_preserved() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let response = provider
        .execute(request(
            &workspace,
            "call-permission-denial-phrase",
            "printf 'Operation not permitted\\n'; printf 'Operation not permitted\\n' >&2; exit 23",
        ))
        .await
        .unwrap();

    assert_eq!(response.exit_code, Some(23));
    assert_eq!(response.stdout, "Operation not permitted\n");
    assert_eq!(response.stderr, "Operation not permitted\n");
    assert!(!response.stderr.contains(SANDBOX_PATH_DENIED_CODE));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn successful_output_with_allowed_path_and_url_diagnostics_is_preserved() {
    let (root, provider, workspace) = fixture(Duration::from_secs(3));
    let allowed_path = fs::canonicalize(root.path().join(&workspace))
        .unwrap()
        .join("allowed.txt");
    let script = r#"import os
import sys
path = os.path.join(os.getcwd(), "allowed.txt")
print('Operation not permitted: "' + path.replace("/", "\\/") + '"')
print("Operation not permitted: https://example.com/tmp/sentinel-url-path")
print("Operation not permitted: '" + path.replace("/", "\\u002f") + "'", file=sys.stderr)"#;
    let request = ExecRequest::new(
        ExecutionId::parse("call-successful-allowed-path-diagnostic").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/usr/bin/python3",
        vec!["-c".into(), script.into()],
        ".",
    )
    .unwrap();
    let response = provider.execute(request).await.unwrap();
    let escaped_allowed = allowed_path.display().to_string().replace('/', "\\/");
    let unicode_allowed = allowed_path.display().to_string().replace('/', "\\u002f");

    assert_eq!(response.exit_code, Some(0));
    assert_eq!(
        response.stdout,
        format!(
            "Operation not permitted: \"{escaped_allowed}\"\nOperation not permitted: https://example.com/tmp/sentinel-url-path\n"
        )
    );
    assert_eq!(
        response.stderr,
        format!("Operation not permitted: '{unicode_allowed}'\n")
    );
    assert!(!response.stdout.contains(SANDBOX_PATH_DENIED_CODE));
    assert!(!response.stderr.contains(SANDBOX_PATH_DENIED_CODE));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn missing_file_below_connected_folder_is_not_a_sandbox_path_denial() {
    let (_root, provider, workspace) = fixture(Duration::from_secs(3));
    let connected = tempfile::tempdir().unwrap();
    let connected_path = fs::canonicalize(connected.path()).unwrap();
    let missing = connected_path.join("incident-notes.md");
    let request = ExecRequest::new(
        ExecutionId::parse("call-connected-missing").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/bin/cat",
        vec![missing.display().to_string()],
        ".",
    )
    .unwrap()
    .with_folder_grants(vec![ExecFolderGrant::new(
        &connected_path,
        ExecFolderAccess::ReadOnly,
    )
    .unwrap()])
    .unwrap();

    let response = provider.execute(request).await.unwrap();
    assert_ne!(response.exit_code, Some(0));
    assert!(response.stderr.contains("No such file or directory"));
    assert!(!response.stderr.contains(SANDBOX_PATH_DENIED_CODE));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn resolved_workspace_paths_keep_enoent_and_symlink_escapes_are_denied() {
    use std::os::unix::fs::symlink;

    let (root, provider, workspace) = fixture(Duration::from_secs(3));
    let workspace_path = fs::canonicalize(root.path().join(&workspace)).unwrap();
    fs::create_dir(workspace_path.join("nested")).unwrap();
    let in_workspace = workspace_path.join("nested/../incident-notes.md");
    let missing = ExecRequest::new(
        ExecutionId::parse("call-absolute-workspace-missing").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/bin/cat",
        vec![in_workspace.display().to_string()],
        ".",
    )
    .unwrap();
    let response = provider.execute(missing).await.unwrap();
    assert_ne!(response.exit_code, Some(0));
    assert!(response.stderr.contains("No such file or directory"));
    assert!(!response.stderr.contains(SANDBOX_PATH_DENIED_CODE));

    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), workspace_path.join("escaped")).unwrap();
    let escaped = ExecRequest::new(
        ExecutionId::parse("call-workspace-symlink-escape").unwrap(),
        ExecutionWorkspaceId::parse(&workspace).unwrap(),
        "/bin/cat",
        vec![workspace_path
            .join("escaped/incident-notes.md")
            .display()
            .to_string()],
        ".",
    )
    .unwrap();
    let response = provider.execute(escaped).await.unwrap();
    assert_eq!(response.exit_code, Some(126));
    assert!(response.stderr.contains(SANDBOX_PATH_DENIED_CODE));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_sandbox_confines_writes_and_network_and_caches_exact_retry() {
    let (root, provider, workspace) = fixture(Duration::from_secs(3));
    let outside = root.path().join("outside");
    let script = format!(
        "printf ok > result; \
         if printf no > '{}'; then echo outside-write > write-status; \
         else echo write-blocked > write-status; fi; \
         if /usr/bin/curl -fsS --max-time 1 https://example.com >/dev/null 2>&1; \
         then echo network-open > network-status; \
         else echo network-blocked > network-status; fi; \
         cat result",
        outside.display()
    );
    let request = request(&workspace, "call-1", &script);

    let first = provider.execute(request.clone()).await.unwrap();
    let second = provider.execute(request).await.unwrap();

    assert_eq!(first, second);
    assert_eq!(first.exit_code, Some(0));
    assert!(first.stdout.is_empty());
    assert!(first.stderr.starts_with(SANDBOX_PATH_DENIED_CODE));
    assert!(!outside.exists());
    assert_eq!(
        fs::read_to_string(root.path().join(&workspace).join("result")).unwrap(),
        "ok"
    );
    assert_eq!(
        fs::read_to_string(root.path().join(&workspace).join("write-status")).unwrap(),
        "write-blocked\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join(&workspace).join("network-status")).unwrap(),
        "network-blocked\n"
    );

    for (execution, command) in [
        ("call-python-path", "python3"),
        ("call-python-system-path", "/usr/bin/python3"),
    ] {
        // The sandbox can only be as healthy as the host interpreter: on
        // macOS installs with a broken Xcode python shim, python cannot
        // run outside any sandbox either, so asserting here would fail on
        // an environment defect while proving nothing about confinement.
        let host_python_works = std::process::Command::new(command)
            .args(["-c", "print(6 * 7)"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !host_python_works {
            eprintln!("skipping {command}: host interpreter unusable in this environment");
            continue;
        }
        let python = ExecRequest::new(
            ExecutionId::parse(execution).unwrap(),
            ExecutionWorkspaceId::parse("chat-1").unwrap(),
            command,
            vec!["-c".into(), "print(6 * 7)".into()],
            ".",
        )
        .unwrap();
        let python = provider.execute(python).await.unwrap();
        // macOS ships /usr/bin/python3 as an Xcode shim that stats Xcode's
        // frameworks before running; under the sandbox (or with a broken
        // Xcode install) the shim dies before python exists. That failure
        // is an environment defect, not a confinement finding — skip it
        // loudly instead of failing the suite.
        if python.exit_code != Some(0) && python.stderr.contains("unable to locate xcodebuild") {
            eprintln!("skipping {command}: Xcode python shim cannot start on this host");
            continue;
        }
        assert_eq!(
            python.exit_code,
            Some(0),
            "{command} stderr: {}",
            python.stderr
        );
        assert_eq!(python.stdout.trim(), "42");
    }
}

/// The production regression this pins: a sandboxed interpreter writing
/// under `$HOME` (system Python drops hundreds of bytecode caches there)
/// must land outside the model-visible workspace, or the junk becomes chat
/// files and is mirrored into remote sandboxes. `HOME` and `TMPDIR` must
/// stay writable, just disjoint from the workspace tree.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_home_and_tmpdir_are_writable_outside_the_workspace() {
    let (root, provider, workspace) = fixture(Duration::from_secs(3));
    let script = "printf home > \"$HOME/home-marker\" && \
                  printf tmp > \"$TMPDIR/tmp-marker\" && \
                  printf '%s' \"$HOME\"";
    let response = provider
        .execute(request(&workspace, "call-env-home", script))
        .await
        .unwrap();
    assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);

    let workspace_dir = fs::canonicalize(root.path().join(&workspace)).unwrap();
    let home = PathBuf::from(response.stdout.trim());
    assert!(
        !home.starts_with(&workspace_dir),
        "HOME must resolve outside the model-visible workspace, got {}",
        home.display()
    );
    assert!(home.join("home-marker").is_file());
    assert!(home.join("tmp-marker").is_file());
    assert!(!workspace_dir.join("home-marker").exists());
    assert!(!workspace_dir.join("tmp-marker").exists());

    let listed = provider
        .list_workspace_files(&ExecutionWorkspaceId::parse(&workspace).unwrap(), None)
        .await
        .unwrap();
    assert!(
        listed.entries.is_empty(),
        "env-home writes must not surface as chat files: {:?}",
        listed.entries
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_network_policy_exposes_only_the_broker_port() {
    use tokio::io::AsyncWriteExt as _;

    let (_root, provider, workspace) = fixture(Duration::from_secs(5));
    let provider = provider.with_network_policy(NetworkPolicy::Open);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let direct_port = listener.local_addr().unwrap().port();
    let direct_server = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await;
        }
    });
    let script = format!(
        "if /usr/bin/curl --noproxy '*' -fsS --max-time 1 \
             http://127.0.0.1:{direct_port} >/dev/null 2>&1; \
         then echo direct-open; else echo direct-blocked; fi; \
         /usr/bin/curl -sS --max-time 2 https://127.0.0.1:9 2>&1 || true"
    );
    let response = provider
        .execute(request(&workspace, "call-broker-pinhole", &script))
        .await
        .unwrap();
    direct_server.abort();

    assert_eq!(response.exit_code, Some(0), "{}", response.stderr);
    assert!(response.stdout.contains("direct-blocked"));
    assert!(
        response.stdout.contains("403"),
        "a fast broker refusal proves the exact proxy pinhole was reachable: {}",
        response.stdout
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_sandbox_times_out_and_rejects_identity_conflicts() {
    let (_root, provider, workspace) = fixture(Duration::from_millis(100));
    let timed_out = provider
        .execute(request(&workspace, "call-timeout", "sleep 5"))
        .await
        .unwrap();
    assert!(timed_out.timed_out);

    provider
        .execute(request(&workspace, "call-conflict", "printf one"))
        .await
        .unwrap();
    let conflict = provider
        .execute(request(&workspace, "call-conflict", "printf two"))
        .await
        .unwrap_err();
    assert!(matches!(conflict, ExecError::IdentityConflict));
}

#[tokio::test]
async fn local_workspace_lifecycle_round_trips_and_stays_inside_scratch() {
    let root = tempfile::tempdir().unwrap();
    let provider = LocalExecutionProvider::new(root.path(), Duration::from_secs(1)).unwrap();
    let workspace = ExecutionWorkspaceId::parse("chat-ws").unwrap();

    assert!(!provider.connect_workspace(&workspace).await.unwrap());
    provider.create_workspace(&workspace).await.unwrap();
    assert!(provider.connect_workspace(&workspace).await.unwrap());

    let path = WorkspaceFilePath::parse("reports/summary.bin").unwrap();
    let content = b"\x00binary\xff".to_vec();
    provider
        .put_workspace_file(&workspace, &path, &content)
        .await
        .unwrap();
    assert_eq!(
        provider
            .get_workspace_file(&workspace, &path)
            .await
            .unwrap(),
        content
    );

    let top = provider
        .list_workspace_files(&workspace, None)
        .await
        .unwrap();
    assert_eq!(top.entries.len(), 1);
    assert_eq!(top.entries[0].path, "reports");
    assert!(top.entries[0].directory);
    let nested = provider
        .list_workspace_files(
            &workspace,
            Some(&WorkspaceFilePath::parse("reports").unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(nested.entries.len(), 1);
    assert_eq!(nested.entries[0].path, "reports/summary.bin");
    assert_eq!(nested.entries[0].size_bytes, Some(content.len() as u64));

    assert!(matches!(
        provider
            .get_workspace_file(&workspace, &WorkspaceFilePath::parse("missing").unwrap())
            .await,
        Err(ExecError::WorkspaceFileNotFound)
    ));
    assert!(matches!(
        provider
            .put_workspace_file(
                &workspace,
                &path,
                &vec![0_u8; crate::MAX_WORKSPACE_FILE_BYTES + 1],
            )
            .await,
        Err(ExecError::WorkspaceFileTooLarge)
    ));

    // A symlink planted in the workspace must never let a read escape it,
    // whether it is the file itself or an intermediate directory.
    #[cfg(unix)]
    {
        let outside = root.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        let workspace_dir = root.path().join("chat-ws");
        std::os::unix::fs::symlink(&outside, workspace_dir.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(root.path(), workspace_dir.join("escape")).unwrap();
        assert!(provider
            .get_workspace_file(&workspace, &WorkspaceFilePath::parse("link.txt").unwrap())
            .await
            .is_err());
        assert!(provider
            .get_workspace_file(
                &workspace,
                &WorkspaceFilePath::parse("escape/outside.txt").unwrap(),
            )
            .await
            .is_err());
        let listed = provider
            .list_workspace_files(&workspace, None)
            .await
            .unwrap();
        assert!(listed.entries.iter().all(|entry| entry.path == "reports"));
    }

    provider.destroy_workspace(&workspace).await.unwrap();
    assert!(!provider.connect_workspace(&workspace).await.unwrap());
    // Destroying a workspace that no longer exists stays a success.
    provider.destroy_workspace(&workspace).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn planted_symlinks_never_redirect_a_workspace_read_or_write() {
    let root = tempfile::tempdir().unwrap();
    let provider = LocalExecutionProvider::new(root.path(), Duration::from_secs(1)).unwrap();
    let workspace = ExecutionWorkspaceId::parse("chat-attack").unwrap();
    provider.create_workspace(&workspace).await.unwrap();
    let workspace_dir = root.path().join("chat-attack");

    // A host secret the confined writer wants the unsandboxed host to touch.
    let secret = root.path().join("secret.txt");
    fs::write(&secret, "original-secret").unwrap();

    // Write: a symlink pre-planted at the destination filename must not
    // redirect the write onto the secret. The atomic rename replaces the
    // symlink itself, so the payload lands inside the workspace and the
    // secret is untouched.
    let write_path = WorkspaceFilePath::parse("report.txt").unwrap();
    std::os::unix::fs::symlink(&secret, workspace_dir.join("report.txt")).unwrap();
    provider
        .put_workspace_file(&workspace, &write_path, b"payload")
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&secret).unwrap(), "original-secret");
    let written = workspace_dir.join("report.txt");
    assert!(!fs::symlink_metadata(&written)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(&written).unwrap(), "payload");

    // Read: a symlink at the requested path must error rather than follow
    // out to the secret, even though the target is a regular file's worth
    // of bytes on the other end.
    std::os::unix::fs::symlink(&secret, workspace_dir.join("leak.txt")).unwrap();
    let leak = provider
        .get_workspace_file(&workspace, &WorkspaceFilePath::parse("leak.txt").unwrap())
        .await;
    assert!(
        matches!(leak, Err(ExecError::InvalidRequest(_))),
        "no-follow read must refuse a symlink, got {leak:?}"
    );

    // A second write still succeeds alongside an unrelated planted dotfile.
    // This does not exercise the temp-name defense — a fixed `.stale`
    // suffix can never collide with the real `.workspace-put.{uuid}` name
    // by construction — it only guards against an incidental regression
    // where a stray dotfile wedged puts.
    fs::write(workspace_dir.join(".workspace-put.stale"), "junk").unwrap();
    provider
        .put_workspace_file(
            &workspace,
            &WorkspaceFilePath::parse("second.txt").unwrap(),
            b"second",
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(workspace_dir.join("second.txt")).unwrap(),
        "second"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_refused_write_creates_nothing_outside_the_workspace() {
    let outside = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let provider = LocalExecutionProvider::new(root.path(), Duration::from_secs(1)).unwrap();
    let workspace = ExecutionWorkspaceId::parse("chat-parents").unwrap();
    provider.create_workspace(&workspace).await.unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("chat-parents/planted")).unwrap();

    let write = provider
        .put_workspace_file(
            &workspace,
            &WorkspaceFilePath::parse("planted/deep/report.txt").unwrap(),
            b"payload",
        )
        .await;

    assert!(write.is_err(), "write through a planted parent must refuse");
    assert!(
        !outside.path().join("deep").exists(),
        "a refused write must not have created directories outside the workspace",
    );
}

#[test]
fn failed_begin_persistence_releases_the_execution_id_for_retry() {
    let receipts = tempfile::tempdir().unwrap();
    let path = receipts.path().join("call-retry.json");
    let error = begin_execution_with_persistence(&path, "fingerprint", |file| {
        file.write_all(b"{")?;
        Err(std::io::Error::other("injected persistence failure"))
    });
    let error = match error {
        Err(error) => error,
        Ok(_) => panic!("injected persistence failure unexpectedly succeeded"),
    };

    assert!(matches!(error, ExecError::Sandbox(_)));
    assert!(
        !path.exists(),
        "an unstarted partial claim must not block retries"
    );
    assert!(matches!(
        begin_execution(&path, "fingerprint").unwrap(),
        BeginExecution::Started
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn profile_denies_network_and_escapes_workspace_paths() {
    let profile = macos_profile(
        Path::new("/Users/test/we\"ird\\workspace"),
        Path::new("/Users/test/env-home"),
        None,
        Some(Path::new(
            "/Applications/Tidebreak.app/Contents/Resources/exec-scripts",
        )),
        Some(Path::new(
            "/Users/test/.code-execution-package-cache/cp311-darwin-arm64/wheels",
        )),
        &[],
        None,
        &[],
        None,
    )
    .unwrap();
    assert!(profile.contains("(deny default)"));
    assert!(!profile.contains("allow network"));
    assert!(!profile.contains("mach-lookup"));
    assert!(!profile.contains("(allow process*)"));
    assert!(profile.contains("(allow process-exec)"));
    assert!(profile.contains("(allow process-fork)"));
    assert!(profile.contains("we\\\"ird\\\\workspace"));
    assert!(profile.contains("Resources/exec-scripts"));
    // The shared package cache is readable and never writable: verified
    // artifacts flow from the host in, never from a sandbox out.
    assert!(profile.contains(".code-execution-package-cache/cp311-darwin-arm64/wheels"));
    let write_rule = profile
        .split("(allow file-write*")
        .nth(1)
        .expect("profile has a write rule");
    assert!(!write_rule.contains("Resources/exec-scripts"));
    assert!(!write_rule.contains(".code-execution-package-cache"));
    assert!(macos_profile(
        Path::new("/Users/test/control\nworkspace"),
        Path::new("/Users/test/env-home"),
        None,
        None,
        None,
        &[],
        None,
        &[],
        None,
    )
    .is_err());

    let proxied = macos_profile(
        Path::new("/Users/test/workspace"),
        Path::new("/Users/test/env-home"),
        None,
        None,
        None,
        &[],
        None,
        &[],
        Some(43127),
    )
    .unwrap();
    assert!(proxied.contains("(allow network-outbound (remote tcp \"localhost:43127\"))"));
    assert!(!proxied.contains("localhost:*"));
}

/// A managed Node runtime is a host-supplied slot like the package cache:
/// present only when the host hands one over, readable and never writable,
/// and first on `PATH` so a skill's npm work runs the pinned interpreter.
#[cfg(target_os = "macos")]
#[test]
fn managed_node_is_read_only_and_leads_path_only_when_supplied() {
    let node = Path::new("/Users/test/Library/Application Support/Tidebreak/node/v22.11.0");
    let profile = |managed_node| {
        macos_profile(
            Path::new("/Users/test/workspace"),
            Path::new("/Users/test/env-home"),
            None,
            None,
            None,
            &[],
            managed_node,
            &[],
            None,
        )
        .unwrap()
    };

    let with_node = profile(Some(node));
    assert!(with_node.contains(&sandbox_subpath(node).unwrap()));
    assert!(with_node.contains("(literal \"/Users\")"));
    assert!(
        with_node.contains("(literal \"/Users/test/Library/Application Support/Tidebreak/node\")")
    );
    let write_rule = with_node
        .split("(allow file-write*")
        .nth(1)
        .expect("profile has a write rule");
    assert!(!write_rule.contains("Tidebreak/node"));
    assert!(!profile(None).contains("Tidebreak/node"));

    let developer_dir = Path::new("/Library/Developer/CommandLineTools");
    assert_eq!(
        sandbox_path(Some(developer_dir), None, Some(node)),
        "/Users/test/Library/Application Support/Tidebreak/node/v22.11.0/bin:\
         /Library/Developer/CommandLineTools/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    );
    assert_eq!(
        sandbox_path(None, None, None),
        "/usr/bin:/bin:/usr/sbin:/sbin",
        "without a managed runtime the sandbox keeps the system PATH it always had"
    );
}

/// The end of the same story, run for real: a runtime the host hands over
/// is executable inside Seatbelt purely because its subtree is readable,
/// and one the host withholds is not on `PATH` at all.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn managed_node_runs_in_the_sandbox_and_is_absent_without_it() {
    let scratch = tempfile::tempdir().unwrap();
    let workspace = "chat-node";
    fs::create_dir(scratch.path().join(workspace)).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let bin = runtime.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("node"), "#!/bin/sh\nprintf v22.11.0\n").unwrap();
    fs::set_permissions(bin.join("node"), fs::Permissions::from_mode(0o755)).unwrap();
    let runtime = fs::canonicalize(runtime.path()).unwrap();

    let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(5)).unwrap();
    let script = "node --version || printf no-node";
    let without = provider
        .execute(request(workspace, "call-no-node", script))
        .await
        .unwrap();
    assert_eq!(without.stdout, "no-node");

    let denied_ancestor = runtime
        .ancestors()
        .find(|path| *path == Path::new("/private"))
        .expect("macOS temporary directories resolve below /private")
        .to_path_buf();
    let provider = provider.with_managed_node(Some(runtime));
    let script = format!(
        "node --version; stat -f %N '{}' >/dev/null && printf metadata-ok",
        denied_ancestor.display()
    );
    let with = provider
        .execute(request(workspace, "call-node", &script))
        .await
        .unwrap();
    assert_eq!(
        with.stdout, "v22.11.0metadata-ok",
        "stderr: {}",
        with.stderr
    );
}

/// A selected Python prefix follows the same read-only runtime contract as
/// managed Node and takes precedence over the unsupported system Python.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn supported_python_runtime_runs_in_the_sandbox() {
    let scratch = tempfile::tempdir().unwrap();
    let workspace = "chat-python";
    fs::create_dir(scratch.path().join(workspace)).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let bin = runtime.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("python3"), "#!/bin/sh\nprintf 'Python 3.12.9'\n").unwrap();
    fs::set_permissions(bin.join("python3"), fs::Permissions::from_mode(0o755)).unwrap();
    let runtime = fs::canonicalize(runtime.path()).unwrap();

    let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(5))
        .unwrap()
        .with_python_runtime(Some(runtime.clone()), Vec::new());
    let response = provider
        .execute(request(
            workspace,
            "call-python-runtime",
            "python3 --version",
        ))
        .await
        .unwrap();
    assert_eq!(response.stdout, "Python 3.12.9");
    assert!(response.stderr.is_empty());

    let profile = macos_profile(
        Path::new("/Users/test/workspace"),
        Path::new("/Users/test/env-home"),
        None,
        None,
        None,
        std::slice::from_ref(&runtime),
        None,
        &[],
        None,
    )
    .unwrap();
    let write_rule = profile
        .split("(allow file-write*")
        .nth(1)
        .expect("profile has a write rule");
    assert!(!write_rule.contains(runtime.to_str().unwrap()));
}

#[cfg(target_os = "macos")]
#[test]
fn python_runtime_cannot_reopen_the_workspace_or_home() {
    for runtime in [Path::new("/"), Path::new("/Users/test")] {
        let runtime_dirs = [runtime.to_path_buf()];
        let error = macos_profile(
            Path::new("/Users/test/workspace"),
            Path::new("/Users/test/env-home"),
            None,
            None,
            None,
            &runtime_dirs,
            None,
            &[],
            None,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("selected Python runtime is too broad"));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn profile_compiles_read_and_write_folder_grants_without_widening_reads() {
    let folders = tempfile::tempdir().unwrap();
    let read_only = folders.path().join("read-only");
    let read_write = folders.path().join("read-write");
    fs::create_dir_all(&read_only).unwrap();
    fs::create_dir_all(&read_write).unwrap();
    let grants = canonicalize_folder_grants(&[
        ExecFolderGrant::new(&read_only, ExecFolderAccess::ReadOnly).unwrap(),
        ExecFolderGrant::new(&read_write, ExecFolderAccess::ReadWrite).unwrap(),
    ])
    .unwrap();
    let profile = macos_profile(
        Path::new("/Users/test/workspace"),
        Path::new("/Users/test/env-home"),
        None,
        None,
        None,
        &[],
        None,
        &grants,
        None,
    )
    .unwrap();
    let canonical_read = fs::canonicalize(read_only).unwrap();
    let canonical_write = fs::canonicalize(read_write).unwrap();

    assert!(profile.contains(&sandbox_subpath(&canonical_read).unwrap()));
    assert!(profile.contains(&sandbox_subpath(&canonical_write).unwrap()));
    let write_rule = profile
        .split("(allow file-write*")
        .nth(1)
        .expect("profile has a write rule");
    assert!(!write_rule.contains(canonical_read.to_str().unwrap()));
    assert!(write_rule.contains(canonical_write.to_str().unwrap()));
    assert!(profile.contains("(deny file-read*"));
}

#[cfg(target_os = "macos")]
#[test]
fn folder_grants_reject_symlinks_and_missing_roots() {
    use std::os::unix::fs::symlink;

    let folders = tempfile::tempdir().unwrap();
    let target = folders.path().join("target");
    let linked = folders.path().join("linked");
    fs::create_dir_all(&target).unwrap();
    symlink(&target, &linked).unwrap();

    let symlink_error =
        canonicalize_folder_grants(&[
            ExecFolderGrant::new(&linked, ExecFolderAccess::ReadOnly).unwrap()
        ])
        .unwrap_err();
    assert!(symlink_error.to_string().contains("symbolic link"));

    let missing_error = canonicalize_folder_grants(&[ExecFolderGrant::new(
        folders.path().join("missing"),
        ExecFolderAccess::ReadOnly,
    )
    .unwrap()])
    .unwrap_err();
    assert!(missing_error.to_string().contains("unavailable"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_sandbox_reads_only_the_granted_sibling() {
    let scratch = tempfile::tempdir().unwrap();
    let workspace = "chat-grant";
    fs::create_dir(scratch.path().join(workspace)).unwrap();
    let host = tempfile::tempdir().unwrap();
    let granted = host.path().join("granted");
    let ungranted = host.path().join("ungranted");
    fs::create_dir(&granted).unwrap();
    fs::create_dir(&ungranted).unwrap();
    fs::write(granted.join("visible.txt"), "visible").unwrap();
    fs::write(ungranted.join("secret.txt"), "secret").unwrap();
    let granted_path = fs::canonicalize(&granted).unwrap();
    let ungranted_path = fs::canonicalize(&ungranted).unwrap();
    let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(3)).unwrap();
    let script = format!(
        "cat '{}'; if cat '{}' >/dev/null 2>&1; then printf ungranted-open; else printf ungranted-blocked; fi",
        granted_path.join("visible.txt").display(),
        ungranted_path.join("secret.txt").display()
    );
    let request = request(workspace, "call-folder-grant", &script)
        .with_folder_grants(vec![ExecFolderGrant::new(
            &granted_path,
            ExecFolderAccess::ReadOnly,
        )
        .unwrap()])
        .unwrap();

    let response = provider.execute(request).await.unwrap();
    assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);
    assert_eq!(response.stdout, "visibleungranted-blocked");
}

/// The cross-conversation containment the shared cache rests on: a
/// sandbox can consume verified artifacts but can neither modify them nor
/// plant new ones, so no conversation can poison what another installs.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn shared_package_cache_is_readable_and_never_writable_in_the_sandbox() {
    let scratch = tempfile::tempdir().unwrap();
    let workspace = "chat-cache";
    fs::create_dir(scratch.path().join(workspace)).unwrap();
    let cache = crate::package_cache::SharedPackageCache::open(
        &scratch.path().join(crate::package_cache::PACKAGE_CACHE_DIR),
        "cp311-darwin-arm64",
    )
    .unwrap();
    let staging = scratch.path().join("staging");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("fpdf2-2.8.3-py3-none-any.whl"), b"wheel-bytes").unwrap();
    cache.verify_and_promote(&staging).unwrap();
    let wheels = fs::canonicalize(cache.wheels_dir()).unwrap();

    let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(3))
        .unwrap()
        .with_shared_package_cache(Some(wheels.clone()));
    let script = format!(
        "cat \"$TIDEBREAK_PACKAGE_CACHE/fpdf2-2.8.3-py3-none-any.whl\"; \
         if printf poison > '{planted}' 2>/dev/null; then printf ' cache-writable'; else printf ' cache-readonly'; fi; \
         if printf poison > '{tampered}' 2>/dev/null; then printf ' wheel-writable'; else printf ' wheel-readonly'; fi",
        planted = wheels.join("planted-1.0-py3-none-any.whl").display(),
        tampered = wheels.join("fpdf2-2.8.3-py3-none-any.whl").display(),
    );
    let response = provider
        .execute(request(workspace, "call-cache", &script))
        .await
        .unwrap();

    assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);
    assert_eq!(response.stdout, "wheel-bytes cache-readonly wheel-readonly");
    assert!(!wheels.join("planted-1.0-py3-none-any.whl").exists());
    assert_eq!(
        fs::read(wheels.join("fpdf2-2.8.3-py3-none-any.whl")).unwrap(),
        b"wheel-bytes"
    );
}

/// The composed end-to-end proof the shared cache exists for: under the
/// default no-network policy, a sandboxed `pip install --user --no-index
/// --find-links "$TIDEBREAK_PACKAGE_CACHE"` resolves a pinned package
/// purely from artifacts the host promoted through the real verification
/// path, and the package imports in a later invocation of the same chat.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn offline_pip_install_resolves_from_the_promoted_shared_cache() {
    // A wheel is a zip archive with recorded member hashes; building one
    // directly keeps the fixture fully offline while staying a real,
    // valid wheel that pip verifies and installs like any registry
    // artifact.
    const BUILD_WHEEL: &str = r#"
import base64, csv, hashlib, io, sys, zipfile
dest = sys.argv[1]
files = {
"tidebreakproof/__init__.py": b"MARKER = 'offline-cache-proof'\n",
"tidebreakproof-1.0.0.dist-info/METADATA": b"Metadata-Version: 2.1\nName: tidebreakproof\nVersion: 1.0.0\n",
"tidebreakproof-1.0.0.dist-info/WHEEL": b"Wheel-Version: 1.0\nGenerator: tidebreak-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
}
record = "tidebreakproof-1.0.0.dist-info/RECORD"
rows = []
with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as archive:
for name, data in files.items():
    archive.writestr(name, data)
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    rows.append((name, f"sha256={digest}", str(len(data))))
rows.append((record, "", ""))
out = io.StringIO()
csv.writer(out, lineterminator="\n").writerows(rows)
archive.writestr(record, out.getvalue())
"#;

    let Some(runtime) =
        crate::package_cache::SharedPackageCache::python_runtime(Path::new("python3")).await
    else {
        eprintln!("skipping: no supported host Python runtime is available");
        return;
    };
    // The proof can only be as healthy as the selected interpreter:
    // without a working pip there is nothing here to prove about the
    // cache.
    let pip_works = std::process::Command::new(runtime.executable())
        .args(["-m", "pip", "--version"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !pip_works {
        eprintln!("skipping: host python/pip unusable in this environment");
        return;
    }

    let scratch = tempfile::tempdir().unwrap();
    let workspace = "chat-offline-install";
    fs::create_dir(scratch.path().join(workspace)).unwrap();

    // Build the wheel into a staging directory and promote it through the
    // same verification pass `populate_with_pip` runs after its download,
    // so the manifest the sandboxed install relies on is real, not
    // hand-forged.
    let staging = scratch.path().join("staging");
    fs::create_dir(&staging).unwrap();
    let built = std::process::Command::new(runtime.executable())
        .args(["-c", BUILD_WHEEL])
        .arg(staging.join("tidebreakproof-1.0.0-py3-none-any.whl"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "wheel fixture build failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let cache = crate::package_cache::SharedPackageCache::open(
        &scratch.path().join(crate::package_cache::PACKAGE_CACHE_DIR),
        runtime.key(),
    )
    .unwrap();
    let report = cache.verify_and_promote(&staging).unwrap();
    assert_eq!(report.promoted, 1, "the staged wheel must promote");
    assert!(cache.is_ready());
    let wheels = fs::canonicalize(cache.wheels_dir()).unwrap();

    // `NetworkPolicy::Off` is the provider default: no broker, and no
    // network allowance in the profile. The curl probe pins that the
    // install ran with the network actually off, not merely unused.
    let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(120))
        .unwrap()
        .with_shared_package_cache(Some(wheels))
        .with_python_runtime(
            Some(runtime.prefix().to_owned()),
            runtime.read_only_paths().to_vec(),
        );
    // A python whose stdlib carries the EXTERNALLY-MANAGED marker needs
    // `--break-system-packages` (its bundled pip understands the flag);
    // passing it unconditionally would fail the older pips that don't.
    let externally_managed = std::process::Command::new(runtime.executable())
        .args([
            "-c",
            "import os, sysconfig; \
             print(os.path.exists(os.path.join(sysconfig.get_path('stdlib'), 'EXTERNALLY-MANAGED')))",
        ])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "True")
        .unwrap_or(false);
    let break_flag = if externally_managed {
        " --break-system-packages"
    } else {
        ""
    };
    let install = format!(
        "if /usr/bin/curl -fsS --max-time 1 https://example.com >/dev/null 2>&1; \
         then echo network-open; else echo network-blocked; fi; \
         python3 -m pip install --user --quiet --no-index \
         --disable-pip-version-check --no-input{break_flag} \
         --find-links \"$TIDEBREAK_PACKAGE_CACHE\" tidebreakproof==1.0.0"
    );
    let response = provider
        .execute(request(workspace, "call-offline-install", &install))
        .await
        .unwrap();
    // The Xcode python shim failing to start is an environment defect,
    // not a cache finding — skip it loudly, as the confinement test does.
    if response.stderr.contains("unable to locate xcodebuild") {
        eprintln!("skipping: Xcode python shim cannot start on this host");
        return;
    }
    assert_eq!(
        response.exit_code,
        Some(0),
        "stdout: {} stderr: {}",
        response.stdout,
        response.stderr
    );
    assert!(response.stdout.contains("network-blocked"));

    // A later invocation of the same chat imports the installed package
    // from its persistent per-chat HOME.
    let imported = provider
        .execute(request(
            workspace,
            "call-offline-import",
            "python3 -c \"import tidebreakproof; print(tidebreakproof.MARKER)\"",
        ))
        .await
        .unwrap();
    assert_eq!(imported.exit_code, Some(0), "stderr: {}", imported.stderr);
    assert_eq!(imported.stdout.trim(), "offline-cache-proof");
}

/// The invariant staging rests on: a staged grant is writable only at the
/// overlay. A command that names the user's folder directly — which is the
/// path the model has always been given — is refused rather than silently
/// staged, so nothing reaches the real files mid-turn.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_staged_grant_is_writable_only_at_its_overlay() {
    let scratch = tempfile::tempdir().unwrap();
    let workspace = "chat-staged";
    fs::create_dir(scratch.path().join(workspace)).unwrap();
    let host = tempfile::tempdir().unwrap();
    let granted = host.path().join("granted");
    let overlay = host.path().join("overlay");
    fs::create_dir(&granted).unwrap();
    fs::create_dir(&overlay).unwrap();
    fs::write(granted.join("report.md"), "original").unwrap();
    let granted_path = fs::canonicalize(&granted).unwrap();
    let overlay_path = fs::canonicalize(&overlay).unwrap();

    let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(3)).unwrap();
    let script = format!(
        "cat '{}'; \
         if printf staged > '{}' 2>/dev/null; then printf ' overlay-written'; else printf ' overlay-blocked'; fi; \
         if printf direct > '{}' 2>/dev/null; then printf ' root-written'; else printf ' root-blocked'; fi",
        granted_path.join("report.md").display(),
        overlay_path.join("report.md").display(),
        granted_path.join("report.md").display(),
    );
    let request = request(workspace, "call-staged-grant", &script)
        .with_folder_grants(vec![ExecFolderGrant::new(
            &granted_path,
            ExecFolderAccess::ReadWrite,
        )
        .unwrap()
        .staged_at(&overlay_path)
        .unwrap()])
        .unwrap();

    let response = provider.execute(request).await.unwrap();
    assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);
    assert_eq!(
        response.stdout, "original overlay-written root-blocked",
        "stderr: {}",
        response.stderr
    );
    assert_eq!(
        fs::read_to_string(granted_path.join("report.md")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(overlay_path.join("report.md")).unwrap(),
        "staged"
    );
}
