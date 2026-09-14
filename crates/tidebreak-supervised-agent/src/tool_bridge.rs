//! A private local helper transports native calls through the supervisor event stream.

#[cfg(unix)]
mod platform {
    use std::collections::{HashMap, VecDeque};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tidebreak_core::code::SupervisorToolTurn;

    use crate::scratch::Scratch;
    use crate::wire::SupervisorToolRequest;
    use serde_json::Value;
    use tidebreak_core::code::supervisor_tools::{
        frame_request_id, human_decision_kind, is_human_decision, is_result_frame,
        validate_request, ResultAssembler, MAX_OUTPUT_BYTES, MAX_REQUEST_BYTES,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::{mpsc, oneshot};
    use tokio::task::{JoinHandle, JoinSet};

    const MAX_ACTIVE: usize = 8;
    const MAX_COMPLETED: usize = 128;
    const MAX_CONNECTIONS: usize = 16;
    const MAX_REPLY_BYTES: usize = MAX_OUTPUT_BYTES + MAX_REQUEST_BYTES;
    const READ_TIMEOUT: Duration = Duration::from_secs(10);
    const CALL_TIMEOUT: Duration = Duration::from_secs(180);
    type Reply = Result<Value, String>;

    struct Incoming {
        request: SupervisorToolRequest,
        reply: oneshot::Sender<Reply>,
    }

    struct Call {
        request: SupervisorToolRequest,
        assembler: ResultAssembler,
        waiters: Vec<oneshot::Sender<Reply>>,
        reply: Option<Reply>,
    }

    /// One private Unix socket and its bounded pending calls.
    pub struct LocalToolBridge {
        directory: tempfile::TempDir,
        listener: JoinHandle<()>,
        incoming: mpsc::Receiver<Incoming>,
        calls: HashMap<String, Call>,
        completed: VecDeque<String>,
        scratch: Scratch,
        active_turn: Arc<Mutex<Option<SupervisorToolTurn>>>,
        ready: VecDeque<Incoming>,
    }

    impl Drop for LocalToolBridge {
        fn drop(&mut self) {
            self.listener.abort();
        }
    }

    impl LocalToolBridge {
        /// Opens a local-only socket in a private directory and pins scratch.
        pub fn start(workdir: &Path) -> Result<Self, String> {
            let scratch = Scratch::open(workdir)?;
            let directory = tempfile::Builder::new()
                .prefix("tidebreak-tools-")
                .tempdir()
                .map_err(|error| error.to_string())?;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
            let path = directory.path().join("call.sock");
            let listener = UnixListener::bind(&path).map_err(|error| error.to_string())?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            let (sender, incoming) = mpsc::channel(MAX_CONNECTIONS);
            let active_turn = Arc::new(Mutex::new(None));
            let incoming_turn = active_turn.clone();
            let listener = tokio::spawn(async move {
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        Some(_) = connections.join_next(), if !connections.is_empty() => (),
                        accepted = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                            let Ok((stream, _)) = accepted else { break; };
                            let sender = sender.clone();
                            let active_turn = incoming_turn.clone();
                            connections.spawn(async move { serve(stream, sender, active_turn).await; });
                        }
                    }
                }
            });
            Ok(Self {
                directory,
                listener,
                incoming,
                calls: HashMap::new(),
                completed: VecDeque::new(),
                scratch,
                active_turn,
                ready: VecDeque::new(),
            })
        }

        /// Socket path supplied only to the local engine process.
        pub fn socket_path(&self) -> PathBuf {
            self.directory.path().join("call.sock")
        }

        /// Assign helper requests to the turn before the engine starts.
        pub fn begin_turn(&mut self, turn: SupervisorToolTurn) {
            *self.active_turn.lock().expect("native helper turn") = Some(turn);
        }

        /// Stop all human waiters before another turn can receive a request.
        pub fn end_turn(&mut self) {
            *self.active_turn.lock().expect("native helper turn") = None;
            while let Ok(incoming) = self.incoming.try_recv() {
                if is_human_decision(&incoming.request.tool) {
                    let _ = incoming.reply.send(Err(
                        "the owning turn ended before the native call completed".into(),
                    ));
                } else {
                    self.ready.push_back(incoming);
                }
            }
            for (id, call) in &mut self.calls {
                if is_human_decision(&call.request.tool) && call.reply.is_none() {
                    let result =
                        Err("the owning turn ended before the human decision completed".into());
                    for waiter in call.waiters.drain(..) {
                        let _ = waiter.send(result.clone());
                    }
                    call.reply = Some(result);
                    self.completed.push_back(id.clone());
                }
            }
            self.prune_completed();
        }

        fn prune_completed(&mut self) {
            while self.completed.len() > MAX_COMPLETED {
                if let Some(id) = self.completed.pop_front() {
                    self.calls.remove(&id);
                }
            }
        }

        /// Whether a human helper still holds this running turn.
        pub fn waiting_for_human(&self) -> bool {
            self.calls
                .values()
                .any(|call| is_human_decision(&call.request.tool) && call.reply.is_none())
        }

        /// Accepts bounded calls and emits each logical request once.
        pub fn drain_requests(&mut self) -> Vec<SupervisorToolRequest> {
            let mut requests = Vec::new();
            while let Some(incoming) = self
                .ready
                .pop_front()
                .or_else(|| self.incoming.try_recv().ok())
            {
                let Incoming { request, reply } = incoming;
                if let Err(error) = validate_request(&request) {
                    let _ = reply.send(Err(error));
                    continue;
                }
                if request.cancelled {
                    let mut original = request.clone();
                    original.cancelled = false;
                    if let Some(call) = self.calls.get_mut(&request.request_id) {
                        call.waiters.retain(|waiter| !waiter.is_closed());
                        if call.request == original
                            && call.reply.is_none()
                            && call.waiters.is_empty()
                        {
                            call.reply = Some(Err("the human request was cancelled".into()));
                            self.completed.push_back(request.request_id.clone());
                            requests.push(request);
                        }
                    }
                    self.prune_completed();
                    let _ = reply.send(Ok(serde_json::json!({"cancelled":true})));
                    continue;
                }
                if is_human_decision(&request.tool)
                    && (request.turn.is_none()
                        || request.turn != *self.active_turn.lock().expect("native helper turn"))
                {
                    let _ =
                        reply.send(Err("the human decision belongs to an inactive turn".into()));
                    continue;
                }
                if let Some(call) = self.calls.get_mut(&request.request_id) {
                    if call.request != request {
                        let _ = reply.send(Err(
                            "request_id already belongs to different arguments".into(),
                        ));
                    } else if let Some(result) = &call.reply {
                        let _ = reply.send(result.clone());
                    } else {
                        call.waiters.retain(|waiter| !waiter.is_closed());
                        if call.waiters.len() >= MAX_CONNECTIONS {
                            let _ = reply
                                .send(Err("too many callers are waiting for this request".into()));
                        } else {
                            call.waiters.push(reply);
                        }
                    }
                    continue;
                }
                if self
                    .calls
                    .values()
                    .filter(|call| call.reply.is_none())
                    .count()
                    >= MAX_ACTIVE
                {
                    let _ = reply.send(Err(
                        "too many native calls are pending; retry after a call finishes".into(),
                    ));
                    continue;
                }
                requests.push(request.clone());
                self.calls.insert(
                    request.request_id.clone(),
                    Call {
                        request,
                        assembler: ResultAssembler::default(),
                        waiters: vec![reply],
                        reply: None,
                    },
                );
            }
            requests
        }

        /// Completes only a matching local call, after artifacts reach scratch.
        pub fn receive_frame(&mut self, message: &str) -> Result<bool, String> {
            if !is_result_frame(message) {
                return Ok(false);
            }
            let id = frame_request_id(message).ok_or("malformed native tool result frame")?;
            let call = self
                .calls
                .get_mut(&id)
                .ok_or("native tool result has no local request")?;
            if call.reply.is_some() {
                return Ok(true);
            }
            let result = match call.assembler.push(&id, message) {
                Ok(None) => return Ok(true),
                Ok(Some(result)) => result,
                Err(error) => {
                    call.assembler = ResultAssembler::default();
                    return Err(error);
                }
            };
            if is_human_decision(&call.request.tool)
                && result.request.as_ref() != Some(&call.request)
            {
                // A delayed rejection or answer cannot release another proposal's waiter.
                call.assembler = ResultAssembler::default();
                return Ok(true);
            }
            let completed = self.scratch.materialize(&result.artifacts).map(|_| serde_json::json!({
                "request_id": result.request_id,
                "output": result.output,
                "artifacts": result.artifacts.iter().map(|artifact| serde_json::json!({
                    "path": artifact.path, "media_type": artifact.media_type, "bytes": artifact.bytes.len(),
                })).collect::<Vec<_>>(),
            }));
            for waiter in call.waiters.drain(..) {
                let _ = waiter.send(completed.clone());
            }
            call.reply = Some(completed.clone());
            call.assembler = ResultAssembler::default();
            self.completed.push_back(id);
            self.prune_completed();
            completed.map(|_| true)
        }
    }

    async fn read_packet(stream: &mut UnixStream, maximum: usize) -> Result<Vec<u8>, String> {
        let size = stream.read_u32().await.map_err(|error| error.to_string())? as usize;
        if size == 0 || size > maximum {
            return Err("native tool packet exceeds its byte limit".into());
        }
        let mut bytes = vec![0; size];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    async fn write_packet(stream: &mut UnixStream, value: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_REPLY_BYTES {
            return Err("native tool reply exceeds its byte limit".into());
        }
        stream
            .write_u32(bytes.len() as u32)
            .await
            .map_err(|error| error.to_string())?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|error| error.to_string())
    }

    async fn serve(
        mut stream: UnixStream,
        sender: mpsc::Sender<Incoming>,
        active_turn: Arc<Mutex<Option<SupervisorToolTurn>>>,
    ) {
        let result = async {
            let bytes =
                tokio::time::timeout(READ_TIMEOUT, read_packet(&mut stream, MAX_REQUEST_BYTES))
                    .await
                    .map_err(|_| "native tool request timed out".to_owned())??;
            let mut request: SupervisorToolRequest =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            validate_request(&request)?;
            if request.turn.is_some() || request.cancelled {
                return Err("only the supervisor may assign or cancel a native turn".into());
            }
            let human = is_human_decision(&request.tool);
            if human {
                human_decision_kind(&request.tool, &request.arguments)?;
                request.turn = active_turn.lock().expect("native helper turn").clone();
                if request.turn.is_none() {
                    return Err("human decisions require a running turn".into());
                }
            }
            let cancellation = request.clone();
            let (reply, mut received) = oneshot::channel();
            sender
                .try_send(Incoming { request, reply })
                .map_err(|_| "native tool request queue is full".to_owned())?;
            if human {
                let answer = tokio::select! {
                    result = &mut received => Some(result),
                    _ = stream.read_u8() => None,
                };
                if let Some(answer) = answer {
                    return answer.map_err(|_| "native tool bridge stopped".to_owned())?;
                }
                drop(received);
                let mut request = cancellation;
                request.cancelled = true;
                let (reply, _) = oneshot::channel();
                // Preserve the original turn and allow another caller of this request to wait.
                let _ = sender.send(Incoming { request, reply }).await;
                return Err("the human request disconnected".into());
            }
            tokio::time::timeout(CALL_TIMEOUT, received)
                .await
                .map_err(|_| {
                    "native call is still pending; resume with the same request_id and arguments"
                        .to_owned()
                })?
                .map_err(|_| "native tool bridge stopped".to_owned())?
        }
        .await;
        let response = match result {
            Ok(value) => value,
            Err(error) => serde_json::json!({"error": error}),
        };
        let _ = tokio::time::timeout(READ_TIMEOUT, write_packet(&mut stream, &response)).await;
    }

    /// Calls the local bridge without any host endpoint or credential.
    pub async fn call(path: &Path, request: &SupervisorToolRequest) -> Result<Value, String> {
        validate_request(request)?;
        if is_human_decision(&request.tool) {
            human_decision_kind(&request.tool, &request.arguments)?;
        }
        let mut stream = tokio::time::timeout(READ_TIMEOUT, UnixStream::connect(path))
            .await
            .map_err(|_| "native tool bridge connection timed out".to_owned())?
            .map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        tokio::time::timeout(READ_TIMEOUT, async {
            stream.write_u32(bytes.len() as u32).await?;
            stream.write_all(&bytes).await
        })
        .await
        .map_err(|_| "native tool bridge write timed out".to_owned())?
        .map_err(|error| error.to_string())?;
        let bytes = if is_human_decision(&request.tool) {
            read_packet(&mut stream, MAX_REPLY_BYTES).await?
        } else {
            tokio::time::timeout(
                CALL_TIMEOUT + READ_TIMEOUT,
                read_packet(&mut stream, MAX_REPLY_BYTES),
            )
            .await
            .map_err(|_| {
                "native call timed out; resume with the same request_id and arguments".to_owned()
            })??
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(error.into());
        }
        Ok(value)
    }

    /// Reads one bounded JSON call from standard input and prints the complete result.
    pub async fn run_cli() -> Result<(), String> {
        let path = std::env::var_os("TIDEBREAK_TOOL_SOCKET")
            .ok_or("native tools are unavailable in this process")?;
        let mut bytes = Vec::new();
        tokio::time::timeout(
            READ_TIMEOUT,
            tokio::io::stdin()
                .take(MAX_REQUEST_BYTES as u64 + 1)
                .read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| "native tool standard input timed out".to_owned())?
        .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("native tool request exceeds the byte limit".into());
        }
        let request = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let result = call(Path::new(&path), &request).await?;
        println!(
            "{}",
            serde_json::to_string(&result).map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tidebreak_core::code::supervisor_tools::{
            encode_result_frames, SupervisorArtifact, SupervisorToolResult,
        };

        async fn drain_one(bridge: &mut LocalToolBridge) -> Vec<SupervisorToolRequest> {
            for _ in 0..100 {
                let requests = bridge.drain_requests();
                if !requests.is_empty() {
                    return requests;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            panic!("helper request did not arrive");
        }

        #[tokio::test]
        async fn helper_receives_output_and_materialized_artifact_and_retries_once() {
            let root = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(root.path()).unwrap();
            let request = SupervisorToolRequest {
                cancelled: false,
                turn: None,
                request_id: "call-1".into(),
                tool: "conversation_export".into(),
                arguments: serde_json::json!({}),
            };
            let path = bridge.socket_path();
            let copy = request.clone();
            let helper = tokio::spawn(async move { call(&path, &copy).await });
            assert_eq!(drain_one(&mut bridge).await, vec![request.clone()]);
            let result = SupervisorToolResult {
                request: None,
                request_id: request.request_id.clone(),
                output: serde_json::json!({"content":"ready","data":{"path":"conversation/thread.txt"}}),
                artifacts: vec![SupervisorArtifact {
                    path: "conversation/thread.txt".into(),
                    media_type: "text/plain".into(),
                    bytes: b"thread text".to_vec(),
                }],
            };
            let frames = encode_result_frames(&result).unwrap();
            for frame in &frames {
                assert!(bridge.receive_frame(frame).unwrap());
            }
            let response = helper.await.unwrap().unwrap();
            assert_eq!(response["output"], result.output);
            assert_eq!(
                std::fs::read(root.path().join("conversation/thread.txt")).unwrap(),
                b"thread text"
            );
            let path = bridge.socket_path();
            let retry = tokio::spawn(async move { call(&path, &request).await });
            for _ in 0..100 {
                assert!(bridge.drain_requests().is_empty());
                if retry.is_finished() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert_eq!(retry.await.unwrap().unwrap(), response);
        }

        fn human_request() -> SupervisorToolRequest {
            SupervisorToolRequest {
                cancelled: false,
                turn: None,
                request_id: "human".into(),
                tool: "ask_user_questions".into(),
                arguments: serde_json::json!({"questions":[{"id":"target","header":"Target","question":"Which target?","allow_free_form":true}]}),
            }
        }

        #[tokio::test]
        async fn repeated_cancellations_keep_completed_history_bounded_without_ending_the_turn() {
            let root = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(root.path()).unwrap();
            let turn = SupervisorToolTurn {
                native_turn: 1,
                runtime_id: uuid::Uuid::new_v4(),
            };
            bridge.begin_turn(turn.clone());
            for index in 0..MAX_COMPLETED + 2 {
                let mut request = human_request();
                request.request_id = format!("human-{index}");
                request.turn = Some(turn.clone());
                let (reply, receiver) = oneshot::channel();
                bridge.ready.push_back(Incoming {
                    request: request.clone(),
                    reply,
                });
                assert_eq!(bridge.drain_requests(), vec![request.clone()]);
                drop(receiver);
                request.cancelled = true;
                let (reply, _) = oneshot::channel();
                bridge.ready.push_back(Incoming {
                    request: request.clone(),
                    reply,
                });
                assert_eq!(bridge.drain_requests(), vec![request]);
                assert!(bridge.calls.len() <= MAX_COMPLETED);
                assert!(bridge.completed.len() <= MAX_COMPLETED);
            }
            assert_eq!(bridge.calls.len(), MAX_COMPLETED);
            assert!(!bridge.calls.contains_key("human-0"));
            assert!(!bridge.waiting_for_human());
        }

        #[tokio::test]
        async fn turn_end_preserves_ordinary_incoming_requests_and_stops_human_requests() {
            let root = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(root.path()).unwrap();
            bridge.begin_turn(SupervisorToolTurn {
                native_turn: 1,
                runtime_id: uuid::Uuid::new_v4(),
            });
            let human = human_request();
            let mut ordinary = human.clone();
            ordinary.request_id = "ordinary".into();
            ordinary.tool = "conversation_export".into();
            ordinary.arguments = serde_json::json!({});
            let socket = bridge.socket_path();
            let human = tokio::spawn(async move { call(&socket, &human).await });
            let socket = bridge.socket_path();
            let copy = ordinary.clone();
            let ordinary_call = tokio::spawn(async move { call(&socket, &copy).await });
            tokio::time::timeout(Duration::from_secs(2), async {
                while bridge.incoming.len() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            bridge.end_turn();
            assert!(human
                .await
                .unwrap()
                .unwrap_err()
                .contains("owning turn ended"));
            assert_eq!(bridge.drain_requests(), vec![ordinary.clone()]);
            assert!(!ordinary_call.is_finished());
            for frame in encode_result_frames(&SupervisorToolResult::failed(
                ordinary.request_id.clone(),
                "ordinary finished",
            ))
            .unwrap()
            {
                bridge.receive_frame(&frame).unwrap();
            }
            assert!(ordinary_call.await.unwrap().is_ok());
        }

        #[tokio::test]
        async fn disconnect_cancels_only_after_every_retry_caller_disconnects() {
            let root = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(root.path()).unwrap();
            bridge.begin_turn(SupervisorToolTurn {
                native_turn: 1,
                runtime_id: uuid::Uuid::new_v4(),
            });
            let request = human_request();
            let socket = bridge.socket_path();
            let first_request = request.clone();
            let first = tokio::spawn(async move { call(&socket, &first_request).await });
            let original = drain_one(&mut bridge).await.remove(0);
            let socket = bridge.socket_path();
            let second = tokio::spawn(async move { call(&socket, &request).await });
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    assert!(bridge.drain_requests().is_empty());
                    if bridge.calls["human"].waiters.len() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            first.abort();
            let _ = first.await;
            tokio::time::timeout(Duration::from_secs(2), async {
                while bridge.incoming.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(bridge.drain_requests().is_empty());
            assert!(bridge.waiting_for_human());
            second.abort();
            let _ = second.await;
            let cancelled = drain_one(&mut bridge).await.remove(0);
            assert!(cancelled.cancelled);
            assert_eq!(cancelled.turn, original.turn);
            assert!(!bridge.waiting_for_human());
        }

        #[tokio::test]
        async fn refuses_changed_request_arguments_and_artifact_conflicts() {
            let root = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(root.path()).unwrap();
            let request = SupervisorToolRequest {
                cancelled: false,
                turn: None,
                request_id: "call-1".into(),
                tool: "conversation_export".into(),
                arguments: serde_json::json!({}),
            };
            let path = bridge.socket_path();
            let copy = request.clone();
            let first = tokio::spawn(async move { call(&path, &copy).await });
            drain_one(&mut bridge).await;
            let mut changed = request.clone();
            changed.arguments = serde_json::json!({"different":true});
            let path = bridge.socket_path();
            let second = tokio::spawn(async move { call(&path, &changed).await });
            for _ in 0..100 {
                assert!(bridge.drain_requests().is_empty());
                if second.is_finished() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert!(second
                .await
                .unwrap()
                .unwrap_err()
                .contains("different arguments"));
            std::fs::create_dir(root.path().join("conversation")).unwrap();
            std::fs::write(root.path().join("conversation/file"), b"original").unwrap();
            let result = SupervisorToolResult {
                request: None,
                request_id: request.request_id,
                output: serde_json::json!({}),
                artifacts: vec![SupervisorArtifact {
                    path: "conversation/file".into(),
                    media_type: "text/plain".into(),
                    bytes: b"replacement".to_vec(),
                }],
            };
            assert!(bridge
                .receive_frame(&encode_result_frames(&result).unwrap()[0])
                .is_err());
            assert!(first.await.unwrap().is_err());
            assert_eq!(
                std::fs::read(root.path().join("conversation/file")).unwrap(),
                b"original"
            );
        }
    }
}

#[cfg(unix)]
pub use platform::*;

#[cfg(not(unix))]
mod platform {
    use crate::wire::SupervisorToolRequest;
    use std::path::{Path, PathBuf};
    use tidebreak_core::code::SupervisorToolTurn;
    pub struct LocalToolBridge;
    impl LocalToolBridge {
        pub fn start(_: &Path) -> Result<Self, String> {
            Err("native tool bridge requires a Unix socket".into())
        }
        pub fn socket_path(&self) -> PathBuf {
            PathBuf::new()
        }
        pub fn begin_turn(&mut self, _: SupervisorToolTurn) {}
        pub fn end_turn(&mut self) {}
        pub fn waiting_for_human(&self) -> bool {
            false
        }
        pub fn drain_requests(&mut self) -> Vec<SupervisorToolRequest> {
            Vec::new()
        }
        pub fn receive_frame(&mut self, _: &str) -> Result<bool, String> {
            Err("native tool bridge requires a Unix socket".into())
        }
    }
    pub async fn run_cli() -> Result<(), String> {
        Err("native tool bridge requires a Unix socket".into())
    }
}
#[cfg(not(unix))]
pub use platform::*;
