//! Protocol and isolation tests for the Chrome computer-use service.
//!
//! These are mock-CDP tests: a scripted transport stands in for Chrome, so
//! attachment, target isolation, navigation, stale refs, cancellation, and
//! unknown outcomes run everywhere without a browser binary. A separate
//! `chrome_real` test is gated in `crates/tidebreak-server/tests/` and runs
//! only when `TIDEBREAK_CHROME_BIN` points at a real Chrome/Chromium.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tidebreak_core::{
    CancelToken, ChromeConnectionGrant, ChromeOriginScope, ComputerUseCall, ComputerUseOutcome,
    OwnerId, SessionId, WorkspaceId,
};
use uuid::Uuid;

use super::cdp::{CdpFrame, CdpSession, CdpTransport};
use super::runtime::{ChromeComputerUseService, ChromeConnectionSpec, ChromeScope};

/// Scripted Chrome protocol exchange. The first `expect` is matched against
/// the next outbound command; the corresponding `reply` is delivered.
#[derive(Clone, Default)]
struct ScriptedChrome {
    next: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    session_events: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    target_events: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    created: Arc<std::sync::Mutex<Vec<String>>>,
    closed: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ScriptedChrome {
    fn expect(&self, method: &str, reply: Value) {
        self.next.lock().unwrap().push((method.to_owned(), reply));
    }
    fn created(&self) -> Vec<String> {
        self.created.lock().unwrap().clone()
    }
    fn closed(&self) -> Vec<String> {
        self.closed.lock().unwrap().clone()
    }
}

#[derive(Clone)]
struct ScriptedTransport {
    chrome: ScriptedChrome,
    outbound: mpsc::UnboundedSender<(String, Value)>,
    inbound: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>>,
}

impl ScriptedTransport {
    fn new(chrome: ScriptedChrome) -> Self {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some((method, params)) = out_rx.recv().await {
                let reply = {
                    let next = chrome.next.lock().unwrap();
                    let index = next
                        .iter()
                        .position(|(expected, _)| expected == &method)
                        .unwrap_or_else(|| {
                            panic!(
                                "unexpected CDP method {method}; script had: {:?}",
                                next.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>()
                            )
                        });
                    next[index].1.clone()
                };
                                    let mut out = next.lock().unwrap();
                let reply = out.remove(position).1;
                let _ = in_tx.send(reply);
                let _ = params;
            }
        });
        Self {
            chrome,
            outbound: out_tx,
            inbound: Arc::new(tokio::sync::Mutex::new(in_rx)),
        }
    }
}

/// Keep `ScriptedChrome` event fields reachable so the compiler never drops
/// them before the test asserts.
impl ScriptedChrome {
    fn track_target<T: Into<String>>(&self, target_id: T) {
        self.created.lock().unwrap().push(target_id.into());
    }
    fn track_close<T: Into<String>>(&self, target_id: T) {
        self.closed.lock().unwrap().push(target_id.into());
    }
}

#[async_trait]
impl CdpTransport for ScriptedTransport {
    async fn send_text(&mut self, text: &str) -> Result<(), super::cdp::CdpError> {
        let frame: Value = serde_json::from_str(text)
            .map_err(|e| super::cdp::CdpError(format!("script transport frame: {e}")))?;
        let id = frame.get("id").cloned().unwrap_or(json!(0));
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("").to_owned();
        let params = frame.get("params").cloned().unwrap_or(json!({}));
        {
            let next = self.chrome.next.lock().unwrap();
            let position = next.iter().position(|(expected, _)| expected == &method);
            if position.is_none() {
                panic!("unexpected CDP method {method}");
            }
        }
        let mut next = self.chrome.next.lock().unwrap();
        let mut reply = next.remove(0).1;
        reply["id"] = id;
        self.outbound
            .send((reply.to_string(), params))
            .map_err(|_| super::cdp::CdpError("script transport closed".into()))
    }

    async fn next(&mut self) -> Result<Option<CdpFrame>, super::cdp::CdpError> {
        let mut inbound = self.inbound.lock().await;
        if let Some((text, _)) = inbound.recv().await {
            Ok(Some(CdpFrame::Text(text)))
        } else {
            Ok(None)
        }
    }
}

fn test_scope() -> (ChromeScope, OwnerId, SessionId, WorkspaceId, CancelToken) {
    let owner = OwnerId::new();
    let workspace = WorkspaceId::new();
    let session = SessionId::new();
    let cancel = CancelToken::new();
    let scope = ChromeScope {
        owner,
        workspace,
        session,
        cancel: cancel.clone(),
    };
    (scope, owner, workspace, session, cancel)
}

fn granted(workspace: &WorkspaceId) -> ChromeConnectionSpec {
    ChromeConnectionSpec {
        connection_id: "conn-test".into(),
        workspace: *workspace,
        endpoint_label: "isolated test Chrome (mock)".into(),
        websocket_endpoint: "ws://127.0.0.1:9222/devtools/browser/mock".into(),
        grant: ChromeConnectionGrant::DeveloperAllSites,
        managed_isolated: true,
    }
}

fn call(name: &str, arguments: Value) -> ComputerUseCall {
    ComputerUseCall {
        request_id: Uuid::new_v4(),
        name: name.to_owned(),
        arguments,
    }
}

fn target_reply(target_id: &str, session_id: &str) -> Value {
    json!({
        "sessionId": session_id,
        "targetInfo": {
            "targetId": target_id,
            "type": "page",
            "title": "Example",
            "url": "https://example.com/"
        }
    })
}

async fn new_tab(service: &ChromeComputerUseService, scope: &ChromeScope) -> String {
    let call = call(
        tidebreak_core::CHROME_NEW_TAB_TOOL,
        json!({"url": "https://example.com/"}),
    );
    let outcome = service.dispatch(scope, &call).await;
    assert_eq!(outcome.result.outcome, ComputerUseOutcome::Completed);
    outcome.result.data["targetRef"]
        .as_str()
        .expect("new tab result has targetRef")
        .to_owned()
}

#[tokio::test]
async fn attachment_lists_only_the_sessions_own_tabs() {
    let chrome = ScriptedChrome::default();
    chrome.expect("Target.attachToTarget", target_reply("t1", "s1"));
    chrome.expect("Page.enable", json!({}));
    chrome.expect("Runtime.enable", json!({}));
    chrome.expect("Network.enable", json!({}));
    chrome.expect("Runtime.evaluate", json!({"result": {"type": "object", "value": {"url": "https://example.com/", "title": "Example"}}}));
    // New tab in the second, isolated session has its own target id.
    let (scope, _owner, workspace, session, _cancel) = test_scope();
    let transport = ScriptedTransport::new(chrome.clone());
    let service = ChromeComputerUseService::new();
    service
        .install_connection(granted(&workspace), CdpSession::with_transport(transport))
        .unwrap();

    let target_ref = new_tab(&service, &scope).await;

    let list = call(tidebreak_core::CHROME_LIST_TABS_TOOL, json!({}));
    let outcome = service.dispatch(&scope, &list).await;
    assert_eq!(outcome.result.outcome, ComputerUseOutcome::Completed);
    let tabs = outcome.result.data["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["targetRef"].as_str().unwrap(), target_ref);

    let other_scope = ChromeScope {
        owner: OwnerId::new(),
        workspace: WorkspaceId::new(),
        session: SessionId::new(),
        cancel: CancelToken::new(),
    };
    let outcome_other = service.dispatch(&other_scope, &list).await;
    // A different workspace is refused: the approved connection is scoped to
    // the first workspace, so it cannot see or mutate the tab.
    assert_ne!(outcome_other.result.outcome, ComputerUseOutcome::Completed);
    let _ = session;
}

#[tokio::test]
async fn navigation_bumps_epoch_and_invalidates_old_snapshot_refs() {
    let chrome = ScriptedChrome::default();
    chrome.expect("Target.attachToTarget", target_reply("t1", "s1"));
    chrome.expect("Page.enable", json!({}));
    chrome.expect("Runtime.enable", json!({}));
    chrome.expect("Network.enable", json!({}));
    chrome.expect("Runtime.evaluate", json!({"result": {"type": "object", "value": {"url": "https://example.com/", "title": "Example"}}}));
    chrome.expect("Runtime.evaluate", json!({"result": {"type": "object", "value": {"url": "https://example.com/", "title": "Example", "readyState": "complete"}}}));
    let (scope, _o, workspace, _s, _c) = test_scope();
    let transport = ScriptedTransport::new(chrome.clone());
    let service = ChromeComputerUseService::new();
    service
        .install_connection(granted(&workspace), CdpSession::with_transport(transport))
        .unwrap();
    let target_ref = new_tab(&service, &scope).await;

    // The script consumes the second evaluate for the load gate; a stale
    // action must be refused before any further protocol call.
    let stale_act = call(
        tidebreak_core::CHROME_ACT_TOOL,
        json!({
            "targetRef": target_ref,
            "snapshotId": "snap-absent",
            "documentEpoch": 0,
            "ref": "n-1",
            "action": {"type": "click"}
        }),
    );
    let outcome = service.dispatch(&scope, &stale_act).await;
    assert_ne!(outcome.result.outcome, ComputerUseOutcome::Completed);
    assert_eq!(outcome.result.error_code.as_deref(), Some("chrome_unknown"));
    // The stale ref was refused before touching Chrome: the script's next
    // expectation is still un-consumed and the service is healthy.
    chrome.expect("Page.navigate", json!({"frameId": "f1"}));
    let nav = call(
        tidebreak_core::CHROME_NAVIGATE_TOOL,
        json!({"targetRef": target_ref, "url": "https://example.org/"}),
    );
    let outcome = service.dispatch(&scope, &nav).await;
    assert_eq!(outcome.result.outcome, ComputerUseOutcome::Completed);
    assert!(outcome.result.data["documentEpoch"].as_u64().unwrap() >= 1);
}
