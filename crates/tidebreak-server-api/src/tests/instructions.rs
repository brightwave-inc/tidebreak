//! Personal and project instructions: the member-plane route that stores a
//! person's own, and what a Work-mode turn composes into its system prompt.

use super::*;

use super::memory::{wait_for_turns, SystemPromptRecorder};

const PERSONAL_HEADING: &str = "## Personal instructions";
const PROJECT_HEADING: &str = "## Project instructions";

async fn json_request(
    router: &Router,
    bearer: &str,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn put_personal(router: &Router, bearer: &str, instructions: &str) -> StatusCode {
    json_request(
        router,
        bearer,
        "PUT",
        "/settings/instructions",
        serde_json::json!({ "instructions": instructions }),
    )
    .await
    .status()
}

async fn read_personal(router: &Router, bearer: &str) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/settings/instructions")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body::<serde_json::Value>(response).await["instructions"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn set_project_instructions(
    router: &Router,
    bearer: &str,
    project: ProjectId,
    instructions: &str,
) {
    let response = json_request(
        router,
        bearer,
        "PATCH",
        &format!("/projects/{project}"),
        serde_json::json!({ "instructions": instructions }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn make_chat_in(router: &Router, bearer: &str, project: ProjectId) -> Chat {
    let response = json_request(
        router,
        bearer,
        "POST",
        "/chats",
        serde_json::json!({ "project_id": project }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await
}

#[tokio::test]
async fn personal_instructions_round_trip_and_the_cap_is_enforced_on_save() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    assert_eq!(read_personal(&router, &bearer).await, "");

    let brief = "Answer in British English.\n\nLead with the answer.";
    let saved = json_request(
        &router,
        &bearer,
        "PUT",
        "/settings/instructions",
        serde_json::json!({ "instructions": brief }),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(saved).await,
        serde_json::json!({ "instructions": brief })
    );
    assert_eq!(read_personal(&router, &bearer).await, brief);

    // The cap counts bytes, so a refused save leaves the stored text alone.
    for refused in [
        "a".repeat(8_193),
        "é".repeat(4_097),
        "Answer\u{0}briefly".to_owned(),
    ] {
        assert_eq!(
            put_personal(&router, &bearer, &refused).await,
            StatusCode::BAD_REQUEST
        );
    }
    let unknown_field = json_request(
        &router,
        &bearer,
        "PUT",
        "/settings/instructions",
        serde_json::json!({ "instructions": "x", "scope": "everyone" }),
    )
    .await;
    assert!(unknown_field.status().is_client_error());
    assert_eq!(read_personal(&router, &bearer).await, brief);

    // Text at the cap reaches the handler even when JSON escaping makes each
    // character six bytes on the wire.
    let at_cap = "\u{1}".repeat(8_192);
    assert_eq!(
        put_personal(&router, &bearer, &at_cap).await,
        StatusCode::OK
    );
    assert_eq!(read_personal(&router, &bearer).await, at_cap);

    assert_eq!(put_personal(&router, &bearer, "").await, StatusCode::OK);
    assert_eq!(read_personal(&router, &bearer).await, "");
}

/// The composed prompt carries the personal instructions, then the project's,
/// only when they are set, and stays byte-identical across turns until
/// someone edits them.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_prompt_carries_personal_then_project_instructions() {
    let recorder = SystemPromptRecorder::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let prompts = || recorder.prompts.lock().unwrap().clone();

    // Nothing set: neither section composes.
    let loose = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, loose.id, "turn one").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, loose.id, 1).await;
    let bare = prompts()[0].clone();
    assert!(!bare.contains(PERSONAL_HEADING), "{bare}");
    assert!(!bare.contains(PROJECT_HEADING), "{bare}");

    assert_eq!(
        put_personal(&router, &bearer, "Answer in British English.").await,
        StatusCode::OK
    );
    let project = make_project(&router, &bearer).await;
    set_project_instructions(
        &router,
        &bearer,
        project.id,
        "Cite the filing for every figure.",
    )
    .await;
    let filed = make_chat_in(&router, &bearer, project.id).await;
    for (turn, content) in ["turn one", "turn two"].into_iter().enumerate() {
        assert_eq!(
            send_message(&router, &bearer, filed.id, content).await,
            StatusCode::ACCEPTED
        );
        wait_for_turns(&store, filed.id, turn + 1).await;
    }
    let filed_prompts = prompts();
    let first = &filed_prompts[1];
    let personal_heading = first.find(PERSONAL_HEADING).expect(first);
    let personal = first.find("Answer in British English.").unwrap();
    let project_heading = first.find(PROJECT_HEADING).expect(first);
    let brief = first.find("Cite the filing for every figure.").unwrap();
    assert!(personal_heading < personal && personal < project_heading && project_heading < brief);
    assert!(
        first.starts_with(bare.as_str()),
        "the host prompt stays ahead"
    );
    assert_eq!(
        filed_prompts[1], filed_prompts[2],
        "unchanged instructions keep the prompt byte-identical across turns"
    );

    // A conversation outside the project gets the personal instructions only.
    assert_eq!(
        send_message(&router, &bearer, loose.id, "turn two").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, loose.id, 2).await;
    let loose_prompt = prompts()[3].clone();
    assert!(loose_prompt.contains("Answer in British English."));
    assert!(!loose_prompt.contains(PROJECT_HEADING), "{loose_prompt}");

    // Clearing both brings back the prompt a conversation had before either.
    assert_eq!(put_personal(&router, &bearer, "").await, StatusCode::OK);
    set_project_instructions(&router, &bearer, project.id, "").await;
    assert_eq!(
        send_message(&router, &bearer, filed.id, "turn three").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, filed.id, 3).await;
    assert_eq!(prompts()[4], bare);
}
