//! Real Chrome acceptance. The fixture owns an isolated temporary profile and
//! kills its browser on every exit. It never connects to a personal profile.
use serde_json::{json, Value};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};
use tidebreak_core::computer_session::*;
use tidebreak_core::*;
use tidebreak_server_core::code::chrome::cdp::CdpSession;
use tidebreak_server_core::code::chrome::{
    ChromeComputerUseService, ChromeConnectionSpec, ChromeScope,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

struct Browser {
    child: Child,
    _profile: tempfile::TempDir,
}
impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
async fn browser() -> (Browser, String) {
    let binary = std::env::var("TIDEBREAK_CHROME_BIN")
        .unwrap_or_else(|_| "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
    let profile = tempfile::tempdir().unwrap();
    let child = Command::new(binary)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--remote-debugging-port=0",
            "--remote-debugging-address=127.0.0.1",
            "--window-size=1200,900",
        ])
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg("about:blank")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("launch isolated Chrome");
    let mut browser = Browser {
        child,
        _profile: profile,
    };
    for _ in 0..200 {
        if let Ok(text) =
            std::fs::read_to_string(browser._profile.path().join("DevToolsActivePort"))
        {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if port.parse::<u16>().is_ok() && path.starts_with("/devtools/browser/") {
                    return (browser, format!("ws://127.0.0.1:{port}{path}"));
                }
            }
        }
        assert!(
            browser.child.try_wait().unwrap().is_none(),
            "Chrome exited before publishing its port"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("Chrome did not publish DevToolsActivePort")
}
struct Fixture {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}
async fn fixture() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = [0; 8192];
                let n = stream.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let response = if path == "/redirect" {
                    format!("HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/denied\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                } else {
                    let body = if path == "/denied" {
                        r#"<body><button onclick="this.textContent='Private clicked'">Private target</button><p>secret destination</p></body>"#
                            .into()
                    } else if path == "/frames" {
                        format!("<body><button>Outer</button><iframe src='http://localhost:{port}/denied'></iframe></body>")
                    } else {
                        r#"<!doctype html><html><head><title>Chrome fixture</title><style>body{font:18px sans-serif;padding:20px}button,input,select{margin:8px;padding:10px}#source,#destination{display:inline-block;width:100px;height:100px;margin:20px;background:#ddd;touch-action:none}#log{height:90px}</style></head><body>
<button id="increment" onclick="count++;this.textContent='Count '+count;console.log('counter changed');fetch('/event')">Count 0</button>
<label>Name<input id="name" aria-label="Name"></label><select aria-label="Choice" onchange="document.querySelector('#choice').textContent=this.value"><option value="a">Alpha</option><option value="b">Beta</option></select><p id="choice">a</p>
<label><input type="checkbox" aria-label="Enabled">Enabled</label><button id="replace" onclick="document.querySelector('#increment').outerHTML='<button id=increment>Replacement</button>'">Replace target</button>
<div id="source" role="button" aria-label="Drag source">Drag source</div><div id="destination" role="button" aria-label="Drag destination">Drag destination</div><p id="log">Ready for test</p>
<script>let count=0,dragging=false;source.onpointerdown=()=>dragging=true;document.onpointerup=e=>{if(dragging&&e.target===destination)log.textContent='Dragged successfully';dragging=false};setTimeout(()=>document.body.dataset.ready='yes',10);</script></body></html>"#.into()
                    };
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body)
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    Fixture { port, task }
}
fn scope() -> ChromeScope {
    ChromeScope {
        owner: OwnerId::local(),
        workspace: WorkspaceId::new(),
        session: SessionId::new(),
        cancel: CancelToken::new(),
    }
}
fn call(name: &str, arguments: Value) -> ComputerUseCall {
    ComputerUseCall {
        request_id: Uuid::new_v4(),
        name: name.into(),
        arguments,
    }
}
async fn run(
    service: &ChromeComputerUseService,
    scope: &ChromeScope,
    name: &str,
    args: Value,
) -> ComputerUseResult {
    service.dispatch(scope, &call(name, args)).await.result
}
async fn ok(
    service: &ChromeComputerUseService,
    scope: &ChromeScope,
    name: &str,
    args: Value,
) -> Value {
    let result = run(service, scope, name, args).await;
    assert!(
        result.outcome == ComputerUseOutcome::Completed,
        "{name}: {}",
        result.text
    );
    result.data
}
async fn snap(service: &ChromeComputerUseService, scope: &ChromeScope, target: &str) -> Value {
    ok(
        service,
        scope,
        CHROME_SNAPSHOT_TOOL,
        json!({"targetRef":target}),
    )
    .await
}
fn node(snapshot: &Value, name: &str) -> String {
    snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == name)
        .unwrap_or_else(|| panic!("missing node {name}: {snapshot}"))["ref"]
        .as_str()
        .unwrap()
        .into()
}
fn act_args(snapshot: &Value, name: &str, action: Value) -> Value {
    json!({"targetRef":snapshot["targetRef"],"snapshotId":snapshot["snapshotId"],"documentEpoch":snapshot["documentEpoch"],"ref":node(snapshot,name),"action":action})
}
async fn setup(
    grant: ChromeConnectionGrant,
) -> (Browser, ChromeComputerUseService, ChromeScope, CdpSession) {
    let (browser, endpoint) = browser().await;
    let cdp = CdpSession::connect(&endpoint).await.unwrap();
    let service = ChromeComputerUseService::new();
    let scope = scope();
    service
        .install_connection(
            ChromeConnectionSpec {
                connection_id: "fixture".into(),
                owner: scope.owner.clone(),
                workspace: scope.workspace,
                endpoint_label: "isolated fixture".into(),
                websocket_endpoint: endpoint,
                grant,
                managed_isolated: true,
            },
            cdp.clone(),
        )
        .unwrap();
    (browser, service, scope, cdp)
}

#[tokio::test]
#[ignore = "requires installed Chrome; runs only against an isolated temporary profile"]
async fn real_chrome_reproduces_controls_and_observes_results() {
    let fixture = fixture().await;
    let (_browser, service, scope, _cdp) = setup(ChromeConnectionGrant::DeveloperAllSites).await;
    let opened = ok(
        &service,
        &scope,
        CHROME_NEW_TAB_TOOL,
        json!({"url":fixture.url()}),
    )
    .await;
    let target = opened["targetRef"].as_str().unwrap();
    let s = snap(&service, &scope, target).await;
    assert!(!s["nodes"].as_array().unwrap().is_empty());
    let image=run(&service,&scope,CHROME_SCREENSHOT_TOOL,json!({"targetRef":target,"snapshotId":s["snapshotId"],"documentEpoch":s["documentEpoch"],"maxWidth":600})).await;
    assert!(
        image.outcome == ComputerUseOutcome::Completed,
        "{}",
        image.text
    );
    assert_eq!(image.images.len(), 1);
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(&image.images[0].base64)
        .unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    assert!(width <= 600, "image width {width}");
    let click = call(
        CHROME_ACT_TOOL,
        act_args(&s, "Count 0", json!({"type":"click"})),
    );
    let first = service.dispatch(&scope, &click).await.result;
    assert!(
        first.outcome == ComputerUseOutcome::Completed,
        "{}",
        first.text
    );
    let recovered = service.dispatch(&scope, &click).await.result;
    assert!(recovered.outcome == ComputerUseOutcome::Completed);
    let s = snap(&service, &scope, target).await;
    node(&s, "Count 1");
    ok(
        &service,
        &scope,
        CHROME_ACT_TOOL,
        act_args(&s, "Name", json!({"type":"fill","value":"Ada"})),
    )
    .await;
    let s = snap(&service, &scope, target).await;
    assert!(s["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["name"] == "Name" && n["value"] == "Ada"));
    ok(
        &service,
        &scope,
        CHROME_ACT_TOOL,
        act_args(&s, "Name", json!({"type":"fill","value":""})),
    )
    .await;
    let s = snap(&service, &scope, target).await;
    assert!(s["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["name"] == "Name" && n["value"] == ""));
    ok(
        &service,
        &scope,
        CHROME_ACT_TOOL,
        act_args(&s, "Choice", json!({"type":"select","value":"b"})),
    )
    .await;
    let s = snap(&service, &scope, target).await;
    ok(
        &service,
        &scope,
        CHROME_ACT_TOOL,
        act_args(&s, "Enabled", json!({"type":"check","checked":true})),
    )
    .await;
    let s = snap(&service, &scope, target).await;
    ok(
        &service,
        &scope,
        CHROME_ACT_TOOL,
        act_args(
            &s,
            "Drag source",
            json!({"type":"drag","to_ref":node(&s,"Drag destination")}),
        ),
    )
    .await;
    let s = snap(&service, &scope, target).await;
    let waited=ok(&service,&scope,CHROME_WAIT_TOOL,json!({"targetRef":target,"snapshotId":s["snapshotId"],"documentEpoch":s["documentEpoch"],"condition":{"kind":"text_present","text":"Dragged successfully"},"timeoutMs":1000})).await;
    assert_eq!(waited["status"], "resolved");
    let diagnostics = ok(
        &service,
        &scope,
        CHROME_DIAGNOSTICS_TOOL,
        json!({"targetRef":target,"snapshotId":s["snapshotId"],"documentEpoch":s["documentEpoch"]}),
    )
    .await;
    assert!(diagnostics["consoleEntries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["text"].as_str().unwrap_or("").contains("counter changed")));
    assert!(!diagnostics["networkEntries"].as_array().unwrap().is_empty());
    service.uninstall_connection("fixture").unwrap();
}

#[tokio::test]
#[ignore = "requires installed Chrome; runs only against an isolated temporary profile"]
async fn real_chrome_fences_identity_stale_nodes_and_stop() {
    let fixture = fixture().await;
    let (_browser, service, scope, cdp) = setup(ChromeConnectionGrant::DeveloperAllSites).await;
    let opened = ok(
        &service,
        &scope,
        CHROME_NEW_TAB_TOOL,
        json!({"url":fixture.url()}),
    )
    .await;
    let target = opened["targetRef"].as_str().unwrap();
    let s = snap(&service, &scope, target).await;
    for other in [
        ChromeScope {
            owner: OwnerId::new("other").unwrap(),
            ..scope.clone()
        },
        ChromeScope {
            workspace: WorkspaceId::new(),
            ..scope.clone()
        },
        ChromeScope {
            session: SessionId::new(),
            ..scope.clone()
        },
    ] {
        let r = run(
            &service,
            &other,
            CHROME_SNAPSHOT_TOOL,
            json!({"targetRef":target}),
        )
        .await;
        assert!(r.outcome != ComputerUseOutcome::Completed);
    }
    let target_id = service
        .discover_tabs("fixture")
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.url == fixture.url())
        .unwrap()
        .target_id;
    let attached = cdp
        .command(
            "Target.attachToTarget",
            json!({"targetId":target_id,"flatten":true}),
        )
        .await
        .unwrap();
    let debug_session = attached["sessionId"].as_str().unwrap();
    cdp.command_in_session(debug_session,"Runtime.evaluate",json!({"expression":"document.querySelector('#increment').outerHTML='<button>Unexpected action</button>'"})).await.unwrap();
    let stale = run(
        &service,
        &scope,
        CHROME_ACT_TOOL,
        act_args(&s, "Count 0", json!({"type":"click"})),
    )
    .await;
    assert!(stale.outcome != ComputerUseOutcome::Completed);
    service.ownership().trip();
    let stopped = run(
        &service,
        &scope,
        CHROME_NEW_TAB_TOOL,
        json!({"url":fixture.url()}),
    )
    .await;
    assert!(stopped.outcome == ComputerUseOutcome::Rejected);
    service.ownership().resume();
    service.revoke_session(&scope.session);
    let revoked = run(
        &service,
        &scope,
        CHROME_NEW_TAB_TOOL,
        json!({"url":fixture.url()}),
    )
    .await;
    assert!(revoked.outcome == ComputerUseOutcome::Rejected);
    service.uninstall_connection("fixture").unwrap();
}

#[tokio::test]
#[ignore = "requires installed Chrome; runs only against an isolated temporary profile"]
async fn real_chrome_attaches_only_selected_tabs_and_controls_cross_origin_frames() {
    let fixture = fixture().await;
    let (_browser, service, scope, cdp) = setup(ChromeConnectionGrant::DeveloperAllSites).await;
    let target = cdp
        .command(
            "Target.createTarget",
            json!({"url":format!("{}frames",fixture.url())}),
        )
        .await
        .unwrap();
    let target_id = target["targetId"].as_str().unwrap();
    let discovered = service.discover_tabs("fixture").await.unwrap();
    assert!(discovered.iter().any(|tab| tab.target_id == target_id));
    let empty = ok(&service, &scope, CHROME_LIST_TABS_TOOL, json!({})).await;
    assert!(empty["tabs"].as_array().unwrap().is_empty());
    let tab = service
        .attach_existing_tab(&scope, "fixture", target_id)
        .await
        .unwrap();
    let other = ChromeScope {
        session: SessionId::new(),
        ..scope.clone()
    };
    assert!(service
        .attach_existing_tab(&other, "fixture", target_id)
        .await
        .is_err());
    let mut snapshot = snap(&service, &scope, &tab.target_ref).await;
    for _ in 0..30 {
        if snapshot["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "Private target")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        snapshot = snap(&service, &scope, &tab.target_ref).await;
    }
    ok(
        &service,
        &scope,
        CHROME_ACT_TOOL,
        act_args(&snapshot, "Private target", json!({"type":"click"})),
    )
    .await;
    let snapshot = snap(&service, &scope, &tab.target_ref).await;
    node(&snapshot, "Private clicked");
    let result=run(&service,&scope,CHROME_SCREENSHOT_TOOL,json!({"targetRef":tab.target_ref,"snapshotId":snapshot["snapshotId"],"documentEpoch":snapshot["documentEpoch"]})).await;
    assert!(
        result.outcome == ComputerUseOutcome::Completed,
        "{}",
        result.text
    );
    service.uninstall_connection("fixture").unwrap();
}
