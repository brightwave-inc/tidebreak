use super::*;

const RENDERER_ERRORS: &str = "/diagnostics/renderer-errors";

/// The renderer's error intake takes the launch bearer, writes what it is
/// sent, and answers `429` once this process has written its allowance, so a
/// render loop that throws every frame cannot flood the log.
#[tokio::test]
async fn renderer_errors_need_the_bearer_and_are_rate_limited() {
    let (router, token, _store, _dir) = test_app().await;
    let report = serde_json::json!({
        "kind": "render",
        "message": "Cannot read properties of undefined",
        "stack": "TypeError: Cannot read properties of undefined\n    at Row",
        "component_stack": "\n    at Row\n    at Transcript",
    });

    let anonymous = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(RENDERER_ERRORS)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(report.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let bearer = format!("Bearer {token}");
    let mut statuses = Vec::new();
    for _ in 0..24 {
        statuses.push(
            post_json(&router, &bearer, RENDERER_ERRORS, report.clone())
                .await
                .status(),
        );
    }
    assert!(
        statuses[..20]
            .iter()
            .all(|status| *status == StatusCode::NO_CONTENT),
        "{statuses:?}"
    );
    assert!(
        statuses[20..]
            .iter()
            .all(|status| *status == StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
}

/// A report with a field the route does not know is refused rather than
/// logged, so the renderer cannot smuggle extra payload into the log.
#[tokio::test]
async fn a_renderer_error_with_unknown_fields_is_refused() {
    let (router, token, _store, _dir) = test_app().await;
    let response = post_json(
        &router,
        &format!("Bearer {token}"),
        RENDERER_ERRORS,
        serde_json::json!({
            "kind": "error",
            "message": "boom",
            "transcript": "the whole conversation",
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
