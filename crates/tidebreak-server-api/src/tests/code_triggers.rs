//! Trigger mutations and re-arming over the repository path.

use super::code::*;

use crate::scripted_harness::{plain_text_script, ScriptedAdapter};

/// Arming one condition twice must answer with the row that exists.
///
/// `arm_trigger` upserts on `(owner, repo, condition)` and updates action and
/// enabled without touching the stored id. Returning the freshly minted id
/// instead would answer 201 with a trigger that `GET`, `PATCH` and `DELETE`
/// cannot find.
#[tokio::test]
async fn re_arming_a_condition_keeps_the_stored_trigger_id() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo_path = init_git_repo(dir.path());
    let (repo, _workspace) = register_and_workspace(&client, addr, &token, &repo_path).await;
    let repo_id = json_id(&repo).to_owned();

    let armed = client
        .post(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "checks_failed", "action": "deliver" }))
        .send()
        .await
        .unwrap();
    assert_eq!(armed.status(), reqwest::StatusCode::CREATED);
    let first: serde_json::Value = armed.json().await.unwrap();

    // Same condition, different action: the store's unique key collides.
    let rearmed = client
        .post(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "checks_failed", "action": "notify" }))
        .send()
        .await
        .unwrap();
    assert_eq!(rearmed.status(), reqwest::StatusCode::CREATED);
    let second: serde_json::Value = rearmed.json().await.unwrap();

    assert_eq!(
        json_id(&second),
        json_id(&first),
        "re-arming a condition must keep the stored id"
    );
    assert_eq!(
        second["action"], "notify",
        "the action is the one just armed"
    );

    let listed = client
        .get(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(rows.len(), 1, "one row per condition, not one per action");
    assert_eq!(json_id(&rows[0]), json_id(&first));

    // The id it answered with has to be one the other routes can reach.
    let patched = client
        .patch(format!(
            "http://{addr}/code/repos/{repo_id}/triggers/{}",
            json_id(&second)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), reqwest::StatusCode::OK);
    let patched_body: serde_json::Value = patched.json().await.unwrap();
    assert_eq!(patched_body["enabled"], false);
    assert_eq!(
        patched_body["action"], "notify",
        "an enabled toggle must not overwrite the action"
    );

    // POST means arm: when it serializes after a disable, it deliberately
    // chooses the requested action and enables the existing row.
    let armed_again = client
        .post(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "checks_failed", "action": "deliver" }))
        .send()
        .await
        .unwrap();
    assert_eq!(armed_again.status(), reqwest::StatusCode::CREATED);
    let armed_again: serde_json::Value = armed_again.json().await.unwrap();
    assert_eq!(json_id(&armed_again), json_id(&first));
    assert_eq!(armed_again["action"], "deliver");
    assert_eq!(armed_again["enabled"], true);
}

/// A trigger id is not authority to mutate it through another repository's
/// route. Both writes must return not found and leave the owning row intact.
#[tokio::test]
async fn trigger_mutations_require_the_repository_in_the_path() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let left = init_git_repo_named(dir.path(), "left-trigger-repo");
    let right = init_git_repo_named(dir.path(), "right-trigger-repo");
    let (left_repo, _left_workspace) = register_and_workspace(&client, addr, &token, &left).await;
    let (right_repo, _right_workspace) =
        register_and_workspace(&client, addr, &token, &right).await;
    let left_id = json_id(&left_repo).to_owned();
    let right_id = json_id(&right_repo).to_owned();

    let armed = client
        .post(format!("http://{addr}/code/repos/{right_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "conflicts", "action": "notify" }))
        .send()
        .await
        .unwrap();
    assert_eq!(armed.status(), reqwest::StatusCode::CREATED);
    let trigger: serde_json::Value = armed.json().await.unwrap();
    let trigger_id = json_id(&trigger).to_owned();

    let patched = client
        .patch(format!(
            "http://{addr}/code/repos/{left_id}/triggers/{trigger_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted = client
        .delete(format!(
            "http://{addr}/code/repos/{left_id}/triggers/{trigger_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NOT_FOUND);

    let listed = client
        .get(format!("http://{addr}/code/repos/{right_id}/triggers"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let rows: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(json_id(&rows[0]), trigger_id);
    assert_eq!(rows[0]["enabled"], true);
}
