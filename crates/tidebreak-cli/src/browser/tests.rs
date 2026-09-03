use super::*;

const VALID_TOKEN: &str = "tbreak_bt_00000000-0000-0000-0000-000000000000";

/// Serialize and unwind-safely restore the short process-environment
/// mutation needed to prove reqwest ignores ambient proxy settings.
static PROXY_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ScopedEnv {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => std::env::set_var(self.key, previous),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Extract the error from `BrowserCapfile::load` without requiring
/// `Debug` on the `Ok` type. `BrowserCapfile` intentionally does not
/// implement `Debug` because it carries the bearer token, so
/// `Result::unwrap_err` (which requires `T: Debug`) cannot be used.
fn capfile_load_err(path: &std::path::Path) -> AgentError {
    match BrowserCapfile::load(path) {
        Ok(_) => panic!("expected BrowserCapfile::load to fail, but it succeeded"),
        Err(error) => error,
    }
}

// -- Capfile parsing ---------------------------------------------------

#[test]
fn capfile_accepts_valid_v1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    let cap = BrowserCapfile::load(&path).unwrap();
    assert_eq!(cap.endpoint, "http://127.0.0.1:9876/code/browser");
    assert_eq!(cap.token, VALID_TOKEN);
    assert!(!cap.semantic_actions);
}

#[test]
fn capfile_accepts_semantic_actions_capability() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/code/browser",
            "token": VALID_TOKEN,
            "semantic_actions": true
        })
        .to_string(),
    )
    .unwrap();
    assert!(BrowserCapfile::load(&path).unwrap().semantic_actions);
}

#[test]
fn capfile_accepts_localhost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://localhost:3000/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    assert!(BrowserCapfile::load(&path).is_ok());
}

#[test]
fn capfile_accepts_ipv6_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://[::1]:8080/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    assert!(BrowserCapfile::load(&path).is_ok());
}

#[test]
fn capfile_rejects_non_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let err = capfile_load_err(dir.path());
    assert!(err.to_string().contains("regular file"));
}

// -- Capfile rejection: endpoint validation ----------------------------

#[test]
fn capfile_rejects_https() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "https://127.0.0.1:9876/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    assert!(BrowserCapfile::load(&path).is_err());
}

#[test]
fn capfile_rejects_non_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://192.168.1.1:8080/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(msg.contains("loopback"), "error: {msg}");
}

#[test]
fn capfile_rejects_missing_port() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(msg.contains("port"), "error: {msg}");
}

#[test]
fn capfile_rejects_wrong_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/some/other/path",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(msg.contains("/code/browser"), "error: {msg}");
}

#[test]
fn capfile_rejects_username_in_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://user@127.0.0.1:9876/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    assert!(BrowserCapfile::load(&path).is_err());
}

#[test]
fn capfile_rejects_query_in_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/code/browser?x=1",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    assert!(BrowserCapfile::load(&path).is_err());
}

// -- Capfile rejection: token validation -------------------------------

#[test]
fn capfile_rejects_wrong_token_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/code/browser",
            "token": "notbreak__00000000-0000-0000-0000-000000000000"
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(msg.contains("prefix"), "error: {msg}");
}

#[test]
fn capfile_rejects_token_without_uuid_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/code/browser",
            "token": "tbreak_bt_00000000-0000-0000-0000-00000000000Z"
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(msg.contains("UUID"), "error: {msg}");
}

#[test]
fn capfile_rejects_short_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/code/browser",
            "token": "tbreak_bt_short"
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(msg.contains("length"), "error: {msg}");
}

// -- Capfile rejection: version, JSON, size, unknown fields ------------

#[test]
fn capfile_rejects_unsupported_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 2,
            "endpoint": "http://127.0.0.1:9876/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(msg.contains("version 2"), "error: {msg}");
}

#[test]
fn capfile_rejects_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://127.0.0.1:9876/code/browser",
            "token": VALID_TOKEN,
            "extra": "should be rejected"
        })
        .to_string(),
    )
    .unwrap();
    assert!(BrowserCapfile::load(&path).is_err());
}

#[test]
fn capfile_rejects_malformed_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(&path, "not json").unwrap();
    assert!(BrowserCapfile::load(&path).is_err());
}

#[test]
fn capfile_rejects_oversized_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    let big = "x".repeat((CAPFILE_MAX_BYTES + 1) as usize);
    std::fs::write(&path, big).unwrap();
    assert!(BrowserCapfile::load(&path).is_err());
}

#[test]
fn capfile_rejects_non_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    std::fs::write(&path, vec![0xff, 0xfe, 0xfd]).unwrap();
    assert!(BrowserCapfile::load(&path).is_err());
}

// -- Capfile: error redaction -----------------------------------------

#[test]
fn capfile_error_never_contains_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    let token = VALID_TOKEN;
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://192.168.1.1:8080/code/browser",
            "token": token
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(!msg.contains(token), "token leaked into error: {msg}");
}

#[test]
fn capfile_error_never_contains_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    let path_str = path.to_string_lossy().to_string();
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": "http://192.168.1.1:8080/code/browser",
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(
        !msg.contains(&path_str),
        "capfile path leaked into error: {msg}"
    );
}

#[test]
fn capfile_error_never_contains_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.json");
    let endpoint = "http://192.168.1.1:8080/code/browser";
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "endpoint": endpoint,
            "token": VALID_TOKEN
        })
        .to_string(),
    )
    .unwrap();
    let err = capfile_load_err(&path);
    let msg = err.to_string();
    assert!(!msg.contains(endpoint), "endpoint leaked into error: {msg}");
}

// -- Capfile: read_file_capped ----------------------------------------

#[test]
fn read_file_capped_returns_exact_contents_within_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    let content = "hello world";
    std::fs::write(&path, content).unwrap();
    let result = read_file_capped(&path, 1024).unwrap();
    assert_eq!(result, content);
}

#[test]
fn read_file_capped_truncates_at_cap_plus_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    let big: String = (0..200).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    std::fs::write(&path, &big).unwrap();
    let result = read_file_capped(&path, 100).unwrap();
    // Should have at most 101 bytes.
    assert!(result.len() <= 101);
}

// -- Command parsing ---------------------------------------------------

#[test]
fn parse_browser_list() {
    let cmd = parse_browser(vec!["list".to_string()]).unwrap();
    assert!(matches!(cmd, BrowserCommand::List));
}

#[test]
fn parse_browser_list_rejects_extra_args() {
    let err = parse_browser(vec!["list".to_string(), "extra".to_string()]).unwrap_err();
    assert!(err.contains("no arguments"), "error: {err}");
}

#[test]
fn parse_browser_navigate() {
    let cmd = parse_browser(vec![
        "navigate".to_string(),
        "--browser-id".to_string(),
        "browser-123".to_string(),
        "--url".to_string(),
        "https://example.com".to_string(),
    ])
    .unwrap();
    match cmd {
        BrowserCommand::Navigate { browser_id, url } => {
            assert_eq!(browser_id, "browser-123");
            assert_eq!(url, "https://example.com");
        }
        _ => panic!("expected Navigate"),
    }
}

#[test]
fn parse_browser_navigate_rejects_duplicate_browser_id() {
    let err = parse_browser(vec![
        "navigate".to_string(),
        "--browser-id".to_string(),
        "browser-1".to_string(),
        "--browser-id".to_string(),
        "browser-2".to_string(),
        "--url".to_string(),
        "https://example.com".to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("duplicate"), "error: {err}");
}

#[test]
fn parse_browser_navigate_rejects_duplicate_url() {
    let err = parse_browser(vec![
        "navigate".to_string(),
        "--browser-id".to_string(),
        "browser-1".to_string(),
        "--url".to_string(),
        "https://example.com".to_string(),
        "--url".to_string(),
        "https://other.com".to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("duplicate"), "error: {err}");
}

#[test]
fn parse_browser_navigate_requires_browser_id() {
    let err = parse_browser(vec![
        "navigate".to_string(),
        "--url".to_string(),
        "https://example.com".to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("--browser-id"), "error: {err}");
}

#[test]
fn parse_browser_navigate_requires_url() {
    let err = parse_browser(vec![
        "navigate".to_string(),
        "--browser-id".to_string(),
        "browser-123".to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("--url"), "error: {err}");
}

#[test]
fn parse_browser_snapshot_default() {
    let cmd = parse_browser(vec![
        "snapshot".to_string(),
        "--browser-id".to_string(),
        "browser-123".to_string(),
    ])
    .unwrap();
    match cmd {
        BrowserCommand::Snapshot {
            browser_id,
            max_nodes,
        } => {
            assert_eq!(browser_id, "browser-123");
            assert_eq!(max_nodes, None);
        }
        _ => panic!("expected Snapshot"),
    }
}

#[test]
fn parse_browser_snapshot_with_max_nodes() {
    let cmd = parse_browser(vec![
        "snapshot".to_string(),
        "--browser-id".to_string(),
        "browser-123".to_string(),
        "--max-nodes".to_string(),
        "100".to_string(),
    ])
    .unwrap();
    match cmd {
        BrowserCommand::Snapshot { max_nodes, .. } => {
            assert_eq!(max_nodes, Some(100));
        }
        _ => panic!("expected Snapshot"),
    }
}

#[test]
fn parse_browser_snapshot_rejects_duplicate_max_nodes() {
    let err = parse_browser(vec![
        "snapshot".to_string(),
        "--browser-id".to_string(),
        "browser-123".to_string(),
        "--max-nodes".to_string(),
        "100".to_string(),
        "--max-nodes".to_string(),
        "200".to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("duplicate"), "error: {err}");
}

#[test]
fn parse_browser_snapshot_requires_browser_id() {
    let err = parse_browser(vec![
        "snapshot".to_string(),
        "--max-nodes".to_string(),
        "100".to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("--browser-id"), "error: {err}");
}

fn browser_args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn parse_browser_wait_supports_each_bounded_condition() {
    let cases = [
        (vec!["--url-changed"], BrowserWaitCondition::UrlChanged),
        (
            vec!["--load-state", "ready"],
            BrowserWaitCondition::LoadState {
                state: tidebreak_core::BrowserLoadState::Ready,
            },
        ),
        (
            vec!["--text-present", "Done"],
            BrowserWaitCondition::TextPresent {
                text: "Done".to_string(),
            },
        ),
        (
            vec!["--text-absent", "Loading"],
            BrowserWaitCondition::TextAbsent {
                text: "Loading".to_string(),
            },
        ),
    ];

    for (condition_args, expected) in cases {
        let mut args = browser_args(&[
            "wait",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "7",
        ]);
        args.extend(condition_args.into_iter().map(str::to_string));
        args.extend(browser_args(&["--timeout-ms", "30000"]));

        match parse_browser(args).unwrap() {
            BrowserCommand::Wait {
                browser_id,
                snapshot_id,
                document_epoch,
                condition,
                timeout_ms,
            } => {
                assert_eq!(browser_id, "browser-1");
                assert_eq!(snapshot_id, "snapshot-1");
                assert_eq!(document_epoch, 7);
                assert_eq!(condition, expected);
                assert_eq!(timeout_ms, Some(30_000));
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }
}

#[test]
fn parse_browser_wait_requires_identity_and_exactly_one_condition() {
    for args in [
        browser_args(&[
            "wait",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--url-changed",
        ]),
        browser_args(&[
            "wait",
            "--browser-id",
            "browser-1",
            "--document-epoch",
            "1",
            "--url-changed",
        ]),
        browser_args(&[
            "wait",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--url-changed",
        ]),
        browser_args(&[
            "wait",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
        ]),
    ] {
        assert!(parse_browser(args).is_err());
    }

    let err = parse_browser(browser_args(&[
        "wait",
        "--browser-id",
        "browser-1",
        "--snapshot-id",
        "snapshot-1",
        "--document-epoch",
        "1",
        "--url-changed",
        "--text-present",
        "Done",
    ]))
    .unwrap_err();
    assert!(err.contains("only one wait condition"), "error: {err}");
}

#[test]
fn parse_browser_wait_enforces_timeout_bounds() {
    for timeout in ["99", "30001"] {
        let err = parse_browser(browser_args(&[
            "wait",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--url-changed",
            "--timeout-ms",
            timeout,
        ]))
        .unwrap_err();
        assert!(err.contains("between 100 and 30000"), "error: {err}");
    }
}

#[test]
fn parse_browser_screenshot_requires_identity_and_bounds_dimensions() {
    match parse_browser(browser_args(&[
        "screenshot",
        "--browser-id",
        "browser-1",
        "--snapshot-id",
        "snapshot-1",
        "--document-epoch",
        "9",
        "--max-width",
        "1440",
        "--max-height",
        "0",
    ]))
    .unwrap()
    {
        BrowserCommand::Screenshot {
            browser_id,
            snapshot_id,
            document_epoch,
            max_width,
            max_height,
        } => {
            assert_eq!(browser_id, "browser-1");
            assert_eq!(snapshot_id, "snapshot-1");
            assert_eq!(document_epoch, 9);
            assert_eq!(max_width, Some(1440));
            assert_eq!(max_height, Some(0));
        }
        other => panic!("expected Screenshot, got {other:?}"),
    }

    for args in [
        browser_args(&[
            "screenshot",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
        ]),
        browser_args(&[
            "screenshot",
            "--browser-id",
            "browser-1",
            "--document-epoch",
            "1",
        ]),
        browser_args(&[
            "screenshot",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
        ]),
        browser_args(&[
            "screenshot",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--max-width",
            "0",
        ]),
        browser_args(&[
            "screenshot",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--max-height",
            "4097",
        ]),
    ] {
        assert!(parse_browser(args).is_err());
    }
}

#[test]
fn parse_browser_act_builds_a_canonical_native_action() {
    let command = parse_browser(browser_args(&[
        "act",
        "--browser-id",
        "browser-1",
        "--snapshot-id",
        "snapshot-1",
        "--document-epoch",
        "9",
        "--ref",
        "@e3",
        "--fill",
        "Tidebreak",
    ]))
    .unwrap();

    let BrowserCommand::Act {
        browser_id,
        snapshot_id,
        document_epoch,
        target_ref,
        action,
    } = command
    else {
        panic!("expected Act");
    };
    assert_eq!(browser_id, "browser-1");
    assert_eq!(snapshot_id, "snapshot-1");
    assert_eq!(document_epoch, 9);
    assert_eq!(target_ref, "@e3");
    assert_eq!(
        action,
        BrowserAction::Fill {
            value: "Tidebreak".to_owned()
        }
    );
}

#[test]
fn parse_browser_act_requires_identity_and_one_supported_action() {
    for args in [
        browser_args(&[
            "act",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--ref",
            "@e1",
            "--click",
        ]),
        browser_args(&[
            "act",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--ref",
            "@e1",
        ]),
        browser_args(&[
            "act",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--ref",
            "@e1",
            "--click",
            "--focus",
        ]),
        browser_args(&[
            "act",
            "--browser-id",
            "browser-1",
            "--snapshot-id",
            "snapshot-1",
            "--document-epoch",
            "1",
            "--ref",
            "@e1",
            "--press",
            "Ctrl+C",
        ]),
    ] {
        assert!(parse_browser(args).is_err());
    }
}

#[test]
fn browser_mcp_registers_act_only_when_the_capability_is_true() {
    let cap = BrowserCapfile {
        endpoint: "http://127.0.0.1:9876/code/browser".to_owned(),
        token: VALID_TOKEN.to_owned(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();
    assert!(browser_tool_registry(&client, false)
        .get(tidebreak_core::BROWSER_ACT_TOOL)
        .is_none());
    let enabled = browser_tool_registry(&client, true);
    let act = enabled
        .get(tidebreak_core::BROWSER_ACT_TOOL)
        .expect("browser_act must register");
    assert_eq!(act.approval_class(), ApprovalClass::Sensitive);
}

#[test]
fn parse_browser_rejects_unknown_flag() {
    let err = parse_browser(vec!["list".to_string(), "--unknown".to_string()]).unwrap_err();
    assert!(err.contains("no arguments"), "error: {err}");
}

#[test]
fn parse_browser_navigate_rejects_unknown_flag() {
    let err = parse_browser(vec![
        "navigate".to_string(),
        "--browser-id".to_string(),
        "browser-1".to_string(),
        "--url".to_string(),
        "https://example.com".to_string(),
        "--unknown".to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("unknown"), "error: {err}");
}

#[test]
fn parse_browser_rejects_unknown_verb() {
    let err = parse_browser(vec!["unknown".to_string()]).unwrap_err();
    assert!(err.contains("unknown browser command"), "error: {err}");
}

#[test]
fn parse_browser_empty_is_usage() {
    let err = parse_browser(vec![]).unwrap_err();
    assert!(err.contains("usage"), "error: {err}");
}

// -- JSON round-trip contracts -----------------------------------------

#[test]
fn browser_list_result_round_trips() {
    let result = BrowserListResult { sessions: vec![] };
    let json = serde_json::to_string(&result).unwrap();
    let back: BrowserListResult = serde_json::from_str(&json).unwrap();
    assert!(back.sessions.is_empty());
}

#[test]
fn browser_navigate_result_round_trips() {
    use tidebreak_core::BrowserLoadState;
    let result = BrowserNavigateResult {
        browser_id: "browser-1".to_string(),
        url: "https://example.com".to_string(),
        load_state: BrowserLoadState::Ready,
        document_epoch: 5,
    };
    let json = serde_json::to_string(&result).unwrap();
    let back: BrowserNavigateResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back.document_epoch, 5);
}

#[test]
fn browser_snapshot_args_well_formed_validates() {
    let args = BrowserSnapshotArgs {
        browser_id: "browser-123".to_string(),
        max_nodes: None,
    };
    assert!(args.is_well_formed());
}

#[test]
fn browser_wait_summary_reports_status_without_echoing_page_data() {
    let result = BrowserWaitResult {
        browser_id: "browser-1".to_string(),
        status: tidebreak_core::BrowserWaitStatus::Resolved,
        message: "Wait condition satisfied".to_string(),
        document_epoch: 3,
        url: Some("https://example.com".to_string()),
        title: Some("Example".to_string()),
    };

    let summary = format_browser_wait_summary(&result);
    assert!(summary.contains("Resolved"));
    assert!(summary.contains("browser-1"));
    assert!(!summary.contains("https://example.com"));
}

#[test]
fn screenshot_output_attaches_pixels_without_serializing_base64() {
    const ONE_PIXEL_PNG: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let result = BrowserScreenshotResult {
        browser_id: "browser-1".to_string(),
        snapshot_id: "snapshot-1".to_string(),
        document_epoch: 4,
        image_base64: ONE_PIXEL_PNG.to_string(),
        mime_type: "image/png".to_string(),
    };

    let output = screenshot_tool_output(&result).unwrap();
    assert!(!output.content.contains(ONE_PIXEL_PNG));
    assert!(output.data.is_none());
    assert_eq!(output.images.len(), 1);
    assert_eq!(output.images[0].width, 1);
    assert_eq!(output.images[0].height, 1);
    assert!(output.image_data.contains(output.images[0].blob_id));

    let durable = serde_json::to_string(&output).unwrap();
    assert!(!durable.contains(ONE_PIXEL_PNG));
    assert!(!durable.contains("imageBase64"));
}

#[test]
fn screenshot_output_rejects_mismatched_or_invalid_png_data() {
    let mismatched = BrowserScreenshotResult {
        browser_id: "browser-1".to_string(),
        snapshot_id: "snapshot-1".to_string(),
        document_epoch: 4,
        image_base64: "bm90IGEgcG5n".to_string(),
        mime_type: "image/jpeg".to_string(),
    };
    assert!(matches!(
        screenshot_tool_output(&mismatched),
        Err(ClientFailure::ToolFailed { .. })
    ));

    let invalid_png = BrowserScreenshotResult {
        mime_type: "image/png".to_string(),
        ..mismatched
    };
    assert!(matches!(
        screenshot_tool_output(&invalid_png),
        Err(ClientFailure::ToolFailed { .. })
    ));
}

// -- Tool advertisement (five exact tools) ----------------------------

#[test]
fn registry_contains_exactly_five_browser_tools() {
    let client = browser_client_stub();
    let tools = ToolRegistry::new()
        .with(Box::new(BrowserListTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserNavigateTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserSnapshotTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserWaitTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserScreenshotTool {
            client: client.clone(),
        }));
    let specs = tools.specs();
    let mut names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "browser_list",
            "browser_navigate",
            "browser_screenshot",
            "browser_snapshot",
            "browser_wait"
        ]
    );
}

#[test]
fn tool_classes_match_spec() {
    let client = browser_client_stub();
    let list = BrowserListTool {
        client: client.clone(),
    };
    let navigate = BrowserNavigateTool {
        client: client.clone(),
    };
    let snapshot = BrowserSnapshotTool {
        client: client.clone(),
    };
    let wait = BrowserWaitTool {
        client: client.clone(),
    };
    let screenshot = BrowserScreenshotTool {
        client: client.clone(),
    };
    assert_eq!(list.approval_class(), ApprovalClass::ReadOnly);
    assert_eq!(navigate.approval_class(), ApprovalClass::Sensitive);
    assert_eq!(snapshot.approval_class(), ApprovalClass::ReadOnly);
    assert_eq!(wait.approval_class(), ApprovalClass::ReadOnly);
    assert_eq!(screenshot.approval_class(), ApprovalClass::ReadOnly);
}

fn browser_client_stub() -> BrowserClient {
    // Build a client that points nowhere — fine for registration tests
    // since they never send a request.
    BrowserClient {
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        endpoint: "http://127.0.0.1:1/code/browser".to_string(),
        token: "tbreak_bt_00000000-0000-0000-0000-000000000000".to_string(),
    }
}

// -- ClientFailure classification -------------------------------------

#[test]
fn client_failure_maps_http_statuses_to_correct_categories() {
    let cases: &[(u16, ToolErrorCategory)] = &[
        (400, ToolErrorCategory::InvalidArguments),
        (401, ToolErrorCategory::ConfigurationRequired),
        (403, ToolErrorCategory::ConfigurationRequired),
        (404, ToolErrorCategory::NotFound),
        (500, ToolErrorCategory::ToolFailed),
        (501, ToolErrorCategory::ConfigurationRequired),
        (502, ToolErrorCategory::ToolFailed),
    ];
    for (status, expected) in cases {
        let failure = ClientFailure::from_http_status(*status, "test_kind", "test message");
        assert_eq!(
            failure.to_tool_error_category(),
            *expected,
            "status {status}"
        );
    }
}

#[test]
fn client_failure_scrubs_token_from_server_messages() {
    let failure = ClientFailure::from_http_status(
        500,
        "internal",
        &format!("server echoed token {VALID_TOKEN} in error"),
    );
    let text = failure.redacted_text();
    assert!(!text.contains(VALID_TOKEN));
    assert!(text.contains("[redacted]"));
}

#[test]
fn client_failure_scrubs_bearer_keyword() {
    let failure =
        ClientFailure::from_http_status(500, "internal", "Authorization: Bearer secret-token");
    let text = failure.redacted_text();
    assert!(text.contains("[redacted]"));
    assert!(!text.contains("Bearer"));
}

#[test]
fn scrub_server_message_replaces_embedded_uuids() {
    let msg = "resource 550e8400-e29b-41d4-a716-446655440000 is gone";
    let scrubbed = scrub_server_message(msg);
    assert!(!scrubbed.contains("550e8400"));
    assert!(scrubbed.contains("[redacted]"));
}

#[test]
fn scrub_server_message_handles_unicode_before_uuid() {
    let id = uuid::Uuid::nil().to_string();
    let msg = format!("é resource {id} is gone");
    let scrubbed = scrub_server_message(&msg);
    assert!(!scrubbed.contains(&id));
    assert!(scrubbed.starts_with("é resource [redacted]"));
}

// -- HTTP: redirects are not followed ---------------------------------

#[tokio::test]
async fn client_does_not_follow_redirects() {
    // A server that returns 301 → the client must see the 301, not the
    // redirect target.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", addr.port());

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        // Drain the request headers.
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" {
                break;
            }
        }
        // Send a 301 redirect with a body containing a server error.
        let body = serde_json::json!({
            "kind": "redirect",
            "message": "this resource has moved"
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let response = format!(
            "HTTP/1.1 301 Moved Permanently\r\n\
             Location: http://evil.example.com/\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body_bytes.len()
        );
        let mut writer = writer;
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
            .await
            .unwrap();
    });

    // Build a client pointed at *this* listener on an arbitrary port
    // (not the /code/browser path, but that doesn't matter for the
    // redirect test).
    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: VALID_TOKEN.to_string(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();

    // Override endpoint to point at the raw listener (no /code/browser).
    let mut redirect_client = client.clone();
    redirect_client.endpoint = endpoint;

    let result = browser_list(&redirect_client).await;
    // Must fail — reqwest with redirect::Policy::none() returns the 301
    // without following it. Our client treats non-2xx as errors.
    assert!(result.is_err(), "client must not follow redirects");
    let failure = result.unwrap_err();
    // The 301 body should be parsed, giving "redirect" kind.
    let text = failure.redacted_text();
    assert!(text.contains("redirect"), "error text: {text}");

    handle.abort();
}

// -- HTTP: response body bounding -------------------------------------

#[tokio::test]
async fn overlimit_response_body_is_refused() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", addr.port());

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" {
                break;
            }
        }
        // Send a response claiming a tiny body but actually streaming a
        // huge one.
        let padding = "x".repeat(128 * 1024); // 128 KiB > 64 KiB navigate cap
        let response = "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Transfer-Encoding: chunked\r\n\
             Connection: close\r\n\r\n";
        let mut writer = writer;
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        // Write chunked body that exceeds the cap.
        let chunk_header = format!("{:x}\r\n", padding.len());
        tokio::io::AsyncWriteExt::write_all(&mut writer, chunk_header.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, padding.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, b"\r\n")
            .await
            .unwrap();
        // Terminal chunk.
        tokio::io::AsyncWriteExt::write_all(&mut writer, b"0\r\n\r\n")
            .await
            .unwrap();
    });

    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: VALID_TOKEN.to_string(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();
    let mut big_client = client.clone();
    big_client.endpoint = endpoint;

    let result = browser_navigate(
        &big_client,
        &BrowserNavigateArgs {
            browser_id: "browser-1".to_string(),
            url: "https://example.com".to_string(),
        },
    )
    .await;
    assert!(result.is_err(), "oversized body must be refused");
    let text = result.unwrap_err().redacted_text();
    assert!(text.contains("size limit"), "error text: {text}");

    handle.abort();
}

// -- HTTP: success path (actual JSON decode) ---------------------------

#[tokio::test]
async fn browser_list_decodes_successful_json_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", addr.port());

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            headers.push(line);
        }
        let has_auth = headers
            .iter()
            .any(|h| h.to_lowercase().starts_with("authorization"));
        assert!(has_auth, "headers: {headers:?}");

        let body = serde_json::json!({
            "sessions": [{
                "browserId": "browser-1",
                "url": "https://example.com",
                "title": "Example",
                "loadState": "ready",
                "visible": true,
                "engine": {
                    "name": "web_kit_gtk",
                    "capabilities": {
                        "lifecycle": true,
                        "persistentProfile": false,
                        "semanticSnapshot": true,
                        "semanticActions": false,
                        "screenshot": false,
                        "crossOriginFrames": false,
                        "profileReset": false
                    }
                },
                "controller": {
                    "kind": "agent",
                    "halted": false,
                    "takeoverRequired": false
                }
            }]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body_bytes.len()
        );
        let mut writer = writer;
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
            .await
            .unwrap();
    });

    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: VALID_TOKEN.to_string(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();
    let mut test_client = client.clone();
    test_client.endpoint = endpoint;

    let result = browser_list(&test_client).await.unwrap();
    assert_eq!(result.sessions.len(), 1);
    assert_eq!(result.sessions[0].browser_id, "browser-1");
    handle.abort();
}

#[tokio::test]
async fn browser_list_decodes_server_error_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", addr.port());

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" {
                break;
            }
        }
        let body = serde_json::json!({
            "kind": "browser_not_found",
            "message": "no browser session for this capability"
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let response = format!(
            "HTTP/1.1 404 Not Found\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body_bytes.len()
        );
        let mut writer = writer;
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
            .await
            .unwrap();
    });

    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: VALID_TOKEN.to_string(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();
    let mut test_client = client.clone();
    test_client.endpoint = endpoint;

    let err = browser_list(&test_client).await.unwrap_err();
    assert_eq!(err.to_tool_error_category(), ToolErrorCategory::NotFound);
    let text = err.redacted_text();
    assert!(text.contains("browser_not_found"), "error: {text}");
    assert!(text.contains("no browser session"), "error: {text}");
    handle.abort();
}

// -- Token scrubbing in server errors ---------------------------------

#[tokio::test]
async fn server_error_body_containing_token_is_scrubbed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", addr.port());
    let token = VALID_TOKEN;

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" {
                break;
            }
        }
        let body = serde_json::json!({
            "kind": "internal",
            "message": format!("token is {token}")
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let response = format!(
            "HTTP/1.1 500 Internal Server Error\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body_bytes.len()
        );
        let mut writer = writer;
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
            .await
            .unwrap();
    });

    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: token.to_string(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();
    let mut test_client = client.clone();
    test_client.endpoint = endpoint;

    let err = browser_list(&test_client).await.unwrap_err();
    let text = err.redacted_text();
    assert!(!text.contains(token), "token leaked: {text}");
    assert!(text.contains("[redacted]"), "should be scrubbed: {text}");
    assert_eq!(err.to_tool_error_category(), ToolErrorCategory::ToolFailed);
    handle.abort();
}

// -- Navigate and snapshot success paths -------------------------------

#[tokio::test]
async fn browser_navigate_decodes_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", addr.port());

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" {
                break;
            }
            if line.to_lowercase().starts_with("content-length:") {
                content_length = line.split(':').nth(1).unwrap().trim().parse().unwrap_or(0);
            }
        }
        let mut body_bytes = vec![0u8; content_length];
        if content_length > 0 {
            tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body_bytes)
                .await
                .unwrap();
        }
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["browser_id"], "browser-1");
        assert_eq!(body["url"], "https://example.com/");

        let result = serde_json::json!({
            "browserId": "browser-1",
            "url": "https://example.com/",
            "loadState": "loading",
            "documentEpoch": 7
        });
        let result_bytes = serde_json::to_vec(&result).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            result_bytes.len()
        );
        let mut writer = writer;
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &result_bytes)
            .await
            .unwrap();
    });

    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: VALID_TOKEN.to_string(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();
    let mut test_client = client.clone();
    test_client.endpoint = endpoint;

    let args = BrowserNavigateArgs {
        browser_id: "browser-1".to_string(),
        url: "https://example.com/".to_string(),
    };
    let result = browser_navigate(&test_client, &args).await.unwrap();
    assert_eq!(result.document_epoch, 7);
    handle.abort();
}

#[tokio::test]
async fn browser_snapshot_decodes_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", addr.port());

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" {
                break;
            }
            if line.to_lowercase().starts_with("content-length:") {
                content_length = line.split(':').nth(1).unwrap().trim().parse().unwrap_or(0);
            }
        }
        let mut body_bytes = vec![0u8; content_length];
        if content_length > 0 {
            tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body_bytes)
                .await
                .unwrap();
        }
        let result = serde_json::json!({
            "browserId": "browser-1",
            "snapshotId": "snap-1",
            "documentEpoch": 3,
            "contentTrust": "untrusted_page",
            "url": "https://example.com/",
            "title": "Test Page",
            "viewport": {
                "width": 1024.0, "height": 768.0,
                "scrollX": 0.0, "scrollY": 0.0
            },
            "nodes": [],
            "frames": [],
            "truncated": false
        });
        let result_bytes = serde_json::to_vec(&result).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            result_bytes.len()
        );
        let mut writer = writer;
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &result_bytes)
            .await
            .unwrap();
    });

    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: VALID_TOKEN.to_string(),
        semantic_actions: false,
    };
    let client = BrowserClient::new(&cap).unwrap();
    let mut test_client = client.clone();
    test_client.endpoint = endpoint;

    let args = BrowserSnapshotArgs {
        browser_id: "browser-1".to_string(),
        max_nodes: Some(250),
    };
    let result = browser_snapshot(&test_client, &args).await.unwrap();
    assert_eq!(result.title, "Test Page");
    assert_eq!(result.document_epoch, 3);
    handle.abort();
}

// -- MCP failure mapping ----------------------------------------------

#[test]
fn mcp_failure_uses_correct_tool_error_categories() {
    let cases: &[(ClientFailure, ToolErrorCategory)] = &[
        (
            ClientFailure::InvalidArguments {
                detail: "bad".to_string(),
            },
            ToolErrorCategory::InvalidArguments,
        ),
        (
            ClientFailure::NotFound {
                detail: "gone".to_string(),
            },
            ToolErrorCategory::NotFound,
        ),
        (
            ClientFailure::ConfigurationRequired {
                detail: "setup".to_string(),
            },
            ToolErrorCategory::ConfigurationRequired,
        ),
        (
            ClientFailure::TransportFailed {
                detail: "timeout".to_string(),
            },
            ToolErrorCategory::TransportFailed,
        ),
        (
            ClientFailure::ToolFailed {
                detail: "crash".to_string(),
            },
            ToolErrorCategory::ToolFailed,
        ),
    ];
    for (failure, expected) in cases {
        let output = mcp_failure(failure.clone());
        assert_eq!(output.error_category, Some(*expected));
        assert!(output.is_error);
    }
}

// -- Proxy bypass contract -------------------------------------------

/// When an ambient HTTP_PROXY is set, the browser client must ignore it.
/// Capfile endpoints are always http and the agent child inherits
/// HTTP_PROXY/ALL_PROXY, so a missing `.no_proxy()` would route the
/// Authorization bearer through the proxy.
#[tokio::test]
async fn browser_client_ignores_ambient_http_proxy() {
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // A client that honored the ambient proxy would send its bearer here
    // and receive a 502 instead of reaching the browser endpoint.
    let proxy_handle = tokio::spawn(async move {
        let (stream, _) = proxy_listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
                || line == "\r\n"
                || line == "\n"
            {
                break;
            }
        }
        let body = serde_json::json!({
            "kind": "proxy_used",
            "message": "browser request reached ambient proxy"
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let response = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body_bytes.len()
        );
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
            .await
            .unwrap();
    });

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                .await
                .unwrap()
                == 0
            {
                break;
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            headers.push(line);
        }
        let body = serde_json::json!({
            "sessions": []
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body_bytes.len()
        );
        tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
            .await
            .unwrap();
        headers
    });

    let cap = BrowserCapfile {
        endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
        token: VALID_TOKEN.to_string(),
        semantic_actions: false,
    };
    let client = {
        let _env_lock = PROXY_ENV_LOCK.lock().await;
        let proxy_url = format!("http://{proxy_addr}");
        let _proxy_env = [
            ScopedEnv::set("HTTP_PROXY", &proxy_url),
            ScopedEnv::set("http_proxy", &proxy_url),
            ScopedEnv::set("ALL_PROXY", &proxy_url),
            ScopedEnv::set("all_proxy", &proxy_url),
            ScopedEnv::set("NO_PROXY", ""),
            ScopedEnv::set("no_proxy", ""),
        ];
        BrowserClient::new(&cap).unwrap()
    };

    let result = browser_list(&client).await;
    let req_headers = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("direct browser endpoint was not reached")
        .unwrap();
    proxy_handle.abort();
    let _ = proxy_handle.await;

    // The request must reach the listener, not the bogus proxy.
    assert!(
        result.is_ok(),
        "request must bypass ambient proxy: {result:?}"
    );

    // Authorization header must be present — the token reaches the real
    // endpoint, not an intercepted proxy.
    let has_auth = req_headers
        .iter()
        .any(|h| h.to_lowercase().starts_with("authorization"));
    assert!(
        has_auth,
        "Authorization must reach the listener: {req_headers:?}"
    );
}
