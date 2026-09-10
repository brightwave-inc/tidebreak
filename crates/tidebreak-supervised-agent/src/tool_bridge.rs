//! A private local helper transports native calls through the supervisor event stream.

#[cfg(unix)]
mod platform {
    use std::collections::{HashMap, VecDeque};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use crate::scratch::Scratch;
    use crate::wire::SupervisorToolRequest;
    use serde_json::Value;
    use tidebreak_core::code::supervisor_tools::{
        frame_request_id, is_result_frame, validate_request, ResultAssembler, MAX_OUTPUT_BYTES,
        MAX_REQUEST_BYTES,
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
            let listener = tokio::spawn(async move {
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        Some(_) = connections.join_next(), if !connections.is_empty() => (),
                        accepted = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                            let Ok((stream, _)) = accepted else { break; };
                            let sender = sender.clone();
                            connections.spawn(async move { serve(stream, sender).await; });
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
            })
        }

        /// Socket path supplied only to the local engine process.
        pub fn socket_path(&self) -> PathBuf {
            self.directory.path().join("call.sock")
        }

        /// Accepts bounded calls and emits each logical request once.
        pub fn drain_requests(&mut self) -> Vec<SupervisorToolRequest> {
            let mut requests = Vec::new();
            while let Ok(incoming) = self.incoming.try_recv() {
                let Incoming { request, reply } = incoming;
                if let Err(error) = validate_request(&request) {
                    let _ = reply.send(Err(error));
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
            let completed = match call.assembler.push(&id, message) {
                Ok(None) => return Ok(true),
                Ok(Some(result)) => self.scratch.materialize(&result.artifacts).map(|_| serde_json::json!({
                    "request_id": result.request_id,
                    "output": result.output,
                    "artifacts": result.artifacts.iter().map(|artifact| serde_json::json!({
                        "path": artifact.path, "media_type": artifact.media_type, "bytes": artifact.bytes.len(),
                    })).collect::<Vec<_>>(),
                })),
                Err(error) => Err(error),
            };
            for waiter in call.waiters.drain(..) {
                let _ = waiter.send(completed.clone());
            }
            call.reply = Some(completed.clone());
            call.assembler = ResultAssembler::default();
            self.completed.push_back(id);
            while self.completed.len() > MAX_COMPLETED {
                if let Some(id) = self.completed.pop_front() {
                    self.calls.remove(&id);
                }
            }
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

    async fn serve(mut stream: UnixStream, sender: mpsc::Sender<Incoming>) {
        let result = async {
            let bytes =
                tokio::time::timeout(READ_TIMEOUT, read_packet(&mut stream, MAX_REQUEST_BYTES))
                    .await
                    .map_err(|_| "native tool request timed out".to_owned())??;
            let request: SupervisorToolRequest =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            validate_request(&request)?;
            let (reply, received) = oneshot::channel();
            sender
                .try_send(Incoming { request, reply })
                .map_err(|_| "native tool request queue is full".to_owned())?;
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
        let bytes = tokio::time::timeout(
            CALL_TIMEOUT + READ_TIMEOUT,
            read_packet(&mut stream, MAX_REPLY_BYTES),
        )
        .await
        .map_err(|_| {
            "native call timed out; resume with the same request_id and arguments".to_owned()
        })??;
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
                request_id: "call-1".into(),
                tool: "conversation_export".into(),
                arguments: serde_json::json!({}),
            };
            let path = bridge.socket_path();
            let copy = request.clone();
            let helper = tokio::spawn(async move { call(&path, &copy).await });
            assert_eq!(drain_one(&mut bridge).await, vec![request.clone()]);
            let result = SupervisorToolResult {
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

        #[tokio::test]
        async fn refuses_changed_request_arguments_and_artifact_conflicts() {
            let root = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(root.path()).unwrap();
            let request = SupervisorToolRequest {
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
    pub struct LocalToolBridge;
    impl LocalToolBridge {
        pub fn start(_: &Path) -> Result<Self, String> {
            Err("native tool bridge requires a Unix socket".into())
        }
        pub fn socket_path(&self) -> PathBuf {
            PathBuf::new()
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
