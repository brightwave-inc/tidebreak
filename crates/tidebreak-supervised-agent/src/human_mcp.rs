//! Native MCP tools hold human decisions without shell timeouts or backgrounding.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tidebreak_core::code::supervisor_tools::{
    human_decision_kind, human_decision_spec, MAX_REQUEST_BYTES,
};
use tidebreak_core::code::SupervisorToolRequest;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::task::{AbortHandle, JoinSet};

const MAX_CALLS: usize = 8;
const MAX_IDS: usize = 128;
const GUIDANCE: &str = "Use ask_user_questions for structured choices and request_plan_approval for a concrete plan. These MCP calls wait for an explicit human decision. Do not invoke them through a shell or in the background. Do not treat an error or a missing response as consent. Plan acceptance preserves the session's permission mode.";

async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>, String> {
    loop {
        let bytes = reader.fill_buf().await.map_err(|error| error.to_string())?;
        if bytes.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err("MCP input ended inside a message".into())
            };
        }
        let end = bytes.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let count = end.unwrap_or(bytes.len());
        if line.len() + count > MAX_REQUEST_BYTES + 4096 {
            return Err("MCP request exceeds its byte limit".into());
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(std::mem::take(line)));
        }
    }
}

async fn write<W: AsyncWrite + Unpin>(writer: &Arc<Mutex<W>>, value: Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let mut writer = writer.lock().await;
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    writer.flush().await.map_err(|error| error.to_string())
}

fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}

struct McpCall {
    request: SupervisorToolRequest,
    tasks: Vec<AbortHandle>,
}

/// Serve the same protocol that the pinned native harnesses use over stdio.
#[cfg(unix)]
pub async fn serve<R, W>(reader: R, writer: W, socket: PathBuf) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    serve_with_limit(reader, writer, socket, MAX_IDS).await
}

#[cfg(unix)]
async fn serve_with_limit<R, W>(
    mut reader: R,
    writer: W,
    socket: PathBuf,
    history_limit: usize,
) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let writer = Arc::new(Mutex::new(writer));
    let mut pending = JoinSet::new();
    let mut calls: HashMap<String, McpCall> = HashMap::new();
    let mut input = Vec::new();
    loop {
        let line = tokio::select! {
            done = pending.join_next(), if !pending.is_empty() => {
                match done.ok_or("MCP task disappeared")? {
                    Ok(result) => result?,
                    Err(error) if error.is_cancelled() => (),
                    Err(error) => return Err(error.to_string()),
                }
                continue;
            }
            line = read_line(&mut reader, &mut input) => line?,
        };
        let Some(line) = line else {
            return Ok(());
        };
        let request: Value = serde_json::from_slice(&line).map_err(|error| error.to_string())?;
        let Some(id) = request.get("id").cloned() else {
            if request["method"] == "notifications/cancelled" {
                let key = request["params"]["requestId"].to_string();
                if let Some(call) = calls.get_mut(&key) {
                    call.request.cancelled = true;
                    for task in &call.tasks {
                        task.abort();
                    }
                }
            }
            continue;
        };
        if !id.is_string() && !id.is_number() {
            return Err("MCP request id must be a string or number".into());
        }
        let result = match request.get("method").and_then(Value::as_str) {
            Some("initialize") => json!({"protocolVersion":request["params"]["protocolVersion"],
                "capabilities":{"tools":{}},"serverInfo":{"name":"tb-human","version":"1.0.0"},"instructions":GUIDANCE}),
            Some("ping") => json!({}),
            Some("tools/list") => {
                let mut tools: Vec<Value> = ["ask_user_questions", "request_plan_approval"].into_iter().map(|name| {
                    let spec = human_decision_spec(name).expect("fixed human tool");
                    json!({"name":spec.name,"description":spec.description,"inputSchema":spec.input_schema,
                        "annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
                }).collect();
                tools.push(json!({"name":"permission_prompt","description":"Wait for an explicit decision on the native tool request.",
                    "inputSchema":{"type":"object","properties":{"tool_name":{"type":"string"},"input":{"type":"object"},"tool_use_id":{"type":"string"}},"required":["tool_name","input","tool_use_id"]},
                    "annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}));
                json!({"tools":tools})
            }
            Some("tools/call") => {
                let params = &request["params"];
                let name = params["name"].as_str().unwrap_or_default();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let native_permission = name == "permission_prompt";
                let (name, args) = if native_permission {
                    let valid = serde_json::from_value::<
                        tidebreak_harness::claude::approvals::PermissionPromptRequest,
                    >(args.clone())
                    .is_ok_and(|request| {
                        !request.tool_use_id.is_empty() && !request.tool_name.is_empty()
                    });
                    if !valid {
                        write(&writer, json!({"jsonrpc":"2.0","id":id,"result":tool_error("invalid native permission request")})).await?;
                        continue;
                    }
                    ("request_tool_approval", json!({"raw":args}))
                } else {
                    (name, args)
                };
                if !native_permission
                    && !matches!(name, "ask_user_questions" | "request_plan_approval")
                {
                    tool_error("this human helper is unavailable")
                } else if let Err(error) = human_decision_kind(name, &args) {
                    tool_error(&error)
                } else if pending.len() >= MAX_CALLS {
                    tool_error("too many human calls are pending")
                } else {
                    let key = id.to_string();
                    let call = if let Some(prior) = calls.get(&key) {
                        if prior.request.cancelled
                            || prior.request.tool != name
                            || prior.request.arguments != args
                        {
                            write(&writer, json!({"jsonrpc":"2.0","id":id,"result":tool_error("MCP request id changed its arguments")})).await?;
                            continue;
                        }
                        prior.request.clone()
                    } else {
                        if calls.len() >= history_limit {
                            calls.retain(|_, call| {
                                call.tasks.iter().any(|task| !task.is_finished())
                            });
                            if calls.len() >= history_limit {
                                write(&writer, json!({"jsonrpc":"2.0","id":id,"result":tool_error("too many human calls are pending")})).await?;
                                continue;
                            }
                        }
                        let call = SupervisorToolRequest {
                            cancelled: false,
                            request_id: format!("human-{}", uuid::Uuid::new_v4()),
                            tool: name.into(),
                            arguments: args,
                            turn: None,
                        };
                        calls.insert(
                            key.clone(),
                            McpCall {
                                request: call.clone(),
                                tasks: Vec::new(),
                            },
                        );
                        call
                    };
                    let writer = writer.clone();
                    let socket = socket.clone();
                    let task = pending.spawn(async move {
                        let reply = crate::tool_bridge::call(&socket, &call).await;
                        let result = if native_permission {
                            let decision = match reply {
                                Ok(result) => crate::native_approvals::decision_from_output(&result["output"]),
                                Err(error) => tidebreak_harness::ApprovalDecision::Deny { feedback: Some(error) },
                            };
                            let response = tidebreak_harness::claude::approvals::PermissionPromptResponse::from_decision(&decision);
                            json!({"content":[{"type":"text","text":response.as_text_block()}],"isError":false})
                        } else {
                            match reply {
                                Ok(result) => json!({"content":[{"type":"text","text":result["output"].to_string()}],"isError":result["output"]["is_error"].as_bool().unwrap_or(false)}),
                                Err(error) => tool_error(&error),
                            }
                        };
                        write(&writer, json!({"jsonrpc":"2.0","id":id,"result":result})).await
                    });
                    calls
                        .get_mut(&key)
                        .expect("registered human call")
                        .tasks
                        .push(task);
                    continue;
                }
            }
            _ => {
                write(&writer, json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})).await?;
                continue;
            }
        };
        write(&writer, json!({"jsonrpc":"2.0","id":id,"result":result})).await?;
    }
}

#[cfg(unix)]
pub async fn run() -> Result<(), String> {
    let socket = std::env::var_os("TIDEBREAK_TOOL_SOCKET")
        .ok_or("human tools require a managed supervisor socket")?;
    serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        Path::new(&socket).to_path_buf(),
    )
    .await
}

#[cfg(not(unix))]
pub async fn run() -> Result<(), String> {
    Err("human tools require a Unix socket".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tool_bridge::LocalToolBridge;
    use tidebreak_core::code::supervisor_tools::{encode_result_frames, SupervisorToolResult};
    use tidebreak_core::code::SupervisorToolTurn;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    async fn send(writer: &mut (impl AsyncWrite + Unpin), request: Value) {
        writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn receive(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn arguments() -> Value {
        json!({"questions":[{"id":"target","header":"Target","question":"Which target?","allow_free_form":true}]})
    }

    #[tokio::test]
    async fn native_permission_mcp_waits_and_returns_the_native_allow_or_deny_shape() {
        for approve in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
            bridge.begin_turn(SupervisorToolTurn {
                native_turn: 1,
                runtime_id: uuid::Uuid::new_v4(),
            });
            let (client, server) = tokio::io::duplex(100_000);
            let (server_read, server_write) = tokio::io::split(server);
            let (client_read, mut client_write) = tokio::io::split(client);
            let mut client_read = BufReader::new(client_read);
            let server = tokio::spawn(serve(
                BufReader::new(server_read),
                server_write,
                bridge.socket_path(),
            ));
            send(&mut client_write, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"permission_prompt","arguments":{"tool_name":"Bash","tool_use_id":"tool-1","input":{"command":"printf marker"}}}})).await;
            let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let Some(request) = bridge.drain_requests().pop() {
                        break request;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(request.tool, "request_tool_approval");
            assert_eq!(request.arguments["raw"]["tool_use_id"], "tool-1");
            tokio::time::pause();
            tokio::time::advance(std::time::Duration::from_secs(901)).await;
            assert!(bridge.waiting_for_human());
            send(
                &mut client_write,
                json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
            )
            .await;
            assert_eq!(receive(&mut client_read).await["id"], 2);
            let result = SupervisorToolResult {
                request: Some(request.clone()),
                request_id: request.request_id.clone(),
                output: json!({"is_error":false,"data":{"decision":if approve { "approved" } else { "rejected" },"feedback":"Skip it"}}),
                artifacts: vec![],
            };
            for frame in encode_result_frames(&result).unwrap() {
                bridge.receive_frame(&frame).unwrap();
            }
            let response = receive(&mut client_read).await;
            assert_eq!(response["id"], 1);
            assert_eq!(response["result"]["isError"], false);
            let body: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(body["behavior"], if approve { "allow" } else { "deny" });
            if !approve {
                assert_eq!(body["message"], "Skip it");
            }
            tokio::time::resume();
            server.abort();
        }
    }

    #[tokio::test]
    async fn human_mcp_waits_past_tool_timeouts_and_keeps_ping_and_exact_result_binding() {
        let dir = tempfile::tempdir().unwrap();
        let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
        bridge.begin_turn(SupervisorToolTurn {
            native_turn: 1,
            runtime_id: uuid::Uuid::new_v4(),
        });
        let (client, server) = tokio::io::duplex(100_000);
        let (server_read, server_write) = tokio::io::split(server);
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);
        let server = tokio::spawn(serve(
            BufReader::new(server_read),
            server_write,
            bridge.socket_path(),
        ));
        send(&mut client_write, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}})).await;
        assert_eq!(
            receive(&mut client_read).await["result"]["capabilities"],
            json!({"tools":{}})
        );
        send(
            &mut client_write,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        let catalog = receive(&mut client_read).await;
        assert_eq!(catalog["result"]["tools"].as_array().unwrap().len(), 3);
        assert!(catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == false));
        send(&mut client_write, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ask_user_questions","arguments":arguments()}})).await;
        let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(request) = bridge.drain_requests().into_iter().next() {
                    break request;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(901)).await;
        assert!(bridge.waiting_for_human());
        send(
            &mut client_write,
            json!({"jsonrpc":"2.0","id":4,"method":"ping"}),
        )
        .await;
        assert_eq!(receive(&mut client_read).await["id"], 4);
        let mut stale = request.clone();
        stale.turn.as_mut().unwrap().native_turn += 1;
        for frame in encode_result_frames(&SupervisorToolResult::failed_request(
            &stale,
            "stale request",
        ))
        .unwrap()
        {
            bridge.receive_frame(&frame).unwrap();
        }
        assert!(bridge.waiting_for_human());
        let mut result = SupervisorToolResult::failed_request(&request, "test answer");
        result.output = json!({"is_error":false,"data":{"decision":"answered","answers":[{"question_id":"target","custom_answer":"test"}]}});
        for frame in encode_result_frames(&result).unwrap() {
            bridge.receive_frame(&frame).unwrap();
        }
        let answered = receive(&mut client_read).await;
        assert_eq!(answered["id"], 3);
        assert_eq!(answered["result"]["isError"], false);
        assert!(answered["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("answered"));
        assert!(!bridge.waiting_for_human());
        server.abort();
    }

    #[tokio::test]
    async fn completed_calls_do_not_discard_a_partially_received_mcp_message() {
        let (mut client, server) = tokio::io::duplex(100);
        let mut reader = BufReader::new(server);
        let mut input = Vec::new();
        client
            .write_all(br#"{"jsonrpc":"2.0","id":4,"#)
            .await
            .unwrap();
        tokio::select! {
            result = read_line(&mut reader, &mut input) => panic!("partial message: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_millis(1)) => (),
        }
        assert!(!input.is_empty());
        client.write_all(b"\"method\":\"ping\"}\n").await.unwrap();
        let message = read_line(&mut reader, &mut input).await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&message).unwrap()["method"],
            "ping"
        );
        assert!(input.is_empty());
    }

    #[tokio::test]
    async fn pinned_harness_mcp_requests_and_answers_replay_through_the_helper() {
        for fixture in [
            include_str!("../../tidebreak-harness/fixtures/codex/0.153.4/managed-human-mcp.ndjson"),
            include_str!(
                "../../tidebreak-harness/fixtures/claude-code/2.1.259/managed-human-mcp.ndjson"
            ),
        ] {
            let rows: Vec<Value> = fixture
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let dir = tempfile::tempdir().unwrap();
            let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
            bridge.begin_turn(SupervisorToolTurn {
                native_turn: 1,
                runtime_id: uuid::Uuid::new_v4(),
            });
            let (client, server) = tokio::io::duplex(100_000);
            let (read, write) = tokio::io::split(server);
            let (client_read, mut client_write) = tokio::io::split(client);
            let mut client_read = BufReader::new(client_read);
            let server = tokio::spawn(serve(BufReader::new(read), write, bridge.socket_path()));
            for row in rows.iter().filter(|row| row["dir"] == "in") {
                let request = &row["msg"];
                send(&mut client_write, request.clone()).await;
                if request.get("id").is_none() {
                    continue;
                }
                if request["method"] == "tools/call" {
                    let admitted = next_request(&mut bridge).await;
                    assert_eq!(admitted.arguments, request["params"]["arguments"]);
                    let captured = rows
                        .iter()
                        .find(|row| row["dir"] == "out" && row["msg"]["id"] == request["id"])
                        .unwrap();
                    let output: Value = serde_json::from_str(
                        captured["msg"]["result"]["content"][0]["text"]
                            .as_str()
                            .unwrap(),
                    )
                    .unwrap();
                    let mut result = SupervisorToolResult::failed_request(&admitted, "fixture");
                    result.output = output;
                    for frame in encode_result_frames(&result).unwrap() {
                        bridge.receive_frame(&frame).unwrap();
                    }
                    assert_eq!(
                        receive(&mut client_read).await["result"],
                        captured["msg"]["result"]
                    );
                } else {
                    let response = receive(&mut client_read).await;
                    assert_eq!(response["id"], request["id"]);
                    assert!(response.get("error").is_none());
                }
            }
            assert!(!bridge.waiting_for_human());
            server.abort();
        }
    }

    async fn next_request(bridge: &mut LocalToolBridge) -> SupervisorToolRequest {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(request) = bridge.drain_requests().into_iter().next() {
                    break request;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn human_mcp_cancels_only_the_named_call_and_eof_cancels_remaining_waits() {
        let dir = tempfile::tempdir().unwrap();
        let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
        bridge.begin_turn(SupervisorToolTurn {
            native_turn: 1,
            runtime_id: uuid::Uuid::new_v4(),
        });
        let (client, server) = tokio::io::duplex(100_000);
        let (read, write) = tokio::io::split(server);
        let (read_client, mut write_client) = tokio::io::split(client);
        let mut read_client = BufReader::new(read_client);
        let server = tokio::spawn(serve(BufReader::new(read), write, bridge.socket_path()));
        send(&mut write_client, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask_user_questions","arguments":arguments()}})).await;
        let first = next_request(&mut bridge).await;
        send(
            &mut write_client,
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
        )
        .await;
        let cancellation = next_request(&mut bridge).await;
        assert!(cancellation.cancelled);
        assert_eq!(cancellation.turn, first.turn);
        assert_eq!(cancellation.request_id, first.request_id);
        assert_eq!(cancellation.arguments, first.arguments);
        assert!(!bridge.waiting_for_human());
        for frame in encode_result_frames(&SupervisorToolResult::failed_request(
            &first,
            "stale answer",
        ))
        .unwrap()
        {
            bridge.receive_frame(&frame).unwrap();
        }
        send(
            &mut write_client,
            json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
        )
        .await;
        assert_eq!(receive(&mut read_client).await["id"], 2);
        send(&mut write_client, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ask_user_questions","arguments":arguments()}})).await;
        let second = next_request(&mut bridge).await;
        assert!(!second.cancelled);
        assert_ne!(second.request_id, first.request_id);
        assert!(bridge.waiting_for_human());
        write_client.shutdown().await.unwrap();
        let cancelled_second = next_request(&mut bridge).await;
        assert!(cancelled_second.cancelled);
        assert_eq!(cancelled_second.request_id, second.request_id);
        assert!(!bridge.waiting_for_human());
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn human_mcp_history_limit_keeps_pending_calls_and_evicts_completed_calls() {
        let dir = tempfile::tempdir().unwrap();
        let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
        bridge.begin_turn(SupervisorToolTurn {
            native_turn: 1,
            runtime_id: uuid::Uuid::new_v4(),
        });
        let (client, server) = tokio::io::duplex(100_000);
        let (read, write) = tokio::io::split(server);
        let (read_client, mut write_client) = tokio::io::split(client);
        let mut read_client = BufReader::new(read_client);
        let server = tokio::spawn(serve_with_limit(
            BufReader::new(read),
            write,
            bridge.socket_path(),
            1,
        ));
        send(&mut write_client, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask_user_questions","arguments":arguments()}})).await;
        let first = next_request(&mut bridge).await;
        send(&mut write_client, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask_user_questions","arguments":arguments()}})).await;
        assert_eq!(receive(&mut read_client).await["result"]["isError"], true);
        assert!(bridge.waiting_for_human());
        for frame in
            encode_result_frames(&SupervisorToolResult::failed_request(&first, "answer")).unwrap()
        {
            bridge.receive_frame(&frame).unwrap();
        }
        assert_eq!(receive(&mut read_client).await["id"], 1);
        send(&mut write_client, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ask_user_questions","arguments":arguments()}})).await;
        let next = next_request(&mut bridge).await;
        assert_ne!(next.request_id, first.request_id);
        server.abort();
    }

    #[tokio::test]
    async fn human_mcp_invalid_proposals_fail_without_creating_a_wait() {
        let dir = tempfile::tempdir().unwrap();
        let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
        bridge.begin_turn(SupervisorToolTurn {
            native_turn: 1,
            runtime_id: uuid::Uuid::new_v4(),
        });
        let (client, server) = tokio::io::duplex(100_000);
        let (read, write) = tokio::io::split(server);
        let (reader, mut writer) = tokio::io::split(client);
        let mut reader = BufReader::new(reader);
        let server = tokio::spawn(serve(BufReader::new(read), write, bridge.socket_path()));
        for (id, name) in [(1, "ask_user_questions"), (2, "request_plan_approval")] {
            send(&mut writer, json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}})).await;
            assert_eq!(receive(&mut reader).await["result"]["isError"], true);
            assert!(bridge.drain_requests().is_empty());
            assert!(!bridge.waiting_for_human());
        }
        server.abort();
    }
}
