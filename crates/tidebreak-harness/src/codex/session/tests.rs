use super::*;
use std::path::PathBuf;

#[test]
fn app_server_plan_is_clean() {
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        None,
        None,
    )
    .unwrap();
    assert_eq!(plan.argv, ["/usr/bin/codex", "app-server", "--stdio"]);
    validate_launch_plan(&plan).unwrap();
}

#[test]
fn extra_bypass_flag_is_rejected() {
    let err = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &["--dangerously-bypass-approvals-and-sandbox".into()],
        std::path::Path::new("/workspace"),
        &[],
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, HarnessError::LaunchRejected(_)));
}

#[test]
fn permission_mode_mapping_matches_0033() {
    assert_eq!(
        thread_start_policy(PermissionMode::Plan),
        ("read-only", "untrusted")
    );
    assert_eq!(
        thread_start_policy(PermissionMode::Ask),
        ("workspace-write", "untrusted")
    );
    assert_eq!(
        thread_start_policy(PermissionMode::Auto),
        ("workspace-write", "on-request")
    );
    assert_eq!(
        thread_start_policy(PermissionMode::Allow),
        ("danger-full-access", "never")
    );
    let _ = PathBuf::from("/workspace");
}

#[test]
fn thread_loads_allow_a_longer_inactivity_window_than_initialization() {
    assert!(THREAD_LOAD_TIMEOUT > HANDSHAKE_TIMEOUT);
    assert_eq!(THREAD_LOAD_TIMEOUT, Duration::from_secs(120));
    assert!(THREAD_LOAD_ABSOLUTE_CEILING > THREAD_LOAD_TIMEOUT);
}

/// A stand-in `codex app-server --stdio` that speaks just enough of the
/// 0.147.0 protocol to reproduce the resume hazard: it answers
/// `thread/resume` for an unknown thread the way codex does, and records
/// every method it was asked for so a test can assert what was on the
/// wire. Recorded shapes come from `fixtures/codex/0.147.0/`.
#[cfg(unix)]
const FAKE_APP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
*'"method":"initialize"'*)
  printf 'initialize\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"result":{"userAgent":"fake/0.147.0"}}\n' "$id"
  ;;
*'"method":"thread/start"'*)
  printf 'thread/start\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
  ;;
*'"method":"thread/resume"'*)
  printf 'thread/resume\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"error":{"code":-32603,"message":"thread not found: STALE-THREAD"}}\n' "$id"
  ;;
*'"method":"turn/start"'*)
  printf 'turn/start\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"result":{"turn":{"id":"TURN-1","status":"inProgress"}}}\n' "$id"
  printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"completed"}}}\n'
  ;;
  esac
done
"#;

/// The same stand-in, but its `thread/resume` succeeds — the engine still
/// holds the thread, as it does after a park (decision 0064).
#[cfg(unix)]
const FAKE_RESUMABLE_APP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
*'"method":"initialize"'*)
  printf 'initialize\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"result":{"userAgent":"fake/0.147.0"}}\n' "$id"
  ;;
*'"method":"thread/start"'*)
  printf 'thread/start\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
  ;;
*'"method":"thread/resume"'*)
  printf 'thread/resume\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
  ;;
*'"method":"turn/start"'*)
  printf 'turn/start\n' >>"$FAKE_CODEX_CALLS"
  printf '{"id":%s,"result":{"turn":{"id":"TURN-1","status":"inProgress"}}}\n' "$id"
  printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"completed"}}}\n'
  ;;
  esac
done
"#;

#[cfg(unix)]
const FAKE_STEERING_APP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
*'"method":"initialize"'*)
  printf '{"id":%s,"result":{"userAgent":"fake/0.147.0"}}\n' "$id"
  ;;
*'"method":"thread/start"'*)
  printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
  ;;
*'"method":"turn/start"'*)
  printf '{"id":%s,"result":{"turn":{"id":"TURN-1","status":"inProgress"}}}\n' "$id"
  printf '{"method":"turn/started","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"inProgress"}}}\n'
  ;;
*'"method":"turn/steer"'*)
  printf '%s\n' "$line" >"$FAKE_CODEX_STEER"
  printf '{"id":%s,"result":{"turnId":"TURN-1"}}\n' "$id"
  printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"completed"}}}\n'
  ;;
  esac
done
"#;

#[cfg(unix)]
const FAKE_POSTURE_APP_SERVER: &str = r#"#!/bin/sh
turns=0
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
*'"method":"initialize"'*)
  printf '{"id":%s,"result":{"userAgent":"fake/0.147.0"}}\n' "$id"
  ;;
*'"method":"thread/start"'*)
  printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
  ;;
*'"method":"turn/start"'*)
  turns=$((turns+1))
  printf '%s\n' "$line" >>"$FAKE_CODEX_TURNS"
  if [ "$turns" -eq 1 ]; then
    printf '{"id":%s,"error":{"code":-32602,"message":"invalid turn options"}}\n' "$id"
  else
    printf '{"id":%s,"result":{"turn":{"id":"TURN-%s","status":"inProgress"}}}\n' "$id" "$turns"
    printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-%s","status":"completed"}}}\n' "$turns"
  fi
  ;;
  esac
done
"#;

#[cfg(unix)]
const FAKE_INTERRUPT_APP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
*'"method":"initialize"'*)
  printf '{"id":%s,"result":{"userAgent":"fake/0.147.0"}}\n' "$id"
  ;;
*'"method":"thread/start"'*)
  printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
  ;;
*'"method":"turn/start"'*)
  printf '{"id":%s,"result":{"turn":{"id":"TURN-1","status":"inProgress"}}}\n' "$id"
  printf '{"method":"turn/started","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"inProgress"}}}\n'
  ;;
*'"method":"turn/interrupt"'*)
  printf '%s\n' "$line" >>"$FAKE_CODEX_INTERRUPTS"
  case "$FAKE_CODEX_INTERRUPT_MODE" in
    success)
      printf '{"id":%s,"result":{}}\n' "$id"
      printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"interrupted"}}}\n'
      ;;
    error)
      printf '{"id":%s,"error":{"code":-32000,"message":"turn is no longer active"}}\n' "$id"
      ;;
    eof)
      exit 0
      ;;
    timeout)
      :
      ;;
  esac
  ;;
  esac
done
"#;

#[cfg(unix)]
fn write_fake_app_server(path: &std::path::Path) {
    write_app_server(path, FAKE_APP_SERVER);
}

#[cfg(unix)]
fn write_app_server(path: &std::path::Path, script: &str) {
    // Write a sibling inode, fsync, then rename over `path` so execve
    // never sees a file that still has a writer (Linux ETXTBSY).
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let staging = path.with_extension("writing");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(&staging)
        .unwrap();
    file.write_all(script.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::rename(&staging, path).unwrap();
    if let Some(parent) = path.parent() {
        let dir = std::fs::File::open(parent).unwrap();
        dir.sync_all().unwrap();
    }
}

#[cfg(unix)]
struct SilentSink;

#[cfg(unix)]
#[async_trait]
impl crate::HarnessEventSink for SilentSink {
    async fn emit(&self, _event: HarnessEvent) {}
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<HarnessEvent>>,
}

#[async_trait]
impl crate::HarnessEventSink for RecordingSink {
    async fn emit(&self, event: HarnessEvent) {
        self.events.lock().expect("codex test events").push(event);
    }
}

fn unit_session(sink: Arc<dyn crate::HarnessEventSink>) -> CodexSession {
    CodexSession::new(SessionSpec {
        owner: tidebreak_core::OwnerId::local(),
        session_id: tidebreak_core::CodeSessionId::new(),
        worktree: PathBuf::from("."),
        allowed_read_roots: Vec::new(),
        permission_mode: PermissionMode::Auto,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        resume_ref: Some("THREAD-1".into()),
        extra_argv: Vec::new(),
        extra_env: Vec::new(),
        relay_key_env: None,
        env: Vec::new(),
        approval: None,
        binary: Some(PathBuf::from("codex")),
        sink,
        browser: None,
    })
}

#[cfg(unix)]
async fn session_reading_script(script: &str) -> (CodexSession, tokio::process::Child) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let session = unit_session(Arc::new(SilentSink));
    *session.stdout.lock().expect("codex stdout") = Some(Arc::new(AsyncMutex::new(StdoutReader {
        stdout,
        lines: StreamLineBuffer::new(),
    })));
    (session, child)
}

/// History that keeps arriving past one inactivity window must still
/// complete: the old fixed deadline would kill this restore.
#[cfg(unix)]
#[tokio::test]
async fn read_until_rpc_resets_inactivity_timeout_on_each_batch() {
    let inactivity = Duration::from_millis(200);
    let (session, mut child) = session_reading_script(
        r#"
i=0
while [ "$i" -lt 4 ]; do
  printf '{"method":"item/completed","params":{"id":%s}}\n' "$i"
  sleep 0.12
  i=$((i + 1))
done
printf '{"id":7,"result":{"thread":{"id":"THREAD-1"}}}\n'
sleep 2
"#,
    )
    .await;
    session
        .read_until_rpc(7, inactivity, THREAD_LOAD_ABSOLUTE_CEILING)
        .await
        .expect("streaming restore should outlive one inactivity window");
    let _ = child.kill().await;
}

/// `thread/resume` returns the entire persisted thread in one response. A
/// healthy response larger than the normal turn-event limit must still reach
/// the matching RPC waiter.
#[cfg(unix)]
#[tokio::test]
async fn read_until_rpc_accepts_a_large_thread_resume_response() {
    let payload_bytes = StreamBudget::default().max_partial_line + 1_024;
    let script = format!(
        r#"
payload=$(printf '%*s' {payload_bytes} '' | tr ' ' a)
printf '{{"id":7,"result":{{"thread":{{"id":"THREAD-1","turns":[{{"content":"%s"}}]}}}}}}\n' "$payload"
sleep 2
"#
    );
    let (session, mut child) = session_reading_script(&script).await;
    session
        .read_until_rpc(7, Duration::from_secs(2), THREAD_LOAD_ABSOLUTE_CEILING)
        .await
        .expect("a valid response above the normal line limit should be read");
    let _ = child.kill().await;
}

/// Even startup responses stay bounded. If Codex exceeds its larger RPC
/// budget, fail at the overflow instead of waiting for an RPC timeout.
#[cfg(unix)]
#[tokio::test]
async fn rpc_line_overflow_fails_immediately() {
    let (session, mut child) = session_reading_script(
        r#"
printf '{"id":7,"result":{"thread":{"id":"THREAD-1","turns":[{"content":"'
printf '%*s' 80 '' | tr ' ' a
printf '"}]}}}\n'
sleep 2
"#,
    )
    .await;
    let budget = StreamBudget {
        max_partial_line: 64,
        ..StreamBudget::default()
    };
    let err = session
        .read_lines_with_budget(budget, true)
        .await
        .expect_err("an oversized RPC line should fail at the parse budget");
    match err {
        HarnessError::Other(message) => assert_eq!(
            message,
            "engine stdout line exceeded the 64 byte parse budget"
        ),
        other => panic!("expected a parse-budget error, got {other:?}"),
    }
    let _ = child.kill().await;
}

/// A child that goes silent is still bounded by one inactivity window.
#[cfg(unix)]
#[tokio::test]
async fn read_until_rpc_times_out_when_the_child_goes_silent() {
    let inactivity = Duration::from_millis(200);
    let (session, mut child) = session_reading_script("sleep 2").await;
    let err = session
        .read_until_rpc(7, inactivity, THREAD_LOAD_ABSOLUTE_CEILING)
        .await
        .unwrap_err();
    match err {
        HarnessError::Other(message) => {
            assert!(
                message.contains("timed out waiting for rpc id 7"),
                "unexpected timeout message: {message}"
            );
        }
        other => panic!("expected inactivity timeout, got {other:?}"),
    }
    let _ = child.kill().await;
}

/// A child that exits without answering fails at once instead of
/// spinning on empty batches until the ceiling.
#[cfg(unix)]
#[tokio::test]
async fn read_until_rpc_fails_promptly_when_the_child_exits() {
    let inactivity = Duration::from_secs(5);
    let (session, mut child) =
        session_reading_script(r#"printf '{"method":"item/completed","params":{"id":0}}\n'"#).await;
    let started = Instant::now();
    let err = session
        .read_until_rpc(7, inactivity, THREAD_LOAD_ABSOLUTE_CEILING)
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < inactivity,
        "EOF should not wait for the inactivity window"
    );
    match err {
        HarnessError::Other(message) => {
            assert!(
                message.contains("exited before answering rpc id 7"),
                "unexpected EOF message: {message}"
            );
        }
        other => panic!("expected an EOF error, got {other:?}"),
    }
    let _ = child.kill().await;
}

fn register_pending(
    session: &CodexSession,
    rpc_id: i64,
    write_state: PendingWriteState,
    deadline: Option<Instant>,
) -> oneshot::Receiver<Result<(), HarnessError>> {
    let (reply, receiver) = oneshot::channel();
    session
        .parser
        .lock()
        .expect("codex parser")
        .note_outbound(&json!(rpc_id), "turn/steer");
    let mut state = session.control_state.lock().expect("codex control state");
    state.turn = ControlTurn::Active("TURN-1".into());
    state.pending.insert(
        rpc_id,
        PendingSteer {
            expected_turn_id: "TURN-1".into(),
            text: "redirect".into(),
            reply: Some(reply),
            deadline,
            write_state,
            accept_response: true,
        },
    );
    receiver
}

#[cfg(unix)]
fn spec_for(
    dir: &std::path::Path,
    binary: &std::path::Path,
    resume_ref: Option<String>,
) -> SessionSpec {
    SessionSpec {
        owner: tidebreak_core::OwnerId::local(),
        session_id: tidebreak_core::CodeSessionId::new(),
        worktree: dir.to_path_buf(),
        allowed_read_roots: Vec::new(),
        permission_mode: PermissionMode::Auto,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        resume_ref,
        extra_argv: Vec::new(),
        extra_env: vec![(
            "FAKE_CODEX_CALLS".into(),
            dir.join("calls").to_string_lossy().into_owned(),
        )],
        relay_key_env: None,
        env: Vec::new(),
        approval: None,
        binary: Some(binary.to_path_buf()),
        sink: Arc::new(SilentSink),
        browser: None,
    }
}

#[cfg(unix)]
fn calls(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
fn turn(text: &str) -> TurnInput {
    TurnInput {
        turn_id: None,
        text: text.into(),
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        images: Vec::new(),
    }
}

#[cfg(unix)]
async fn run_interrupt_case(
    mode: &str,
) -> (
    Result<TurnOutcome, HarnessError>,
    Result<(), HarnessError>,
    bool,
    bool,
) {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codex");
    write_app_server(&binary, FAKE_INTERRUPT_APP_SERVER);
    let mut spec = spec_for(dir.path(), &binary, None);
    spec.extra_env
        .push(("FAKE_CODEX_INTERRUPT_MODE".into(), mode.to_owned()));
    spec.extra_env.push((
        "FAKE_CODEX_INTERRUPTS".into(),
        dir.path()
            .join("interrupts.ndjson")
            .to_string_lossy()
            .into_owned(),
    ));
    let session = Arc::new(CodexSession::new(spec));
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.run_turn(turn("keep working")).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.active_control_turn_id().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fake turn was never admitted");

    let stopped = session.interrupt().await;
    let outcome = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .expect("fake turn did not finish")
        .expect("turn task panicked");
    let child_alive = session.child_pid().is_some();
    let request_written = std::fs::read_to_string(dir.path().join("interrupts.ndjson"))
        .is_ok_and(|input| input.lines().count() == 1);
    session.park().await.unwrap();
    (outcome, stopped, child_alive, request_written)
}

/// The wedge from the app-server dying before its first turn: codex
/// never persisted the thread, so a thread id that has run no turn must
/// not be reported as a resume ref. The next spawn then starts a clean
/// thread.
#[cfg(unix)]
#[tokio::test]
async fn a_thread_that_ran_no_turn_is_not_a_resume_ref() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codex");
    write_fake_app_server(&binary);

    let session = CodexSession::new(spec_for(dir.path(), &binary, None));
    assert!(
        calls(dir.path()).is_empty(),
        "nothing spawns before the first turn (decision 0064)"
    );
    assert_eq!(session.child_pid(), None);
    assert_eq!(
        session.resume_ref(),
        None,
        "a thread with no turns is not resumable and must not be persisted"
    );

    session.run_turn(turn("first turn")).await.unwrap();
    assert_eq!(
        calls(dir.path()),
        ["initialize", "thread/start", "turn/start"],
        "the first turn spawns, handshakes, and runs"
    );
    assert_eq!(session.resume_ref().as_deref(), Some("THREAD-1"));
}

/// A resume ref the engine no longer knows is a lost resume, not a turn
/// failure: the server fences on this rather than failing every turn.
/// With the child spawned on the first turn (decision 0064), that is
/// where the stored ref meets the engine.
#[cfg(unix)]
#[tokio::test]
async fn a_stale_resume_ref_reports_a_lost_resume() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codex");
    write_fake_app_server(&binary);

    let session = CodexSession::new(spec_for(
        dir.path(),
        &binary,
        Some("STALE-THREAD".to_owned()),
    ));
    let Err(err) = session.run_turn(turn("first turn")).await else {
        panic!("a turn on an unknown thread must not succeed");
    };
    assert_eq!(calls(dir.path()), ["initialize", "thread/resume"]);
    let HarnessError::ResumeLost(detail) = err else {
        panic!("expected a lost resume, got {err}");
    };
    assert!(detail.contains("thread not found"), "detail: {detail}");
    assert_eq!(
        session.child_pid(),
        None,
        "a failed handshake leaves no half-attached child behind"
    );
}

/// Decision 0064: a parked thread that has run resumes on a replacement
/// child, with `thread/resume` on the wire and the same thread id kept.
#[cfg(unix)]
#[tokio::test]
async fn a_parked_thread_is_resumed_on_the_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codex");
    write_app_server(&binary, FAKE_RESUMABLE_APP_SERVER);

    let session = CodexSession::new(spec_for(dir.path(), &binary, None));
    session.run_turn(turn("one")).await.unwrap();
    let first_pid = session.child_pid().expect("the child outlives its turn");

    session.park().await.unwrap();
    assert_eq!(session.child_pid(), None, "the parked child is gone");

    session.run_turn(turn("two")).await.unwrap();
    assert_eq!(
        calls(dir.path()),
        [
            "initialize",
            "thread/start",
            "turn/start",
            "initialize",
            "thread/resume",
            "turn/start"
        ],
        "the wake respawns and resumes rather than starting a new thread"
    );
    let second_pid = session.child_pid().expect("the wake turn spawned a child");
    assert_ne!(first_pid, second_pid, "a new process answered the wake");
    assert_eq!(session.resume_ref().as_deref(), Some("THREAD-1"));
}

/// Codex never persisted a thread that ran no turn, so waking one must
/// start clean. Resuming it would fence the session on "thread not
/// found" — the fake's own answer catches a wrong ensure here.
#[cfg(unix)]
#[tokio::test]
async fn a_parked_thread_that_never_ran_is_restarted_not_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codex");
    write_fake_app_server(&binary);

    let session = CodexSession::new(spec_for(dir.path(), &binary, None));
    session.ensure_child().await.unwrap();
    session.park().await.unwrap();
    session.ensure_child().await.unwrap();
    assert_eq!(
        calls(dir.path()),
        ["initialize", "thread/start", "initialize", "thread/start"],
        "an unwritten thread is restarted, never resumed"
    );
}

/// A stop aimed at a parked session must not fail and must not spawn
/// anything (decision 0064).
#[tokio::test]
async fn an_interrupt_with_no_child_is_a_no_op() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    session.interrupt().await.unwrap();
    assert_eq!(session.child_pid(), None);
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_turn_start_keeps_posture_pending() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codex");
    write_app_server(&binary, FAKE_POSTURE_APP_SERVER);
    let mut spec = spec_for(dir.path(), &binary, None);
    spec.extra_env.push((
        "FAKE_CODEX_TURNS".into(),
        dir.path()
            .join("turns.ndjson")
            .to_string_lossy()
            .into_owned(),
    ));
    let session = CodexSession::new(spec);
    session
        .set_permission_mode(PermissionMode::Plan)
        .await
        .unwrap();

    session.run_turn(turn("first")).await.unwrap();
    assert!(
        session.pending_posture().is_some(),
        "a rejected turn must leave its posture armed"
    );
    session.run_turn(turn("second")).await.unwrap();
    assert!(
        session.pending_posture().is_none(),
        "the matching accepted retry settles the posture"
    );

    let requests = std::fs::read_to_string(dir.path().join("turns.ndjson")).unwrap();
    let requests = requests
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(request["params"]["sandboxPolicy"]["type"], "readOnly");
        assert_eq!(request["params"]["approvalPolicy"], "untrusted");
    }
}

#[tokio::test]
async fn late_success_cannot_clear_newer_posture_generation() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let first = session.pending_posture().expect("resume posture is armed");
    session.register_turn_admission(41, "THREAD-1".into(), Some(first.id));
    let newer = session.arm_posture(PermissionMode::Plan);

    session
        .emit_parsed(r#"{"id":41,"result":{"turn":{"id":"TURN-OLD"}}}"#)
        .await;

    assert_eq!(session.pending_posture(), Some(newer));
}

#[test]
fn admission_timeout_keeps_posture_pending() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let generation = session.pending_posture().expect("resume posture is armed");
    session.register_turn_admission(41, "THREAD-1".into(), Some(generation.id));
    session
        .posture
        .lock()
        .expect("codex posture")
        .admission
        .as_mut()
        .expect("turn admission")
        .deadline = Instant::now();

    assert!(session.expire_turn_admission());
    assert_eq!(session.pending_posture(), Some(generation));
}

#[tokio::test]
async fn malformed_turn_start_result_keeps_posture_pending() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let generation = session.pending_posture().expect("resume posture is armed");
    session
        .parser
        .lock()
        .expect("codex parser")
        .note_outbound(&json!(41), "turn/start");
    session.register_turn_admission(41, "THREAD-1".into(), Some(generation.id));

    let events = session.emit_parsed(r#"{"id":41,"result":{}}"#).await;

    assert!(matches!(
        events.last(),
        Some(HarnessEvent::TurnFailed { .. })
    ));
    assert_eq!(session.pending_posture(), Some(generation));
}

#[tokio::test]
async fn terminal_before_admission_keeps_posture_pending() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let generation = session.pending_posture().expect("resume posture is armed");
    session.register_turn_admission(41, "THREAD-1".into(), Some(generation.id));

    session
        .emit_parsed(
            r#"{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"failed"}}}"#,
        )
        .await;

    assert_eq!(session.pending_posture(), Some(generation));
}

#[cfg(unix)]
#[tokio::test]
async fn codex_interrupt_waits_for_correlated_success() {
    let (outcome, stopped, child_alive, request_written) = run_interrupt_case("success").await;
    stopped.unwrap();
    assert!(matches!(outcome.unwrap(), TurnOutcome::Clean));
    assert!(child_alive, "a native interrupt keeps the session child");
    assert!(request_written);
}

#[cfg(unix)]
#[tokio::test]
async fn codex_interrupt_error_falls_back_to_the_process_tree() {
    let (outcome, stopped, child_alive, request_written) = run_interrupt_case("error").await;
    stopped.unwrap();
    assert!(outcome.is_err());
    assert!(!child_alive);
    assert!(request_written);
}

#[cfg(unix)]
#[tokio::test]
async fn codex_interrupt_timeout_falls_back_to_the_process_tree() {
    let (outcome, stopped, child_alive, request_written) = run_interrupt_case("timeout").await;
    stopped.unwrap();
    assert!(outcome.is_err());
    assert!(!child_alive);
    assert!(request_written);
}

#[tokio::test]
async fn a_late_codex_interrupt_response_cannot_resolve_a_new_waiter() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let old = session.register_interrupt(52);
    session.cancel_interrupt(52, "timed out");
    assert!(old.await.unwrap().is_err());
    let current = session.register_interrupt(53);

    session.emit_parsed(r#"{"id":52,"result":{}}"#).await;
    assert_eq!(
        session
            .control_state
            .lock()
            .expect("codex control state")
            .interrupt
            .as_ref()
            .map(|pending| pending.rpc_id),
        Some(53)
    );

    session.emit_parsed(r#"{"id":53,"result":{}}"#).await;
    current.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn codex_interrupt_eof_falls_back_to_the_process_tree() {
    let (outcome, stopped, child_alive, request_written) = run_interrupt_case("eof").await;
    stopped.unwrap();
    assert!(outcome.is_err());
    assert!(!child_alive);
    assert!(request_written);
}

#[test]
fn steer_response_must_acknowledge_the_expected_turn() {
    validate_steer_response(&json!({ "result": { "turnId": "TURN-1" } }), "TURN-1").unwrap();
    let mismatch = validate_steer_response(&json!({ "result": { "turnId": "TURN-2" } }), "TURN-1")
        .unwrap_err();
    assert!(matches!(mismatch, HarnessError::SteeringRejected(_)));
    let rejected = validate_steer_response(
        &json!({ "error": { "message": "turn is no longer steerable" } }),
        "TURN-1",
    )
    .unwrap_err();
    assert!(matches!(rejected, HarnessError::SteeringRejected(_)));
}

#[test]
fn native_steer_request_matches_the_verified_json_shape() {
    let request = steer_request(
        41,
        "THREAD-1",
        "TURN-1",
        "try the other file",
        "tidebreak-steer-41",
    );
    assert_eq!(request["id"], 41);
    assert_eq!(request["method"], "turn/steer");
    assert_eq!(request["params"]["threadId"], "THREAD-1");
    assert_eq!(request["params"]["expectedTurnId"], "TURN-1");
    assert_eq!(
        request["params"]["input"],
        json!([{
            "type": "text",
            "text": "try the other file"
        }])
    );
    assert_eq!(
        request["params"]["clientUserMessageId"],
        "tidebreak-steer-41"
    );
}

#[test]
fn only_true_json_rpc_responses_match_waiters() {
    assert!(!is_rpc_response(&json!({
        "id": 7,
        "method": "item/commandExecution/requestApproval",
        "params": { "itemId": "call-1" }
    })));
    assert!(is_rpc_response(&json!({
        "id": 7,
        "result": { "turnId": "TURN-1" }
    })));
    assert!(is_rpc_response(&json!({
        "id": "7",
        "error": { "code": -32602, "message": "rejected" }
    })));
}

#[tokio::test]
async fn same_id_server_request_does_not_resolve_steering() {
    let sink = Arc::new(RecordingSink::default());
    let session = unit_session(sink.clone());
    let receiver = register_pending(
        &session,
        7,
        PendingWriteState::Written,
        Some(Instant::now() + CONTROL_RPC_TIMEOUT),
    );

    session
        .emit_parsed(
            r#"{"id":7,"method":"item/commandExecution/requestApproval","params":{"itemId":"call-1"}}"#,
        )
        .await;
    assert!(session
        .control_state
        .lock()
        .expect("codex control state")
        .pending
        .contains_key(&7));

    session
        .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
        .await;
    receiver.await.unwrap().unwrap();
    let events = sink.events.lock().expect("codex test events");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, HarnessEvent::UserSteered { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn terminal_closes_admission_and_rejects_queued_steering() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let receiver = register_pending(&session, 7, PendingWriteState::Queued, None);

    session
        .emit_parsed(
            r#"{"method":"turn/completed","params":{"turn":{"id":"TURN-1","status":"completed"}}}"#,
        )
        .await;

    {
        let state = session.control_state.lock().expect("codex control state");
        assert_eq!(state.turn, ControlTurn::Closed);
        assert!(state.pending.is_empty());
    }
    assert!(matches!(
        receiver.await.unwrap(),
        Err(HarnessError::SteeringRejected(_))
    ));
}

#[tokio::test]
async fn terminal_race_consumes_an_inflight_ack_without_a_steer_event() {
    let sink = Arc::new(RecordingSink::default());
    let session = unit_session(sink.clone());
    let receiver = register_pending(
        &session,
        7,
        PendingWriteState::Writing,
        Some(Instant::now() + CONTROL_RPC_TIMEOUT),
    );

    session
        .emit_parsed(
            r#"{"method":"turn/completed","params":{"turn":{"id":"TURN-1","status":"completed"}}}"#,
        )
        .await;
    assert!(matches!(
        receiver.await.unwrap(),
        Err(HarnessError::SteeringRejected(_))
    ));
    assert!(session
        .control_state
        .lock()
        .expect("codex control state")
        .pending
        .contains_key(&7));

    session
        .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
        .await;
    assert!(session
        .control_state
        .lock()
        .expect("codex control state")
        .pending
        .is_empty());
    assert!(!sink
        .events
        .lock()
        .expect("codex test events")
        .iter()
        .any(|event| matches!(event, HarnessEvent::UserSteered { .. })));
}

#[test]
fn cancellation_before_write_removes_the_registration() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let receiver = register_pending(&session, 7, PendingWriteState::Queued, None);
    drop(receiver);
    {
        let _registration = ControlRegistration {
            session: &session,
            rpc_id: 7,
            armed: true,
        };
    }
    assert!(session
        .control_state
        .lock()
        .expect("codex control state")
        .pending
        .is_empty());
}

#[tokio::test]
async fn caller_cancellation_after_native_acceptance_keeps_user_steered() {
    let sink = Arc::new(RecordingSink::default());
    let session = unit_session(sink.clone());
    let receiver = register_pending(
        &session,
        7,
        PendingWriteState::Written,
        Some(Instant::now() + CONTROL_RPC_TIMEOUT),
    );
    drop(receiver);

    session
        .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
        .await;

    let events = sink.events.lock().expect("codex test events");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, HarnessEvent::UserSteered { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn user_steered_is_emitted_before_turn_completed() {
    let sink = Arc::new(RecordingSink::default());
    let session = unit_session(sink.clone());
    let receiver = register_pending(
        &session,
        7,
        PendingWriteState::Written,
        Some(Instant::now() + CONTROL_RPC_TIMEOUT),
    );

    session
        .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
        .await;
    session
        .emit_parsed(
            r#"{"method":"turn/completed","params":{"turn":{"id":"TURN-1","status":"completed"}}}"#,
        )
        .await;
    receiver.await.unwrap().unwrap();

    let events = sink.events.lock().expect("codex test events");
    let steered = events
        .iter()
        .position(|event| matches!(event, HarnessEvent::UserSteered { .. }))
        .unwrap();
    let completed = events
        .iter()
        .position(|event| matches!(event, HarnessEvent::TurnCompleted { .. }))
        .unwrap();
    assert!(steered < completed);
}

#[tokio::test]
async fn rejected_or_mismatched_ack_never_emits_user_steered() {
    for response in [
        json!({ "id": 7, "error": { "message": "not steerable" } }),
        json!({ "id": 7, "result": { "turnId": "TURN-2" } }),
    ] {
        let sink = Arc::new(RecordingSink::default());
        let session = unit_session(sink.clone());
        let receiver = register_pending(
            &session,
            7,
            PendingWriteState::Written,
            Some(Instant::now() + CONTROL_RPC_TIMEOUT),
        );
        session.emit_parsed(&response.to_string()).await;
        assert!(matches!(
            receiver.await.unwrap(),
            Err(HarnessError::SteeringRejected(_))
        ));
        assert!(!sink
            .events
            .lock()
            .expect("codex test events")
            .iter()
            .any(|event| matches!(event, HarnessEvent::UserSteered { .. })));
    }
}

#[tokio::test]
async fn control_timeout_cleans_pending_state() {
    let session = unit_session(Arc::new(RecordingSink::default()));
    let receiver = register_pending(
        &session,
        7,
        PendingWriteState::Written,
        Some(Instant::now()),
    );
    session.expire_control_requests();
    assert!(session
        .control_state
        .lock()
        .expect("codex control state")
        .pending
        .is_empty());
    assert!(matches!(
        receiver.await.unwrap(),
        Err(HarnessError::SteeringRejected(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn native_steer_uses_the_active_turn_id_and_waits_for_ack() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codex");
    write_app_server(&binary, FAKE_STEERING_APP_SERVER);
    let mut spec = spec_for(dir.path(), &binary, None);
    let sink = Arc::new(RecordingSink::default());
    spec.sink = sink.clone();
    spec.extra_env.push((
        "FAKE_CODEX_STEER".into(),
        dir.path().join("steer.json").to_string_lossy().into_owned(),
    ));
    let session = Arc::new(CodexSession::new(spec));
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .run_turn(TurnInput {
                    turn_id: None,
                    text: "first turn".into(),
                    model: None,
                    reasoning_effort: None,
                    fast_mode: false,
                    images: Vec::new(),
                })
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if session.active_control_turn_id().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fake turn was never acknowledged");

    session.steer("try the other file".into()).await.unwrap();
    running.await.unwrap().unwrap();

    let request: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("steer.json")).unwrap())
            .unwrap();
    assert_eq!(request["method"], "turn/steer");
    assert_eq!(request["params"]["threadId"], "THREAD-1");
    assert_eq!(request["params"]["expectedTurnId"], "TURN-1");
    assert_eq!(request["params"]["input"][0]["type"], "text");
    assert_eq!(request["params"]["input"][0]["text"], "try the other file");
    let steers: Vec<_> = sink
        .events
        .lock()
        .expect("codex test events")
        .iter()
        .filter_map(|event| match event {
            HarnessEvent::UserSteered { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(steers, ["try the other file"]);
}

// ── Browser MCP advertisement contract tests ──

#[test]
fn browser_absent_produces_same_argv_as_before() {
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        None,
        None,
    )
    .unwrap();
    assert_eq!(plan.argv, ["/usr/bin/codex", "app-server", "--stdio"]);
}

#[test]
fn browser_present_appends_exactly_one_trusted_config_override() {
    let spec = BrowserChannelSpec::new(
        std::path::PathBuf::from("/tmp/browser-cap.json"),
        std::path::PathBuf::from("/usr/local/bin/tidebreak"),
    );
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        Some(&spec),
        None,
    )
    .unwrap();
    let overrides: Vec<_> = plan
        .argv
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "-c")
        .collect();
    assert_eq!(overrides.len(), 1);
    let idx = overrides[0].0;
    let value = &plan.argv[idx + 1];
    assert!(value.contains("mcp_servers.tb-browser"));
    assert!(value.contains("command=\"/usr/local/bin/tidebreak\""));
    assert!(value.contains(r#"args=["browser-mcp"]"#));
    assert!(value.contains(r#"env_vars=["TIDEBREAK_BROWSER_CAPFILE"]"#));
    // The override names the env var but must not contain a capfile path or token value.
    let capfile_str = spec.capability_file.to_string_lossy();
    assert!(!value.contains(capfile_str.as_ref()));
}

#[test]
fn browser_override_is_after_extra_argv() {
    let spec = BrowserChannelSpec::new(
        std::path::PathBuf::from("/tmp/browser-cap.json"),
        std::path::PathBuf::from("/usr/local/bin/tidebreak"),
    );
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &["--extra".into(), "--flag".into()],
        std::path::Path::new("/workspace"),
        &[],
        Some(&spec),
        None,
    )
    .unwrap();
    let browser_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
    let extra_flag_idx = plan.argv.iter().position(|arg| arg == "--extra").unwrap();
    assert!(extra_flag_idx < browser_idx);
}

#[test]
fn browser_capfile_path_is_never_in_argv() {
    let capfile = std::path::PathBuf::from("/tmp/tidebreak-browser-abc123.json");
    let spec = BrowserChannelSpec::new(
        capfile.clone(),
        std::path::PathBuf::from("/usr/local/bin/tidebreak"),
    );
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        Some(&spec),
        None,
    )
    .unwrap();
    let capfile_str = capfile.to_string_lossy();
    assert!(!plan
        .argv
        .iter()
        .any(|arg| arg.contains(capfile_str.as_ref())));
}

#[test]
fn browser_env_key_is_stripped_from_plan_even_when_browser_is_some() {
    let spec = BrowserChannelSpec::new(
        std::path::PathBuf::from("/tmp/browser-cap.json"),
        std::path::PathBuf::from("/usr/local/bin/tidebreak"),
    );
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[("TIDEBREAK_BROWSER_CAPFILE".into(), "/evil/cap.json".into())],
        Some(&spec),
        None,
    )
    .unwrap();
    let has_reserved = plan
        .env
        .iter()
        .any(|(key, _)| BrowserChannelSpec::is_reserved_env_key(key));
    assert!(!has_reserved);
}

#[test]
fn session_relay_key_survives_the_reserved_namespace_strip() {
    // Decision 71 hands the codex child its per-session relay key by env
    // (the provider config spawn_wiring emits reads it through
    // env_key={relay_key_env}); stripping it as a reserved key left
    // hosted codex sessions with no credential at all. Only the exact
    // wired name survives; the rest of the namespace stays reserved.
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[
            ("TIDEBREAK_LLM_KEY".into(), "tbreak_hl_test".into()),
            ("TIDEBREAK_BROWSER_CAPFILE".into(), "/evil/cap.json".into()),
        ],
        None,
        Some("TIDEBREAK_LLM_KEY"),
    )
    .unwrap();
    let relay = plan.env.iter().find(|(key, _)| key == "TIDEBREAK_LLM_KEY");
    assert_eq!(
        relay.map(|(_, value)| value.as_str()),
        Some("tbreak_hl_test")
    );
    assert!(!plan
        .env
        .iter()
        .any(|(key, _)| key == "TIDEBREAK_BROWSER_CAPFILE"));
}

#[test]
fn relay_key_is_stripped_when_no_relay_is_wired() {
    // Without a wired relay there is no exception: a settings value
    // squatting on a relay-shaped name is Tidebreak's namespace, not
    // the user's.
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[("TIDEBREAK_LLM_KEY".into(), "from-settings".into())],
        None,
        None,
    )
    .unwrap();
    assert!(plan.env.is_empty(), "{:?}", plan.env);
}

#[test]
fn bridge_command_with_spaces_remains_one_command_value() {
    let spec = BrowserChannelSpec::new(
        std::path::PathBuf::from("/tmp/browser-cap.json"),
        std::path::PathBuf::from("/Applications/Tidebreak.app/Contents/bin/tidebreak"),
    );
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        Some(&spec),
        None,
    )
    .unwrap();
    let override_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
    let value = &plan.argv[override_idx + 1];
    // The command must appear as one quoted value, not split on spaces.
    assert!(value.contains("command=\"/Applications/Tidebreak.app/Contents/bin/tidebreak\""));
    // args must still be exactly ["browser-mcp"].
    assert!(value.contains(r#"args=["browser-mcp"]"#));
}

#[test]
fn bridge_command_with_backslashes_is_escaped() {
    // A Windows path like C:\bin\tidebreak.exe must have its backslashes
    // JSON-escaped so the config string remains valid.
    let spec = BrowserChannelSpec::new(
        std::path::PathBuf::from("/tmp/browser-cap.json"),
        std::path::PathBuf::from(r"C:\Program Files\Tidebreak\tidebreak.exe"),
    );
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        Some(&spec),
        None,
    )
    .unwrap();
    let override_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
    let value = &plan.argv[override_idx + 1];
    // The backslashes must be escaped as \\, not left raw.
    assert!(value.contains("\\\\"));
    // The original \t in Tidebreak must not become a tab character.
    assert!(!value.contains('\t'));
    // The command must still parse as one value.
    assert!(value.contains("command=\""));
    assert!(value.contains(r#"args=["browser-mcp"]"#));
}

#[test]
fn bridge_command_with_embedded_quote_is_escaped() {
    // A path containing a double-quote (unlikely but defensive) must
    // escape it with a backslash so the config value remains valid.
    let spec = BrowserChannelSpec::new(
        std::path::PathBuf::from("/tmp/browser-cap.json"),
        std::path::PathBuf::from("/tmp/tidebreak-\"-binary"),
    );
    let plan = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        Some(&spec),
        None,
    )
    .unwrap();
    let override_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
    let value = &plan.argv[override_idx + 1];
    // The embedded " must be escaped as \", not raw.
    assert!(value.contains("\\\""));
    // The surrounding command="..." delimiter must still close properly.
    assert!(value.starts_with("mcp_servers.tb-browser="));
    assert!(value.contains("command=\""));
    assert!(value.ends_with("]}"));
}

#[cfg(unix)]
#[test]
fn non_utf8_bridge_command_is_rejected_instead_of_changed() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let spec = BrowserChannelSpec::new(
        std::path::PathBuf::from("/tmp/browser-cap.json"),
        std::path::PathBuf::from(OsString::from_vec(b"/tmp/tidebreak-\xff".to_vec())),
    );
    let error = compose_app_server_plan(
        std::path::Path::new("/usr/bin/codex"),
        &[],
        std::path::Path::new("/workspace"),
        &[],
        Some(&spec),
        None,
    )
    .expect_err("non-UTF-8 bridge paths must fail closed");

    assert!(error.to_string().contains("not valid UTF-8"));
}
