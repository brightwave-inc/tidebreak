//! Image uploads and attachments handed to the engine.

use super::code::*;

use std::sync::Arc;

use axum::Router;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CapLevel, CodeSessionId, CodeSessionLifecycle, Store};

async fn code_app_with_put_gate(
    adapter: ScriptedAdapter,
    gate: Arc<PutGate>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_options(adapter, None, Some(gate), None, false).await
}

/// The smallest valid PNG: a 1x1 RGBA pixel.
///
/// Attachment paths run the real ingest, which reads dimensions out of the
/// header, so a signature followed by filler is not an image and never was —
/// it only used to reach the turn because resolution trusted the blob store
/// rather than a publication.
fn one_pixel_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

/// Publish one image to a session and return its blob id.
///
/// Publication is the authority a turn attachment is checked against, so a
/// test that attaches an image has to reserve it the way a client does.
async fn publish_one_pixel_png(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session_id: &str,
) -> String {
    let response = client
        .post(format!(
            "http://{addr}/code/sessions/{session_id}/attachments/images"
        ))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(one_pixel_png())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::CREATED,
        "publishing the fixture image failed"
    );
    let body: serde_json::Value = response.json().await.unwrap();
    body["attachment_id"]
        .as_str()
        .or_else(|| body["blob_id"].as_str())
        .expect("the publication names the blob")
        .to_owned()
}

/// Only Claude Code's adapter puts image bytes on the wire. On every other
/// engine an attached image used to be dropped between the composer and the
/// child, because the field it rides is one those adapters never read. The
/// bytes go to disk instead and the prompt carries the path, which is the
/// route a fork's transcript already takes.
#[tokio::test]
async fn an_engine_with_no_image_protocol_is_handed_the_file_and_its_path() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_image_input(CapLevel::Unsupported)
        .with_delay(std::time::Duration::from_millis(100));
    let engine = adapter.clone();
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let worktree =
        std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap().to_owned());
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let pixels = crate::routes::image_attachment::png_header(4, 4);
    let published = client
        .post(format!(
            "http://{addr}/code/sessions/{session}/attachments/images"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(pixels.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), reqwest::StatusCode::CREATED);
    let attachment: serde_json::Value = published.json().await.unwrap();
    let blob_id = attachment["attachment_id"].as_str().unwrap().to_owned();

    let turn_request = {
        let client = client.clone();
        let token = token.clone();
        let blob_id = blob_id.clone();
        let turn_url = format!("http://{addr}/code/sessions/{session}/turns");
        tokio::spawn(async move {
            client
                .post(turn_url)
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "message": "what is in this",
                    "attachments": [{ "blob_id": blob_id, "media_type": "image/png" }],
                }))
                .send()
                .await
                .unwrap()
        })
    };

    wait_until(|| !engine.turn_inputs().is_empty()).await;
    let handed = engine.turn_inputs().remove(0);
    assert_eq!(
        handed.images, 0,
        "an engine with no image protocol is sent no image bytes"
    );
    assert!(
        handed.text.starts_with("what is in this"),
        "the message the person wrote leads: {:?}",
        handed.text
    );
    let path = handed
        .text
        .lines()
        .find_map(|line| line.strip_prefix("- `")?.strip_suffix('`'))
        .map(std::path::PathBuf::from)
        .expect("the prompt names the attachment path");
    assert!(path.is_absolute(), "the engine receives an absolute path");
    assert!(
        !path.starts_with(&worktree),
        "private attachment storage must stay outside the Git worktree: {}",
        path.display()
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        pixels,
        "the engine reads the bytes while the turn is active"
    );

    let accepted = turn_request.await.unwrap();
    let accepted_status = accepted.status();
    let accepted_body: serde_json::Value = accepted.json().await.unwrap();
    assert_eq!(
        accepted_status,
        reqwest::StatusCode::ACCEPTED,
        "{accepted_body}"
    );
    assert!(
        !path.exists(),
        "the worker removes the private attachment after the turn"
    );

    // The transcript keeps what was typed, not what the engine was handed.
    let turns: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/sessions/{session}/turns"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turns[0]["user_input"], "what is in this");

    assert!(
        !worktree.join(".tidebreak").exists(),
        "fallback delivery does not write private bytes into the worktree"
    );
}

/// The engine that has its own image path keeps it. Bytes in the protocol are
/// lossless and already captured; writing a file for Claude Code would cost a
/// tool call to read back something it can already see.
#[tokio::test]
async fn an_engine_that_states_image_input_is_still_handed_the_bytes() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_image_input(CapLevel::Supported);
    let engine = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let published = client
        .post(format!(
            "http://{addr}/code/sessions/{session}/attachments/images"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(crate::routes::image_attachment::png_header(4, 4))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), reqwest::StatusCode::CREATED);
    let attachment: serde_json::Value = published.json().await.unwrap();

    let accepted = client
        .post(format!("http://{addr}/code/sessions/{session}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "message": "what is in this",
            "attachments": [{
                "blob_id": attachment["attachment_id"],
                "media_type": "image/png",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);

    wait_until(|| !engine.turn_inputs().is_empty()).await;
    let handed = engine.turn_inputs().remove(0);
    assert_eq!(handed.images, 1, "the bytes ride the protocol");
    assert_eq!(
        handed.text, "what is in this",
        "nothing is appended to a prompt that carries the image itself"
    );
    assert!(
        !runtime
            .data_dir
            .join("code")
            .join("private")
            .join(workspace["id"].as_str().unwrap())
            .join("attachments")
            .exists(),
        "no file is written for an engine that never needs to read one"
    );
}

#[tokio::test]
async fn attachments_are_accepted_and_journaled_when_the_adapter_declares_support() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_image_input(CapLevel::Supported);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();

    let pixels = one_pixel_png();
    let blob_id = publish_one_pixel_png(&client, addr, &token, json_id(&session)).await;

    let accepted = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "message": "look at this",
            "attachments": [{
                "blob_id": blob_id,
                "media_type": "image/png",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = accepted.json().await.unwrap();
    assert_eq!(turn["attachments"][0]["blob_id"], blob_id);
    assert_eq!(turn["attachments"][0]["media_type"], "png");
    assert_eq!(turn["attachments"][0]["byte_len"], pixels.len() as u64);
    assert!(
        turn["attachments"][0].get("bytes").is_none(),
        "journaled attachment must stay a bounded reference: {turn}"
    );

    let listed = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(listed[0]["attachments"][0]["blob_id"], blob_id);
    assert_eq!(listed[0]["attachments"][0]["byte_len"], pixels.len() as u64);
}

/// A blob id is not a capability. Publication is.
///
/// The blob store is content-addressed and owner-blind, so before this an
/// attachment resolved on the strength of the bytes existing anywhere. That
/// let a session bind an id it had merely learned, and then read the pixels
/// back through its own image route. Chat has bound this with
/// `chat_image_publication` since it shipped; this is the code-mode
/// equivalent, and the assertion is that publishing to one session does not
/// authorize another.
#[tokio::test]
async fn a_live_session_image_upload_is_idempotently_published() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);
    let pixels = one_pixel_png();
    let expected = crate::routes::image_attachment::inspect_image_bytes(&pixels).unwrap();

    for _ in 0..2 {
        let response = client
            .post(format!(
                "http://{addr}/code/sessions/{session}/attachments/images"
            ))
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "image/png")
            .body(pixels.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::CREATED,
            "an exact upload retry must keep its successful publication"
        );
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["attachment_id"], expected.blob_id.to_string());
    }

    let session_id: CodeSessionId = session.parse().unwrap();
    assert_eq!(
        runtime
            .db
            .get_published_code_session_image(
                &tidebreak_core::OwnerId::local(),
                session_id,
                expected.blob_id,
            )
            .await
            .unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn an_image_upload_losing_the_session_end_race_conflicts_and_queues_retirement() {
    let gate = Arc::new(PutGate {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let (router, token, runtime, dir) =
        code_app_with_put_gate(ScriptedAdapter::new(plain_text_script()), gate.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);
    let session_id: CodeSessionId = session.parse().unwrap();
    let pixels = one_pixel_png();
    let image = crate::routes::image_attachment::inspect_image_bytes(&pixels).unwrap();
    let upload = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session = session.clone();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/sessions/{session}/attachments/images"
                ))
                .bearer_auth(&token)
                .header(reqwest::header::CONTENT_TYPE, "image/png")
                .body(pixels)
                .send()
                .await
                .unwrap()
        }
    });
    gate.started.notified().await;

    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
    )
    .await
    .unwrap()
    .unwrap();
    row.lifecycle = CodeSessionLifecycle::Ended;
    row.child_pid = None;
    assert!(tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap());
    gate.release.notify_one();

    let response = upload.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "session_ended");
    assert!(runtime
        .db
        .get_published_code_session_image(
            &tidebreak_core::OwnerId::local(),
            session_id,
            image.blob_id,
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        runtime
            .db
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        tidebreak_core::BlobRetirementStatus::Queued
    );
}

#[tokio::test]
async fn a_session_cannot_attach_an_image_published_to_another_session() {
    // The capability gate refuses attachments before authority is consulted,
    // so an engine that declares image input is what puts this test on the
    // path it means to exercise.
    let adapter = ScriptedAdapter::new(plain_text_script()).with_image_input(CapLevel::Supported);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let start = |workspace_id: String| {
        let client = client.clone();
        let token = token.clone();
        async move {
            let response = client
                .post(format!(
                    "http://{addr}/code/workspaces/{workspace_id}/sessions"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "harness": "claude_code",
                    "permission_mode": "plan",
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::CREATED);
            let body: serde_json::Value = response.json().await.unwrap();
            json_id(&body).to_owned()
        }
    };
    let owning = start(json_id(&workspace).to_owned()).await;
    let other = start(json_id(&workspace).to_owned()).await;

    // A 1x1 PNG, published to the first session only.
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let published = client
        .post(format!(
            "http://{addr}/code/sessions/{owning}/attachments/images"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(png)
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        reqwest::StatusCode::CREATED,
        "publish failed: {}",
        published.text().await.unwrap()
    );
    let published: serde_json::Value = published.json().await.unwrap();
    let blob_id = published["attachment_id"]
        .as_str()
        .or_else(|| published["blob_id"].as_str())
        .expect("the publication names the blob")
        .to_owned();

    let submit = |session: String| {
        let client = client.clone();
        let token = token.clone();
        let blob_id = blob_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "message": "look at this",
                    "attachments": [{ "blob_id": blob_id, "media_type": "image/png" }],
                }))
                .send()
                .await
                .unwrap()
        }
    };

    // The session it was published to may attach it.
    let owned = submit(owning).await;
    assert!(
        owned.status().is_success(),
        "the publishing session must be able to attach its own image: {}",
        owned.text().await.unwrap()
    );

    // A sibling session that merely knows the id may not — even though the
    // bytes are plainly present in the shared blob store.
    // Without the publication check this returns 202 with the image bound, so
    // the assertion is load-bearing rather than incidentally true.
    let stolen = submit(other).await;
    let status = stolen.status();
    let body = stolen.text().await.unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "knowing a blob id must not authorize attaching it: {body}"
    );
    assert!(
        body.contains("was not published to session"),
        "the refusal must name authority, not blob absence: {body}"
    );
}
