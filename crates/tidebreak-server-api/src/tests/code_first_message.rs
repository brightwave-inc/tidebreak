//! The first message in a new workspace, replayed from the desktop.
//!
//! `fixtures/first-message.json` holds the requests the desktop sends for a
//! new workspace's first message: create the session, publish the pasted
//! image to it, then one turn carrying a draft restored after a remount and a
//! long paste. The desktop's `firstMessage.e2e.dom.test.tsx` checks that the
//! real composer and client send exactly these. This test sends them to a real
//! server and reads the first turn back, so the pair fails if either side
//! drifts.

use super::code::*;

use std::time::Duration;

use base64::Engine as _;
use tidebreak_core::{CapLevel, SessionId};

use crate::scripted_harness::{plain_text_script, ScriptedAdapter};

#[derive(serde::Deserialize)]
struct FirstMessageFixture {
    draft: String,
    pasted_text: String,
    image: FixtureImage,
    requests: Vec<FixtureRequest>,
}

#[derive(serde::Deserialize)]
struct FixtureImage {
    media_type: String,
    base64: String,
}

#[derive(serde::Deserialize)]
struct FixtureRequest {
    method: String,
    path: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    json: Option<serde_json::Value>,
    #[serde(default)]
    body: Option<String>,
}

fn fixture() -> FirstMessageFixture {
    serde_json::from_str(include_str!("../../fixtures/first-message.json"))
        .expect("the first-message fixture parses")
}

/// Put this run's ids where the fixture names them.
fn fill(template: &str, ids: &[(&str, &str)]) -> String {
    ids.iter().fold(template.to_owned(), |text, (name, id)| {
        text.replace(&format!("{{{name}}}"), id)
    })
}

#[tokio::test]
async fn the_desktops_first_message_lands_in_the_first_turn() {
    let fixture = fixture();
    let image = base64::engine::general_purpose::STANDARD
        .decode(&fixture.image.base64)
        .unwrap();
    // Every event waits long after the send is answered: the answer has to
    // come from acceptance, not from the engine finishing.
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_allow_mode(CapLevel::Supported)
        .with_image_input(CapLevel::Supported)
        .with_delay(Duration::from_secs(60));
    let engine = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace).to_owned();

    let mut ids: Vec<(&str, String)> = vec![("workspace", workspace_id)];
    let mut accepted = None;
    for request in &fixture.requests {
        let named: Vec<(&str, &str)> = ids.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let url = format!("http://{addr}{}", fill(&request.path, &named));
        assert_eq!(request.method, "POST", "the first message only writes");
        let mut call = client.post(url).bearer_auth(&token);
        if let Some(json) = &request.json {
            let body = fill(&json.to_string(), &named);
            call = call
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        } else {
            assert_eq!(request.body.as_deref(), Some("image"));
            let media_type = request.content_type.as_deref().unwrap();
            assert_eq!(media_type, fixture.image.media_type);
            call = call
                .header(reqwest::header::CONTENT_TYPE, media_type)
                .body(image.clone());
        }
        let response = tokio::time::timeout(Duration::from_secs(10), call.send())
            .await
            .expect("every request answers without waiting for the engine")
            .unwrap();
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        assert!(
            status.is_success(),
            "{} {}: {body}",
            request.method,
            request.path
        );
        if request.path.ends_with("/sessions") {
            ids.push(("session", json_id(&body).to_owned()));
        } else if request.path.ends_with("/attachments/images") {
            ids.push(("image", body["attachment_id"].as_str().unwrap().to_owned()));
        } else if request.path.ends_with("/turns") {
            assert_eq!(status, reqwest::StatusCode::ACCEPTED);
            accepted = Some(body);
        }
    }

    // The send was answered while the engine still had the turn.
    let accepted = accepted.expect("the fixture sends a turn");
    assert_eq!(accepted["status"], "running", "{accepted}");
    assert_eq!(accepted["ordinal"], 1);

    let message = format!(
        "{}\n\n<pasted_text>\n{}\n</pasted_text>",
        fixture.draft, fixture.pasted_text
    );
    // The engine got all three in its first turn: the restored draft and
    // the paste in the prompt, and the image on its own protocol.
    wait_until(|| !engine.turn_inputs().is_empty()).await;
    let handed = engine.turn_inputs().remove(0);
    assert_eq!(handed.text, message);
    assert_eq!(handed.images, 1, "the published image rides the first turn");

    // So did the record of the turn.
    let session: SessionId = ids
        .iter()
        .find(|(name, _)| *name == "session")
        .unwrap()
        .1
        .parse()
        .unwrap();
    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session,
    )
    .await
    .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].user_input, message);
    let image_id: uuid::Uuid = ids
        .iter()
        .find(|(name, _)| *name == "image")
        .unwrap()
        .1
        .parse()
        .unwrap();
    assert_eq!(turns[0].attachments.len(), 1);
    assert_eq!(turns[0].attachments[0].blob_id, image_id);
}
