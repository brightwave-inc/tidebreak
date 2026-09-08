use super::cdp::{CdpError, CdpFrame, CdpSession, CdpTransport};
use super::{ChromeComputerUseService, ChromeConnectionSpec, ChromeScope};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tidebreak_core::computer_session::{ComputerUseCall, ComputerUseOutcome, ComputerUseResult};
use tidebreak_core::{
    CancelToken, ChromeConnectionGrant, OwnerId, SessionId, WorkspaceId, CHROME_ACT_TOOL,
    CHROME_NEW_TAB_TOOL, CHROME_SCREENSHOT_TOOL, CHROME_SNAPSHOT_TOOL,
};
use tokio::sync::mpsc;
use uuid::Uuid;

struct Transport {
    sent: mpsc::UnboundedSender<Value>,
    received: mpsc::UnboundedReceiver<CdpFrame>,
}
#[async_trait]
impl CdpTransport for Transport {
    async fn send_text(&mut self, text: &str) -> Result<(), CdpError> {
        self.sent
            .send(serde_json::from_str(text).unwrap())
            .map_err(|_| CdpError("test stopped".into()))
    }
    async fn next(&mut self) -> Result<Option<CdpFrame>, CdpError> {
        Ok(self.received.recv().await)
    }
}
fn connection() -> (
    ChromeComputerUseService,
    ChromeScope,
    mpsc::UnboundedReceiver<Value>,
    mpsc::UnboundedSender<CdpFrame>,
) {
    let (sent, requests) = mpsc::unbounded_channel();
    let (replies, received) = mpsc::unbounded_channel();
    let cdp = CdpSession::with_transport(Transport { sent, received });
    let service = ChromeComputerUseService::new();
    let scope = ChromeScope {
        owner: OwnerId::local(),
        workspace: WorkspaceId::new(),
        session: SessionId::new(),
        cancel: CancelToken::new(),
    };
    service
        .install_connection(
            ChromeConnectionSpec {
                connection_id: "test".into(),
                owner: scope.owner.clone(),
                workspace: scope.workspace,
                endpoint_label: "fixture".into(),
                websocket_endpoint: "ws://127.0.0.1:9222/devtools/browser/test".into(),
                grant: ChromeConnectionGrant::DeveloperAllSites,
                managed_isolated: true,
            },
            cdp,
        )
        .unwrap();
    (service, scope, requests, replies)
}
fn new_tab() -> ComputerUseCall {
    ComputerUseCall {
        request_id: Uuid::new_v4(),
        name: CHROME_NEW_TAB_TOOL.into(),
        arguments: json!({"url":"http://127.0.0.1:3000/"}),
    }
}

#[tokio::test]
async fn stop_drains_held_input_before_a_resumed_action_can_press_again() {
    let (service, scope, requests, replies) = connection();
    let reply_inject = replies.clone();
    let log = Arc::new(Mutex::new(Vec::new()));
    let pressed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let released = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    respond(requests, replies, log.clone(), move |request| {
        if key_event(request, "keyDown")
            && pressed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
        {
            return Scripted::Hold;
        }
        if key_event(request, "keyUp")
            && released.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
        {
            return Scripted::Hold;
        }
        page_reply(request)
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let first = dispatched(
        &service,
        &scope,
        act(
            &target,
            &snapshot,
            "n-0-0",
            json!({"type":"press","key":"Enter"}),
        ),
    )
    .await;
    logged(&log, |request| key_event(request, "keyDown")).await;
    service.ownership().trip();
    let cleanup = logged(&log, |request| key_event(request, "keyUp")).await;
    service.ownership().resume();
    let next = tokio::spawn({
        let service = service.clone();
        let scope = scope.clone();
        async move {
            let snapshot = service
                .dispatch(
                    &scope,
                    &call(CHROME_SNAPSHOT_TOOL, json!({"targetRef":target})),
                )
                .await
                .result;
            assert_eq!(snapshot.outcome, ComputerUseOutcome::Completed);
            let snapshot_id = snapshot.data["snapshotId"].as_str().unwrap();
            service
                .dispatch(
                    &scope,
                    &act(
                        &target,
                        snapshot_id,
                        "n-0-0",
                        json!({"type":"press","key":"Enter"}),
                    ),
                )
                .await
                .result
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let first_returned_before_cleanup = first.is_finished();
    let next_pressed_before_cleanup = log
        .lock()
        .unwrap()
        .iter()
        .filter(|request| key_event(request, "keyDown"))
        .count()
        > 1;
    reply_inject
        .send(CdpFrame::Text(
            json!({"id":cleanup["id"],"result":{}}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ComputerUseOutcome::Unknown
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), next)
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ComputerUseOutcome::Completed
    );
    assert!(
        !first_returned_before_cleanup,
        "stopped action returned before its key release completed"
    );
    assert!(
        !next_pressed_before_cleanup,
        "resumed action pressed a key while old cleanup was still pending"
    );
    let inputs = log
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "Input.dispatchKeyEvent")
        .map(|request| request["params"]["type"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(inputs, ["keyDown", "keyUp", "keyDown", "keyUp"]);
}

#[tokio::test]
async fn stop_drains_a_sent_foreground_activation_before_returning() {
    let (service, scope, requests, replies) = connection();
    let reply_inject = replies.clone();
    let log = Arc::new(Mutex::new(Vec::new()));
    respond(requests, replies, log.clone(), |request| {
        if request["method"] == "Page.bringToFront" {
            Scripted::Hold
        } else {
            page_reply(request)
        }
    });
    let tab = service
        .attach_existing_tab(&scope, "test", "T1")
        .await
        .unwrap();
    let activation = call(
        tidebreak_core::CHROME_ACTIVATE_TAB_TOOL,
        json!({"targetRef":tab.target_ref}),
    );
    let mut task = dispatched(&service, &scope, activation.clone()).await;
    let sent = logged(&log, |request| request["method"] == "Page.bringToFront").await;
    service.ownership().trip();
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut task)
        .await
        .is_err());
    reply_inject
        .send(CdpFrame::Text(
            json!({"id":sent["id"],"result":{}}).to_string(),
        ))
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.outcome, ComputerUseOutcome::Unknown);
    service.ownership().resume();
    assert_eq!(
        service.dispatch(&scope, &activation).await.result.outcome,
        ComputerUseOutcome::Unknown
    );
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == "Page.bringToFront")
            .count(),
        1
    );
}

#[tokio::test]
async fn wrong_owner_and_workspace_never_reach_chrome() {
    let (service, scope, mut requests, _replies) = connection();
    for other in [
        ChromeScope {
            owner: OwnerId::new("other").unwrap(),
            ..scope.clone()
        },
        ChromeScope {
            workspace: WorkspaceId::new(),
            ..scope.clone()
        },
    ] {
        assert_eq!(
            service.dispatch(&other, &new_tab()).await.result.outcome,
            ComputerUseOutcome::Rejected
        );
    }
    assert!(requests.try_recv().is_err());
}

#[tokio::test]
async fn stop_interrupts_inflight_work_and_resume_does_not_replay_it() {
    let (service, scope, mut requests, _replies) = connection();
    let call = new_tab();
    let task = tokio::spawn({
        let service = service.clone();
        let scope = scope.clone();
        let call = call.clone();
        async move { service.dispatch(&scope, &call).await.result }
    });
    assert_eq!(
        requests.recv().await.unwrap()["method"],
        "Target.createTarget"
    );
    service.ownership().trip();
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.outcome, ComputerUseOutcome::Unknown);
    service.ownership().resume();
    let result = service.dispatch(&scope, &call).await.result;
    assert_eq!(result.outcome, ComputerUseOutcome::Unknown);
    assert!(requests.try_recv().is_err());
    let mut different = call;
    different.arguments = json!({"url":"https://example.com"});
    assert_eq!(
        service.dispatch(&scope, &different).await.result.outcome,
        ComputerUseOutcome::Rejected
    );
}

#[tokio::test]
async fn session_revocation_cancels_inflight_work_permanently() {
    let (service, scope, mut requests, _replies) = connection();
    let task = tokio::spawn({
        let service = service.clone();
        let scope = scope.clone();
        async move { service.dispatch(&scope, &new_tab()).await.result }
    });
    requests.recv().await.unwrap();
    service.revoke_session(&scope.session);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ComputerUseOutcome::Unknown
    );
    service.ownership().resume();
    assert_eq!(
        service.dispatch(&scope, &new_tab()).await.result.outcome,
        ComputerUseOutcome::Rejected
    );
    assert!(requests.try_recv().is_err());
    let other = ChromeScope {
        session: SessionId::new(),
        ..scope
    };
    assert!(service.state(&other).available);
}

#[tokio::test]
async fn uninstall_closes_inflight_transport() {
    let (service, scope, mut requests, _replies) = connection();
    let task = tokio::spawn({
        let service = service.clone();
        let scope = scope.clone();
        async move { service.dispatch(&scope, &new_tab()).await.result }
    });
    requests.recv().await.unwrap();
    service.uninstall_connection("test").unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ComputerUseOutcome::Unknown
    );
    assert!(!service.state(&scope).available);
}

#[tokio::test]
async fn cdp_clones_share_command_ids_and_honor_dispatch_guards() {
    let (sent, mut requests) = mpsc::unbounded_channel();
    let (replies, received) = mpsc::unbounded_channel();
    let cdp = CdpSession::with_transport(Transport { sent, received });
    let ids = Arc::new(Mutex::new(Vec::new()));
    let responder = tokio::spawn({
        let ids = ids.clone();
        async move {
            for _ in 0..2 {
                let request: Value = requests.recv().await.unwrap();
                ids.lock().unwrap().push(request["id"].as_u64().unwrap());
                replies
                    .send(CdpFrame::Text(
                        json!({"id":request["id"],"result":{}}).to_string(),
                    ))
                    .unwrap();
            }
        }
    });
    let a = cdp.clone();
    let b = cdp.clone();
    let (result_a, result_b) = tokio::join!(a.command("A", json!({})), b.command("B", json!({})));
    assert!(result_a.is_ok() && result_b.is_ok());
    responder.await.unwrap();
    {
        let ids = ids.lock().unwrap();
        assert_ne!(ids[0], ids[1]);
    }
    let result = cdp
        .command_guarded(None, "C", json!({}), Arc::new(|| false))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn revoked_guard_discards_command_before_transport_send() {
    let (sent, mut requests) = mpsc::unbounded_channel();
    let (_replies, received) = mpsc::unbounded_channel();
    let cdp = CdpSession::with_transport(Transport { sent, received });
    let result = cdp
        .command_guarded(
            None,
            "Input.dispatchMouseEvent",
            json!({}),
            Arc::new(|| false),
        )
        .await;
    assert!(result.is_err());
    assert!(requests.try_recv().is_err());
}

// MARK: - Scripted fake CDP server
//
// The fake Chrome below is the same channel transport the tests above use,
// driven by a per-test script so one command can fail, hang, or return a
// crafted payload while everything else answers like a healthy page.

enum Scripted {
    Result(Value),
    Error(&'static str),
    Hold,
}

fn respond(
    mut requests: mpsc::UnboundedReceiver<Value>,
    replies: mpsc::UnboundedSender<CdpFrame>,
    log: Arc<Mutex<Vec<Value>>>,
    script: impl Fn(&Value) -> Scripted + Send + 'static,
) {
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            log.lock().unwrap().push(request.clone());
            let reply = match script(&request) {
                Scripted::Result(result) => json!({"id":request["id"],"result":result}),
                Scripted::Error(message) => {
                    json!({"id":request["id"],"error":{"message":message,"code":1}})
                }
                Scripted::Hold => continue,
            };
            if replies.send(CdpFrame::Text(reply.to_string())).is_err() {
                return;
            }
        }
    });
}

const PAGE_URL: &str = "http://127.0.0.1:3000/";

fn page_reply(request: &Value) -> Scripted {
    Scripted::Result(match request["method"].as_str().unwrap_or("") {
        "Target.getTargetInfo" => {
            json!({"targetInfo":{"type":"page","url":PAGE_URL,"title":"Fixture"}})
        }
        "Target.attachToTarget" => json!({"sessionId":"S1"}),
        "Page.getFrameTree" => {
            json!({"frameTree":{"frame":{"id":"F1","loaderId":"L1","url":PAGE_URL}}})
        }
        "Page.createIsolatedWorld" => json!({"executionContextId":7}),
        "DOM.getFrameOwner" => json!({"backendNodeId":11}),
        "DOM.getBoxModel" => {
            json!({"model":{"content":[100.0,50.0,300.0,50.0,300.0,250.0,100.0,250.0]}})
        }
        "Page.getLayoutMetrics" => {
            json!({"cssVisualViewport":{"clientWidth":800.0,"clientHeight":600.0,"pageX":0.0,"pageY":0.0}})
        }
        "Runtime.evaluate" => {
            let expression = request["params"]["expression"].as_str().unwrap_or("");
            let value = if expression.contains("title:document.title") {
                json!({"title":"Fixture","width":800.0,"height":600.0,"scrollX":0.0,"scrollY":0.0})
            } else if expression == "({width:innerWidth,height:innerHeight})" {
                json!({"width":400.0,"height":400.0})
            } else if expression.contains("\"prefix\"") {
                json!({"truncated":false,"nodes":[fixture_node("n-0-0", "F1")]})
            } else {
                json!({"ok":true,"x":10.0,"y":20.0,"viewport":{"width":200.0,"height":200.0}})
            };
            json!({"result":{"value":value}})
        }
        _ => json!({}),
    })
}

fn fixture_node(reference: &str, frame: &str) -> Value {
    json!({"kind":"interactive","ref":reference,"tag":"button","role":"button","name":"Go",
        "frame":frame,"disabled":false,"sensitive":false,"actions":["click"],
        "bounds":{"x":1.0,"y":2.0,"width":10.0,"height":10.0}})
}

fn scripted_connection(
    script: impl Fn(&Value) -> Scripted + Send + 'static,
) -> (
    ChromeComputerUseService,
    ChromeScope,
    Arc<Mutex<Vec<Value>>>,
) {
    let (service, scope, requests, replies) = connection();
    let log = Arc::new(Mutex::new(Vec::new()));
    respond(requests, replies, log.clone(), script);
    (service, scope, log)
}

fn call(name: &str, arguments: Value) -> ComputerUseCall {
    ComputerUseCall {
        request_id: Uuid::new_v4(),
        name: name.into(),
        arguments,
    }
}

async fn controlled_tab(
    service: &ChromeComputerUseService,
    scope: &ChromeScope,
) -> (String, String) {
    let summary = service
        .attach_existing_tab(scope, "test", "T1")
        .await
        .unwrap();
    let snapshot = service
        .dispatch(
            scope,
            &call(
                CHROME_SNAPSHOT_TOOL,
                json!({"targetRef":summary.target_ref}),
            ),
        )
        .await
        .result;
    assert_eq!(
        snapshot.outcome,
        ComputerUseOutcome::Completed,
        "{}",
        snapshot.text
    );
    (
        summary.target_ref,
        snapshot.data["snapshotId"].as_str().unwrap().into(),
    )
}

fn act(target: &str, snapshot: &str, node: &str, action: Value) -> ComputerUseCall {
    call(
        CHROME_ACT_TOOL,
        json!({"targetRef":target,"snapshotId":snapshot,"documentEpoch":1,"ref":node,"action":action}),
    )
}

async fn dispatched(
    service: &ChromeComputerUseService,
    scope: &ChromeScope,
    request: ComputerUseCall,
) -> tokio::task::JoinHandle<ComputerUseResult> {
    let service = service.clone();
    let scope = scope.clone();
    tokio::spawn(async move { service.dispatch(&scope, &request).await.result })
}

async fn logged(log: &Arc<Mutex<Vec<Value>>>, matches: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..200 {
        if let Some(hit) = log.lock().unwrap().iter().find(|request| matches(request)) {
            return hit.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "expected protocol request was never sent; saw {:?}",
        log.lock()
            .unwrap()
            .iter()
            .map(|request| request["method"].clone())
            .collect::<Vec<_>>()
    );
}

fn key_event(request: &Value, kind: &str) -> bool {
    request["method"] == "Input.dispatchKeyEvent" && request["params"]["type"] == kind
}

fn mouse_event(request: &Value, kind: &str) -> bool {
    request["method"] == "Input.dispatchMouseEvent" && request["params"]["type"] == kind
}

#[tokio::test]
async fn failed_key_down_still_releases_the_key_and_consumes_the_snapshot() {
    let (service, scope, log) = scripted_connection(|request| {
        if key_event(request, "keyDown") {
            Scripted::Error("input rejected")
        } else {
            page_reply(request)
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let press = act(
        &target,
        &snapshot,
        "n-0-0",
        json!({"type":"press","key":"Enter"}),
    );
    let result = service.dispatch(&scope, &press).await.result;
    assert_eq!(
        result.outcome,
        ComputerUseOutcome::Unknown,
        "{}",
        result.text
    );
    let release = logged(&log, |request| key_event(request, "keyUp")).await;
    assert_eq!(release["params"]["key"], "Enter");
    let retry = act(
        &target,
        &snapshot,
        "n-0-0",
        json!({"type":"press","key":"Enter"}),
    );
    let retry = service.dispatch(&scope, &retry).await.result;
    assert!(retry.text.contains("stale"), "{}", retry.text);
}

#[tokio::test]
async fn cancelled_key_down_response_still_releases_the_key() {
    let (service, scope, log) = scripted_connection(|request| {
        if key_event(request, "keyDown") {
            Scripted::Hold
        } else {
            page_reply(request)
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let press = act(
        &target,
        &snapshot,
        "n-0-0",
        json!({"type":"press","key":"Enter"}),
    );
    let task = dispatched(&service, &scope, press).await;
    logged(&log, |request| key_event(request, "keyDown")).await;
    service.ownership().trip();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.outcome, ComputerUseOutcome::Unknown);
    logged(&log, |request| key_event(request, "keyUp")).await;
    service.ownership().resume();
    let retry = act(
        &target,
        &snapshot,
        "n-0-0",
        json!({"type":"press","key":"Enter"}),
    );
    let retry = service.dispatch(&scope, &retry).await.result;
    assert!(retry.text.contains("stale"), "{}", retry.text);
}

#[tokio::test]
async fn dropped_act_future_releases_the_button_and_consumes_the_snapshot() {
    let (service, scope, log) = scripted_connection(|request| {
        if mouse_event(request, "mousePressed") {
            Scripted::Hold
        } else {
            page_reply(request)
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let click = act(&target, &snapshot, "n-0-0", json!({"type":"click"}));
    let task = dispatched(&service, &scope, click).await;
    logged(&log, |request| mouse_event(request, "mousePressed")).await;
    task.abort();
    let _ = task.await;
    let release = logged(&log, |request| mouse_event(request, "mouseReleased")).await;
    assert_eq!(release["params"]["buttons"], 0);
    let retry = act(&target, &snapshot, "n-0-0", json!({"type":"click"}));
    let retry = service.dispatch(&scope, &retry).await.result;
    assert!(retry.text.contains("stale"), "{}", retry.text);
}

#[tokio::test]
async fn failed_drag_cancels_dragging_then_releases_at_the_origin() {
    let (service, scope, log) = scripted_connection(|request| {
        if mouse_event(request, "mouseMoved") && request["params"]["buttons"] == 1 {
            Scripted::Error("target closed")
        } else {
            page_reply(request)
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let drag = act(
        &target,
        &snapshot,
        "n-0-0",
        json!({"type":"drag","to_ref":"n-0-0"}),
    );
    let result = service.dispatch(&scope, &drag).await.result;
    assert_eq!(result.outcome, ComputerUseOutcome::Unknown);
    logged(&log, |request| request["method"] == "Input.cancelDragging").await;
    let release = logged(&log, |request| mouse_event(request, "mouseReleased")).await;
    assert_eq!(release["params"]["x"], 10.0);
    assert_eq!(release["params"]["y"], 20.0);
    let entries = log.lock().unwrap();
    let cancel_at = entries
        .iter()
        .position(|request| request["method"] == "Input.cancelDragging")
        .unwrap();
    let release_at = entries
        .iter()
        .position(|request| mouse_event(request, "mouseReleased"))
        .unwrap();
    assert!(cancel_at < release_at);
}

#[tokio::test]
async fn completed_press_releases_the_key_exactly_once() {
    let (service, scope, log) = scripted_connection(page_reply);
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let press = act(
        &target,
        &snapshot,
        "n-0-0",
        json!({"type":"press","key":"Enter"}),
    );
    let result = service.dispatch(&scope, &press).await.result;
    assert_eq!(
        result.outcome,
        ComputerUseOutcome::Completed,
        "{}",
        result.text
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let key_events = log
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "Input.dispatchKeyEvent")
        .count();
    assert_eq!(key_events, 2);
}

#[tokio::test]
async fn screenshot_refits_capture_scale_to_the_transport_budget() {
    use base64::Engine;
    let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = captures.clone();
    let (service, scope, log) = scripted_connection(move |request| {
        if request["method"] == "Page.captureScreenshot" {
            let attempt = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut bytes = Vec::new();
            let pixels = image::RgbaImage::from_pixel(320, 240, image::Rgba([0, 0, 0, 255]));
            image::DynamicImage::ImageRgba8(pixels)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes.resize(if attempt == 0 { 2_000_000 } else { 200_000 }, 0);
            Scripted::Result(
                json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes)}),
            )
        } else {
            page_reply(request)
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let shot = call(
        CHROME_SCREENSHOT_TOOL,
        json!({"targetRef":target,"snapshotId":snapshot,"documentEpoch":1}),
    );
    let result = service.dispatch(&scope, &shot).await.result;
    assert_eq!(
        result.outcome,
        ComputerUseOutcome::Completed,
        "{}",
        result.text
    );
    assert_eq!(result.images.len(), 1);
    let scales = log
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "Page.captureScreenshot")
        .map(|request| request["params"]["clip"]["scale"].as_f64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(scales.len(), 2);
    assert!(scales[1] < scales[0], "{scales:?}");
    let width = result.data["width"].as_f64().unwrap();
    let height = result.data["height"].as_f64().unwrap();
    // Report the delivered PNG pixels, including any Chrome device scale.
    assert_eq!((width, height), (320.0, 240.0));
}

#[tokio::test]
async fn nested_frame_click_uses_the_innermost_owner_in_one_session() {
    let (service, scope, log) = scripted_connection(|request| {
        match request["method"].as_str().unwrap_or("") {
            "Page.getFrameTree" => Scripted::Result(json!({"frameTree":{
                "frame":{"id":"F1","loaderId":"L1","url":PAGE_URL},
                "childFrames":[{"frame":{"id":"F2","loaderId":"L2","url":PAGE_URL},
                    "childFrames":[{"frame":{"id":"F3","loaderId":"L3","url":PAGE_URL}}]}]}})),
            "DOM.getFrameOwner" => Scripted::Result(json!({"backendNodeId":
                if request["params"]["frameId"] == "F3" { 13 } else { 12 }
            })),
            "DOM.getBoxModel" => Scripted::Result(json!({"model":{"content":
                if request["params"]["backendNodeId"] == 13 { json!([150.0,180.0,350.0,180.0,350.0,380.0,150.0,380.0]) }
                else { json!([100.0,120.0,500.0,120.0,500.0,520.0,100.0,520.0]) }
            }})),
            "Runtime.evaluate" => {
                let expression = request["params"]["expression"].as_str().unwrap_or("");
                if expression.contains("\"prefix\"") {
                    let nodes = if expression.contains("\"frame\":\"F3\"") {
                        json!([fixture_node("n-2-0", "F3")])
                    } else {
                        json!([])
                    };
                    Scripted::Result(json!({"result":{"value":{"truncated":false,"nodes":nodes}}}))
                } else {
                    page_reply(request)
                }
            }
            _ => page_reply(request),
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let click = act(&target, &snapshot, "n-2-0", json!({"type":"click"}));
    let result = service.dispatch(&scope, &click).await.result;
    assert_eq!(
        result.outcome,
        ComputerUseOutcome::Completed,
        "{}",
        result.text
    );
    let moved = logged(&log, |request| mouse_event(request, "mouseMoved")).await;
    assert_eq!(moved["params"]["x"], 160.0);
    assert_eq!(moved["params"]["y"], 200.0);
    let owners = log
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "DOM.getFrameOwner")
        .map(|request| request["params"]["frameId"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        owners,
        ["F3", "F3"],
        "each probe uses the owner box that already includes F2's offset"
    );
}

#[tokio::test]
async fn nested_cross_session_click_adds_one_owner_per_session_boundary() {
    let (service, scope, requests, replies) = connection();
    // Actual Chrome omits the out-of-process frame from the parent tree.
    // Its own tree supplies parentId, even when its URL is still loading
    // when the attach event arrives.
    replies
        .send(CdpFrame::Text(
            json!({
                "method":"Target.attachedToTarget", "sessionId":"S1",
                "params":{"sessionId":"S2","targetInfo":{"targetId":"F3","type":"iframe"}}
            })
            .to_string(),
        ))
        .unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    respond(requests, replies, log.clone(), |request| {
        match request["method"].as_str().unwrap_or("") {
            "Page.getFrameTree" if request["sessionId"] == "S2" => {
                Scripted::Result(json!({"frameTree":{
                    "frame":{"id":"F3","parentId":"F2","loaderId":"L3","url":PAGE_URL},
                    "childFrames":[{"frame":{"id":"F4","loaderId":"L4","url":PAGE_URL}}]
                }}))
            }
            "Page.getFrameTree" => Scripted::Result(json!({"frameTree":{
                "frame":{"id":"F1","loaderId":"L1","url":PAGE_URL},
                "childFrames":[{"frame":{"id":"F2","loaderId":"L2","url":PAGE_URL}}]
            }})),
            "DOM.getFrameOwner" => Scripted::Result(json!({"backendNodeId":
                match request["params"]["frameId"].as_str().unwrap() {
                    "F4" => 14,
                    "F3" => 13,
                    _ => 12,
                }
            })),
            "DOM.getBoxModel" => Scripted::Result(json!({"model":{"content":
                match request["params"]["backendNodeId"].as_u64().unwrap() {
                    14 => json!([30.0,40.0,230.0,40.0,230.0,240.0,30.0,240.0]),
                    13 => json!([150.0,180.0,550.0,180.0,550.0,580.0,150.0,580.0]),
                    _ => json!([100.0,120.0,500.0,120.0,500.0,520.0,100.0,520.0]),
                }
            }})),
            "Runtime.evaluate" => {
                let expression = request["params"]["expression"].as_str().unwrap_or("");
                if expression.contains("\"prefix\"") {
                    let nodes = if expression.contains("\"frame\":\"F4\"") {
                        json!([fixture_node("n-3-0", "F4")])
                    } else {
                        json!([])
                    };
                    Scripted::Result(json!({"result":{"value":{"truncated":false,"nodes":nodes}}}))
                } else {
                    page_reply(request)
                }
            }
            _ => page_reply(request),
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let result = service
        .dispatch(
            &scope,
            &act(&target, &snapshot, "n-3-0", json!({"type":"click"})),
        )
        .await
        .result;
    assert_eq!(
        result.outcome,
        ComputerUseOutcome::Completed,
        "{}",
        result.text
    );
    let moved = logged(&log, |request| mouse_event(request, "mouseMoved")).await;
    assert_eq!(
        moved["sessionId"], "S1",
        "mouse input reaches the top session"
    );
    assert_eq!(moved["params"]["x"], 190.0);
    assert_eq!(moved["params"]["y"], 240.0);
    let focus = log
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "Emulation.setFocusEmulationEnabled")
        .map(|request| {
            (
                request["sessionId"].as_str().unwrap().to_owned(),
                request["params"]["enabled"].as_bool().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        focus,
        [
            ("S1".into(), true),
            ("S2".into(), true),
            ("S1".into(), false),
            ("S2".into(), false)
        ]
    );
    let owners = log
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "DOM.getFrameOwner")
        .map(|request| {
            (
                request["params"]["frameId"].as_str().unwrap().to_owned(),
                request["sessionId"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        owners,
        [
            ("F4".into(), "S2".into()),
            ("F3".into(), "S1".into()),
            ("F4".into(), "S2".into()),
            ("F3".into(), "S1".into()),
        ]
    );
}

#[tokio::test]
async fn unresolved_cross_session_parent_refuses_before_mouse_input() {
    for parent_id in [None, Some("missing"), Some("F2")] {
        let (service, scope, requests, replies) = connection();
        replies
            .send(CdpFrame::Text(
                json!({
                    "method":"Target.attachedToTarget", "sessionId":"S1",
                    "params":{"sessionId":"S2","targetInfo":{"targetId":"F2","type":"iframe"}}
                })
                .to_string(),
            ))
            .unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        respond(
            requests,
            replies,
            log.clone(),
            move |request| match request["method"].as_str().unwrap_or("") {
                "Page.getFrameTree" if request["sessionId"] == "S2" => Scripted::Result(json!({
                    "frameTree":{"frame":{"id":"F2","parentId":parent_id,"loaderId":"L2","url":PAGE_URL}}
                })),
                "Runtime.evaluate" => {
                    let expression = request["params"]["expression"].as_str().unwrap_or("");
                    if expression.contains("\"prefix\"") {
                        let nodes = if expression.contains("\"frame\":\"F2\"") {
                            json!([fixture_node("n-1-0", "F2")])
                        } else {
                            json!([])
                        };
                        Scripted::Result(
                            json!({"result":{"value":{"truncated":false,"nodes":nodes}}}),
                        )
                    } else {
                        page_reply(request)
                    }
                }
                _ => page_reply(request),
            },
        );
        let (target, snapshot) = controlled_tab(&service, &scope).await;
        let result = service
            .dispatch(
                &scope,
                &act(&target, &snapshot, "n-1-0", json!({"type":"click"})),
            )
            .await
            .result;
        assert_ne!(result.outcome, ComputerUseOutcome::Completed);
        assert!(
            result.text.contains("frame position cannot be resolved"),
            "{}",
            result.text
        );
        assert!(!log
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "Input.dispatchMouseEvent"));
    }
}

#[tokio::test]
async fn background_input_scopes_page_focus_without_browser_activation() {
    let (service, scope, log) = scripted_connection(page_reply);
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let result = service
        .dispatch(
            &scope,
            &act(
                &target,
                &snapshot,
                "n-0-0",
                json!({"type":"press","key":"Enter"}),
            ),
        )
        .await
        .result;
    assert_eq!(
        result.outcome,
        ComputerUseOutcome::Completed,
        "{}",
        result.text
    );
    let entries = log.lock().unwrap();
    let sequence = entries
        .iter()
        .filter_map(|request| match request["method"].as_str()? {
            "Emulation.setFocusEmulationEnabled" => Some(if request["params"]["enabled"] == true {
                "enable"
            } else {
                "disable"
            }),
            "Input.dispatchKeyEvent" => request["params"]["type"].as_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(sequence, ["enable", "keyDown", "keyUp", "disable"]);
    assert!(!entries.iter().any(|request| matches!(
        request["method"].as_str(),
        Some("Page.bringToFront" | "Target.activateTarget")
    )));
}

#[tokio::test]
async fn ended_authority_restores_page_focus_before_returning() {
    for end in ["stop", "revoke", "cancel"] {
        let (service, scope, requests, replies) = connection();
        let inject = replies.clone();
        let log = Arc::new(Mutex::new(Vec::new()));
        respond(requests, replies, log.clone(), |request| {
            if key_event(request, "keyDown")
                || (request["method"] == "Emulation.setFocusEmulationEnabled"
                    && request["params"]["enabled"] == false)
            {
                Scripted::Hold
            } else {
                page_reply(request)
            }
        });
        let (target, snapshot) = controlled_tab(&service, &scope).await;
        let mut task = dispatched(
            &service,
            &scope,
            act(
                &target,
                &snapshot,
                "n-0-0",
                json!({"type":"press","key":"Enter"}),
            ),
        )
        .await;
        logged(&log, |request| key_event(request, "keyDown")).await;
        match end {
            "stop" => service.ownership().trip(),
            "revoke" => service.revoke_session(&scope.session),
            _ => scope.cancel.cancel(),
        };
        let reset = logged(&log, |request| {
            request["method"] == "Emulation.setFocusEmulationEnabled"
                && request["params"]["enabled"] == false
        })
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut task)
                .await
                .is_err(),
            "{end} returned before page focus reset"
        );
        inject
            .send(CdpFrame::Text(
                json!({"id":reset["id"],"result":{}}).to_string(),
            ))
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(result.outcome, ComputerUseOutcome::Completed);
    }
}

#[tokio::test]
async fn page_focus_setup_failure_refuses_without_input_or_activation() {
    let (service, scope, log) = scripted_connection(|request| {
        if request["method"] == "Emulation.setFocusEmulationEnabled"
            && request["params"]["enabled"] == true
        {
            Scripted::Error("emulation unavailable")
        } else {
            page_reply(request)
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let result = service
        .dispatch(
            &scope,
            &act(&target, &snapshot, "n-0-0", json!({"type":"click"})),
        )
        .await
        .result;
    assert_ne!(result.outcome, ComputerUseOutcome::Completed);
    assert!(
        result.text.contains("emulation unavailable"),
        "{}",
        result.text
    );
    let entries = log.lock().unwrap();
    assert!(!entries.iter().any(|request| request["method"]
        .as_str()
        .is_some_and(|method| method.starts_with("Input.")
            || method == "Page.bringToFront"
            || method == "Target.activateTarget")));
    assert!(entries.iter().any(|request| request["method"]
        == "Emulation.setFocusEmulationEnabled"
        && request["params"]["enabled"] == false));
}

#[tokio::test]
async fn page_focus_reset_failure_disconnects_before_more_input() {
    let (service, scope, _log) = scripted_connection(|request| {
        if request["method"] == "Emulation.setFocusEmulationEnabled"
            && request["params"]["enabled"] == false
        {
            Scripted::Error("reset unavailable")
        } else {
            page_reply(request)
        }
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let result = service
        .dispatch(
            &scope,
            &act(&target, &snapshot, "n-0-0", json!({"type":"click"})),
        )
        .await
        .result;
    assert_ne!(result.outcome, ComputerUseOutcome::Completed);
    let result = service
        .dispatch(
            &scope,
            &call(CHROME_SNAPSHOT_TOOL, json!({"targetRef":target})),
        )
        .await
        .result;
    assert_eq!(result.outcome, ComputerUseOutcome::Rejected);
    assert!(!service.state(&scope).available);
}

#[tokio::test]
async fn dropped_action_restores_page_focus_before_next_input() {
    let (service, scope, requests, replies) = connection();
    let inject = replies.clone();
    let log = Arc::new(Mutex::new(Vec::new()));
    let presses = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let resets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    respond(requests, replies, log.clone(), move |request| {
        if key_event(request, "keyDown")
            && presses.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
        {
            return Scripted::Hold;
        }
        if request["method"] == "Emulation.setFocusEmulationEnabled"
            && request["params"]["enabled"] == false
            && resets.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
        {
            return Scripted::Hold;
        }
        page_reply(request)
    });
    let (target, snapshot) = controlled_tab(&service, &scope).await;
    let first = dispatched(
        &service,
        &scope,
        act(
            &target,
            &snapshot,
            "n-0-0",
            json!({"type":"press","key":"Enter"}),
        ),
    )
    .await;
    logged(&log, |request| key_event(request, "keyDown")).await;
    first.abort();
    assert!(matches!(first.await, Err(error) if error.is_cancelled()));
    let reset = logged(&log, |request| {
        request["method"] == "Emulation.setFocusEmulationEnabled"
            && request["params"]["enabled"] == false
    })
    .await;
    let snapshot = service
        .dispatch(
            &scope,
            &call(CHROME_SNAPSHOT_TOOL, json!({"targetRef":target})),
        )
        .await
        .result;
    assert_eq!(snapshot.outcome, ComputerUseOutcome::Completed);
    let mut next = dispatched(
        &service,
        &scope,
        act(
            &target,
            snapshot.data["snapshotId"].as_str().unwrap(),
            "n-0-0",
            json!({"type":"press","key":"Enter"}),
        ),
    )
    .await;
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut next)
        .await
        .is_err());
    {
        let entries = log.lock().unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|request| key_event(request, "keyDown"))
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(
                    |request| request["method"] == "Emulation.setFocusEmulationEnabled"
                        && request["params"]["enabled"] == true
                )
                .count(),
            1
        );
    }
    inject
        .send(CdpFrame::Text(
            json!({"id":reset["id"],"result":{}}).to_string(),
        ))
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), next)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result.outcome,
        ComputerUseOutcome::Completed,
        "{}",
        result.text
    );
}

#[tokio::test]
async fn disconnect_during_page_input_refuses_further_commands() {
    for end in ["uninstall", "transport"] {
        let (service, scope, requests, replies) = connection();
        let inject = replies.clone();
        let log = Arc::new(Mutex::new(Vec::new()));
        respond(requests, replies, log.clone(), |request| {
            if key_event(request, "keyDown") {
                Scripted::Hold
            } else {
                page_reply(request)
            }
        });
        let (target, snapshot) = controlled_tab(&service, &scope).await;
        let task = dispatched(
            &service,
            &scope,
            act(
                &target,
                &snapshot,
                "n-0-0",
                json!({"type":"press","key":"Enter"}),
            ),
        )
        .await;
        logged(&log, |request| key_event(request, "keyDown")).await;
        if end == "uninstall" {
            service.uninstall_connection("test").unwrap();
        } else {
            inject.send(CdpFrame::Close).unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.outcome,
            ComputerUseOutcome::Unknown,
            "{end}: {}",
            result.text
        );
        assert!(!service.state(&scope).available);
        let before = log.lock().unwrap().len();
        let result = service
            .dispatch(
                &scope,
                &call(CHROME_SNAPSHOT_TOOL, json!({"targetRef":target})),
            )
            .await
            .result;
        assert_eq!(result.outcome, ComputerUseOutcome::Rejected);
        assert_eq!(
            log.lock().unwrap().len(),
            before,
            "{end} sent a command after disconnect"
        );
    }
}
