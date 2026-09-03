//! Worktree roots, restore and release, storage reclaim, and workspace reads.

use super::code::*;

use crate::scripted_harness::plain_text_script;
use tidebreak_core::{CodeWorkspaceStatus, WorkspaceId};

#[tokio::test]
async fn two_repos_with_the_same_name_get_distinct_worktrees() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let left = init_git_repo_named(&dir.path().join("left"), "origin");
    let right = init_git_repo_named(&dir.path().join("right"), "origin");
    let (repo_a, ws_a) = register_and_workspace(&client, addr, &token, &left).await;
    let (repo_b, ws_b) = register_and_workspace(&client, addr, &token, &right).await;
    assert_eq!(repo_a["display_name"], "origin");
    assert_eq!(repo_b["display_name"], "origin");
    let path_a = ws_a["worktree_path"].as_str().unwrap();
    let path_b = ws_b["worktree_path"].as_str().unwrap();
    // Same repo name, so the same repo folder — the workspace id suffix is
    // what keeps the two checkouts apart.
    assert_ne!(path_a, path_b);
    assert!(path_a.contains(&json_id(&ws_a)[..8]));
    assert!(path_b.contains(&json_id(&ws_b)[..8]));
    assert!(std::path::Path::new(path_a).join("README.md").is_file());
    assert!(std::path::Path::new(path_b).join("README.md").is_file());
}

/// The worktree root is a setting, and moving it moves only what comes next.
///
/// The two halves are one test because the second is meaningless without the
/// first: a root that new workspaces honour but old ones silently follow would
/// leave every existing checkout pointing at nothing.
#[tokio::test]
async fn the_worktree_root_moves_new_workspaces_and_leaves_existing_ones() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (repo_body, before) = register_and_workspace(&client, addr, &token, &repo).await;
    let before_path = before["worktree_path"].as_str().unwrap().to_owned();
    // The default is the data directory until a root is set.
    let defaults = client
        .get(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(defaults["root"].is_null());
    assert_eq!(defaults["effective_root"], defaults["default_root"]);
    assert!(before_path.starts_with(defaults["default_root"].as_str().unwrap()));

    // A root that does not exist yet is created rather than refused.
    let chosen = dir.path().join("visible").join("workspaces");
    let moved = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": chosen }))
        .send()
        .await
        .unwrap();
    assert_eq!(moved.status(), reqwest::StatusCode::OK);
    let moved: serde_json::Value = moved.json().await.unwrap();
    assert_eq!(moved["root"], moved["effective_root"]);
    assert!(chosen.is_dir());

    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "second change",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let after: serde_json::Value = created.json().await.unwrap();
    let after_path = after["worktree_path"].as_str().unwrap();
    assert!(
        after_path.starts_with(chosen.to_str().unwrap()),
        "{after_path}"
    );
    // Readable name first, id last.
    assert!(after_path.ends_with(&format!("second-change-{}", &json_id(&after)[..8])));
    assert!(std::path::Path::new(after_path).join("README.md").is_file());

    // The workspace created before the move keeps the path on its row, and the
    // checkout is still there.
    let reread = client
        .get(format!(
            "http://{addr}/code/workspaces/{}",
            json_id(&before)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(reread["worktree_path"], before_path);
    assert!(std::path::Path::new(&before_path)
        .join("README.md")
        .is_file());

    // Clearing the setting returns the deployment to its default and, again,
    // touches nothing on disk.
    let cleared = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": null }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(cleared["root"].is_null());
    assert_eq!(cleared["effective_root"], cleared["default_root"]);
}

/// A root the deployment cannot write worktrees under is refused when it is
/// set, not at the first workspace that fails.
#[tokio::test]
async fn the_worktree_root_refuses_a_relative_path_and_a_file() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let relative = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": "workspaces" }))
        .send()
        .await
        .unwrap();
    assert_eq!(relative.status(), reqwest::StatusCode::BAD_REQUEST);

    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();
    let refused = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": file }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn workspace_tree_is_bounded_ignores_and_never_returns_contents() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    std::fs::write(repo.join(".gitignore"), "secret.bin\n").unwrap();
    std::fs::write(repo.join("src.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo.join("secret.bin"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    std::fs::write(repo.join("notes.md"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    assert!(std::process::Command::new("git")
        .args(["add", ".gitignore", "src.rs"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["commit", "-m", "more"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(worktree.join(".gitignore"), "secret.bin\n").unwrap();
    std::fs::write(worktree.join("src.rs"), "fn main() {}\n").unwrap();
    std::fs::write(worktree.join("secret.bin"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    std::fs::write(worktree.join("notes.md"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    for index in 0..80 {
        std::fs::write(worktree.join(format!("bulk-{index:03}.txt")), "x\n").unwrap();
    }

    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/tree?query=bulk-&limit=50",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = listed.json().await.unwrap();
    assert_eq!(body["paths"].as_array().unwrap().len(), 50);
    assert_eq!(body["truncated"], true);
    let rendered = body.to_string();
    assert!(
        !rendered.contains("UNIQUE_PAYLOAD_xyz"),
        "tree route leaked file contents: {rendered}"
    );
    assert!(!rendered.contains("secret.bin"));

    let named = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/tree?query=notes",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(named["paths"][0], "notes.md");

    let searched = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/search",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .query(&[
            ("query", "unique_payload_XYZ"),
            ("include", "*.md"),
            ("limit", "50"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(searched.status(), reqwest::StatusCode::OK);
    let searched: serde_json::Value = searched.json().await.unwrap();
    assert_eq!(searched["truncated"], false);
    assert_eq!(searched["matches"].as_array().unwrap().len(), 1);
    assert_eq!(searched["matches"][0]["path"], "notes.md");
    assert_eq!(searched["matches"][0]["line_number"], 1);
    assert_eq!(searched["matches"][0]["line"], "UNIQUE_PAYLOAD_xyz");
    assert!(searched.to_string().find("secret.bin").is_none());

    let bounded_search = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/search",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .query(&[("query", "x"), ("include", "bulk-*.txt"), ("limit", "1")])
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(bounded_search["matches"].as_array().unwrap().len(), 1);
    assert_eq!(bounded_search["truncated"], true);
}

/// Archive keeps the branch; restore puts a checkout back under the same
/// workspace row. Committed work returns, force-discarded work stays gone,
/// and restoring an already-active workspace is a no-op.
#[tokio::test]
async fn restore_reactivates_an_archived_workspace_on_its_own_branch() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let id = json_id(&workspace);
    let path = workspace["worktree_path"].as_str().unwrap().to_owned();
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();

    // One committed file (should survive on the branch) and one uncommitted
    // (force-archive discards it).
    std::fs::write(std::path::Path::new(&path).join("kept.txt"), "kept\n").unwrap();
    for args in [
        ["add", "kept.txt"].as_slice(),
        ["commit", "-m", "keep this"].as_slice(),
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(std::path::Path::new(&path).join("scratch.txt"), "gone\n").unwrap();

    let archived = client
        .post(format!("http://{addr}/code/workspaces/{id}/archive"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert!(!std::path::Path::new(&path).exists());

    let restored = client
        .post(format!("http://{addr}/code/workspaces/{id}/restore"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(restored.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = restored.json().await.unwrap();
    assert_eq!(body["status"], "active");
    assert_eq!(body["worktree_path"], path.as_str());
    assert_eq!(body["branch_name"], branch.as_str());
    assert!(body.get("archived_at").is_none() || body["archived_at"].is_null());
    let kept = std::fs::read_to_string(std::path::Path::new(&path).join("kept.txt")).unwrap();
    assert_eq!(
        kept.replace("\r\n", "\n").replace('\r', "\n"),
        "kept\n",
        "restored committed content must match regardless of platform newlines"
    );
    assert!(!std::path::Path::new(&path).join("scratch.txt").exists());

    // Idempotent on an active workspace.
    let again = client
        .post(format!("http://{addr}/code/workspaces/{id}/restore"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::OK);
}

/// Restore refuses a released workspace when something claims its path.
#[tokio::test]
async fn restore_refuses_an_occupied_path() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let id = json_id(&workspace);
    let path = workspace["worktree_path"].as_str().unwrap().to_owned();
    std::fs::write(std::path::Path::new(&path).join("saved.txt"), "saved\n").unwrap();
    for args in [
        ["add", "saved.txt"].as_slice(),
        ["commit", "-m", "saved work"].as_slice(),
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
    }

    let archived = client
        .post(format!("http://{addr}/code/workspaces/{id}/archive"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);

    std::fs::create_dir_all(&path).unwrap();
    let occupied = client
        .post(format!("http://{addr}/code/workspaces/{id}/restore"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(occupied.status(), reqwest::StatusCode::CONFLICT);
    let occupied_body: serde_json::Value = occupied.json().await.unwrap();
    assert_eq!(occupied_body["kind"], "worktree_path_occupied");
    std::fs::remove_dir_all(&path).unwrap();
}

/// Archive deep-cleans over HTTP: it frees the checkout and branch, then
/// restore rebuilds both from the saved bundle.
///
/// The assertion that matters is the last one — the file the workspace's
/// commit added is back on disk after a round trip through a branch that no
/// longer exists. Without that, archive is deletion with extra steps.
#[tokio::test]
async fn archive_frees_the_branch_and_restore_rebuilds_it_from_the_bundle() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "released later",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let workspace_id = json_id(&workspace);
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let repo_root = std::path::PathBuf::from(&repo);

    // Commit real work, so the bundle has something to carry.
    std::fs::write(path.join("kept.txt"), "survives release\n").unwrap();
    for args in [
        vec!["add", "kept.txt"],
        vec!["commit", "-m", "work worth keeping"],
    ] {
        let ok = std::process::Command::new("git")
            .args(&args)
            .current_dir(&path)
            .status()
            .unwrap();
        assert!(ok.success(), "git {args:?} failed");
    }

    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/archive"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert!(!path.exists(), "archive removes the checkout");
    let released_body: serde_json::Value = archived.json().await.unwrap();
    assert_eq!(released_body["status"], "released");
    assert!(released_body["bundle_bytes"].as_i64().unwrap() > 0);
    assert!(released_body["released_tip"].as_str().is_some());
    assert!(
        !branch_exists_in(&repo_root, &branch),
        "archive drops the branch"
    );

    let restored = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/restore"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        restored.status(),
        reqwest::StatusCode::OK,
        "restore from bundle failed: {}",
        restored.text().await.unwrap()
    );
    let restored_body: serde_json::Value = restored.json().await.unwrap();
    assert_eq!(restored_body["status"], "active");
    assert!(restored_body["released_at"].is_null());
    // Normalize: git checks out with CRLF under Windows' default
    // `core.autocrlf`.
    assert_eq!(
        std::fs::read_to_string(path.join("kept.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "survives release\n",
        "the released commit did not come back"
    );
}

/// Reclaim deletes only a checkout Tidebreak made.
///
/// A registered repository is a directory the user already had, and the clone
/// parent is a setting that moves, so there is no path test that stays
/// honest. The recorded origin is the whole guard: without it this route is a
/// recursive delete pointed at someone's own work.
#[tokio::test]
async fn reclaim_refuses_a_checkout_tidebreak_did_not_clone() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let repo_id = json_id(&repo_body);
    let root = std::path::PathBuf::from(&repo);

    let refused = client
        .delete(format!(
            "http://{addr}/code/repos/{repo_id}?reclaim_checkout=true"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "checkout_not_reclaimable");
    assert!(
        root.join(".git").exists(),
        "a registered checkout must survive a refused reclaim"
    );

    // The registration still goes away; only the directory is spared.
    let removed = client
        .delete(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(
        root.join(".git").exists(),
        "removal must not delete the user's checkout"
    );
    let listed = client
        .get(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert!(listed.is_empty(), "a removed registration leaves the list");
}
