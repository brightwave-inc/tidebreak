use super::cdp::{CdpError, CdpFrame, CdpSession, CdpTransport};
use super::{ChromeComputerUseService, ChromeConnectionSpec, ChromeScope};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tidebreak_core::computer_session::{ComputerUseCall, ComputerUseOutcome};
use tidebreak_core::{
    CancelToken, ChromeConnectionGrant, OwnerId, SessionId, WorkspaceId, CHROME_NEW_TAB_TOOL,
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
    let ids = ids.lock().unwrap();
    assert_ne!(ids[0], ids[1]);
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
