//! Pull-request reads and creation over the forge REST API (decision 65).
//!
//! A gateway-authenticated hosted machine has no `gh`. Where decision 63 gave
//! its git transport a borrowed credential, this module gives its
//! pull-request surfaces the same: each function drives one operation against
//! the forge's REST API with a per-operation borrowed credential, and shapes
//! every answer exactly like the `gh --json` payload code mode already
//! parses — one JSON vocabulary, however the host was asked.
//!
//! Deliberately narrow: creation, the PR-card digest, the fact reads
//! (decision 62), and Delivery's repository-scoped reads and actions. Auto-merge
//! (and merge-queue enqueue) rides one pinned GitHub mutation because the
//! REST merge endpoint cannot arm it. Mark-ready and admin merge stay
//! explicit refusals. Host stacks ride the same generic GET the rest of
//! Delivery uses.

use std::time::Duration;

use futures::{stream, StreamExt as _};
use serde_json::Value;

use crate::obo_gateway::GitCredential;
use crate::routes::code::types::CodeGitHubRepositoryTarget;
use tidebreak_core::{PullRequestCheck, PullRequestCheckBucket, PullRequestDigest};

/// Timeout for one REST call — the same bound the `gh` runner gets.
const REST_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on one REST response body. PR and check payloads are kilobytes; the
/// bound exists so a confused origin cannot grow the process.
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

/// Pinned mutation that arms auto-merge, or enqueues when the repository
/// uses a merge queue. Not a general GraphQL runner.
const ENABLE_AUTO_MERGE_MUTATION: &str = "\
mutation($id: ID!, $oid: GitObjectID!, $method: PullRequestMergeMethod!) {\
  enablePullRequestAutoMerge(input: {\
    pullRequestId: $id, expectedHeadOid: $oid, mergeMethod: $method\
  }) { pullRequest { number } }\
}";

/// Bound the check-run fan-out within one repository list read.
const CHECK_RUN_CONCURRENCY: usize = 4;

/// The REST base for a forge host: `api.github.com` for github.com, the
/// GHES `/api/v3/` convention for anything else.
///
/// Today the lending gate pins the host to github.com before any credential
/// exists, so the second arm is spelled for honesty, not reach.
pub(crate) fn default_api_base(host: &str) -> String {
    if host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com") {
        "https://api.github.com".to_owned()
    } else {
        format!("https://{host}/api/v3")
    }
}

/// GraphQL origin for the same forge `api_base` REST talks to.
fn graphql_url(api_base: &str) -> String {
    match api_base.strip_suffix("/api/v3") {
        Some(root) => format!("{root}/api/graphql"),
        None => format!("{api_base}/graphql"),
    }
}

/// One authenticated REST call, bounded and JSON-decoded.
///
/// Errors are strings for the caller to wrap: this module does not know
/// whether it is failing a create, a status read, or a fact sweep.
async fn request(
    method: reqwest::Method,
    url: String,
    credential: &GitCredential,
    body: Option<&Value>,
) -> Result<(reqwest::StatusCode, Value), String> {
    let client = reqwest::Client::builder()
        .timeout(REST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("tidebreak/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("the forge REST client could not be built: {error}"))?;
    let mut request = client
        .request(method, url)
        .bearer_auth(&credential.secret)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28");
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("the forge could not be reached: {error}"))?;
    let status = response.status();
    let mut bytes = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| format!("the forge response broke mid-read: {error}"))?;
        if bytes.len() + chunk.len() > RESPONSE_LIMIT {
            return Err("the forge response passed the size ceiling".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok((status, value))
}

/// The forge's own message for a refused call, bounded, or the status alone.
fn forge_message(status: reqwest::StatusCode, value: &Value) -> String {
    let detail = value
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or_default();
    if detail.is_empty() {
        format!("the forge answered {status}")
    } else {
        let mut bounded = detail.chars().take(300).collect::<String>();
        if bounded.len() < detail.len() {
            bounded.push('…');
        }
        bounded
    }
}

/// Read one repository in the same REST shape that `gh api repos/...`
/// returns. Delivery turns this into its repository reference.
pub(crate) async fn repository(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
) -> Result<Value, String> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!("{api_base}/repos/{}/{}", target.owner, target.name),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(value)
}

/// One authenticated GET on a repository-scoped endpoint, answered in the
/// same REST shape `gh api <endpoint>` returns.
///
/// The detail drawers already speak endpoint strings; this lets them ask the
/// forge directly when no `gh` exists.
pub(crate) async fn api_get(
    api_base: &str,
    credential: &GitCredential,
    endpoint: &str,
) -> Result<Value, String> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!("{api_base}/{endpoint}"),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(value)
}

/// Read one pull request in the `gh pr view --json` vocabulary, checks
/// included — the single-row read the by-number sweep and the detail
/// drawer share.
pub(crate) async fn delivery_pull_request(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    number: u64,
) -> Result<Value, String> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/pulls/{number}",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    let mut fact = fact_value(&value);
    let checks = match value.pointer("/head/sha").and_then(Value::as_str) {
        Some(sha) => check_run_values(api_base, target, credential, sha).await?,
        None => Vec::new(),
    };
    fact.as_object_mut()
        .expect("fact values are objects")
        .insert("statusCheckRollup".to_owned(), Value::Array(checks));
    Ok(fact)
}

/// List one repository's pull requests in the `gh pr list --json`
/// vocabulary that Delivery already parses.
///
/// GitHub REST has no `merged` list state. A merged query reads closed pull
/// requests and lets Delivery's existing `mergedAt` filter select the rows.
/// When checks are requested, each pull request carries an explicit
/// `statusCheckRollup`, including an empty array for a head with no runs.
pub(crate) async fn delivery_pull_requests(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    state: &str,
    checks_loaded: bool,
) -> Result<Vec<Value>, String> {
    let state = if state == "merged" { "closed" } else { state };
    let (status, value) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/pulls?state={state}&per_page=100",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    let pulls = value.as_array().cloned().unwrap_or_default();
    stream::iter(pulls)
        .map(|pull| async move {
            let mut fact = fact_value(&pull);
            if checks_loaded {
                let checks = match pull.pointer("/head/sha").and_then(Value::as_str) {
                    Some(sha) => check_run_values(api_base, target, credential, sha).await?,
                    None => Vec::new(),
                };
                fact.as_object_mut()
                    .expect("fact values are objects")
                    .insert("statusCheckRollup".to_owned(), Value::Array(checks));
            }
            Ok(fact)
        })
        .buffered(CHECK_RUN_CONCURRENCY)
        .collect::<Vec<Result<Value, String>>>()
        .await
        .into_iter()
        .collect()
}

/// Read one repository's GitHub Actions runs.
pub(crate) async fn workflow_runs(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
) -> Result<Value, String> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/actions/runs?per_page=100",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(value)
}

/// Read one repository's deployments.
pub(crate) async fn deployments(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
) -> Result<Value, String> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/deployments?per_page=100",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(value)
}

/// Create a pull request and return it in the fact shape.
pub(crate) async fn create_pull_request(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    title: &str,
    body: &str,
    base: &str,
    head: &str,
) -> Result<Value, String> {
    let (status, value) = request(
        reqwest::Method::POST,
        format!("{api_base}/repos/{}/{}/pulls", target.owner, target.name),
        credential,
        Some(&serde_json::json!({
            "title": title,
            "body": body,
            "base": base,
            "head": head,
        })),
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(fact_value(&value))
}

/// Register a stack of pull requests (GitHub stacked pull requests), from the
/// chain's numbers, bottom to top.
pub(crate) async fn create_stack(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    numbers: &[u64],
) -> Result<(), String> {
    let (status, value) = request(
        reqwest::Method::POST,
        format!("{api_base}/repos/{}/{}/stacks", target.owner, target.name),
        credential,
        Some(&serde_json::json!({ "pull_requests": numbers })),
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(())
}

/// Merge one pull request only when its head still matches the reviewed SHA.
pub(crate) async fn merge_pull_request(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    number: u64,
    method: &str,
    expected_head_sha: &str,
) -> Result<(), String> {
    let (status, value) = request(
        reqwest::Method::PUT,
        format!(
            "{api_base}/repos/{}/{}/pulls/{number}/merge",
            target.owner, target.name
        ),
        credential,
        Some(&serde_json::json!({
            "sha": expected_head_sha,
            "merge_method": method,
        })),
    )
    .await?;
    if !status.is_success() || value.get("merged").and_then(Value::as_bool) != Some(true) {
        return Err(forge_message(status, &value));
    }
    Ok(())
}

/// Arm auto-merge for one pull request, or add it to the merge queue when
/// the repository uses one. REST cannot do this; GitHub only exposes the
/// transition as [`ENABLE_AUTO_MERGE_MUTATION`].
pub(crate) async fn enable_pull_request_auto_merge(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    number: u64,
    method: &str,
    expected_head_sha: &str,
) -> Result<(), String> {
    let (status, pull) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/pulls/{number}",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &pull));
    }
    let node_id = pull
        .get("node_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "the forge pull request did not name a node id".to_owned())?;
    let merge_method = match method {
        "squash" => "SQUASH",
        "rebase" => "REBASE",
        _ => "MERGE",
    };
    let body = serde_json::json!({
        "query": ENABLE_AUTO_MERGE_MUTATION,
        "variables": {
            "id": node_id,
            "oid": expected_head_sha,
            "method": merge_method,
        }
    });
    let (status, value) = request(
        reqwest::Method::POST,
        graphql_url(api_base),
        credential,
        Some(&body),
    )
    .await?;
    if !status.is_success()
        || value
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        || value
            .pointer("/data/enablePullRequestAutoMerge")
            .is_none_or(Value::is_null)
    {
        return Err(forge_message(status, &value));
    }
    Ok(())
}

/// Close or reopen one pull request through the repository-pinned endpoint.
pub(crate) async fn update_pull_request_state(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    number: u64,
    state: &str,
) -> Result<(), String> {
    let (status, value) = request(
        reqwest::Method::PATCH,
        format!(
            "{api_base}/repos/{}/{}/pulls/{number}",
            target.owner, target.name
        ),
        credential,
        Some(&serde_json::json!({ "state": state })),
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(())
}

/// Post one issue comment on a pull request.
pub(crate) async fn comment_on_pull_request(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    number: u64,
    body: &str,
) -> Result<(), String> {
    let (status, value) = request(
        reqwest::Method::POST,
        format!(
            "{api_base}/repos/{}/{}/issues/{number}/comments",
            target.owner, target.name
        ),
        credential,
        Some(&serde_json::json!({ "body": body })),
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(())
}

/// Re-run every job in one GitHub Actions workflow run.
pub(crate) async fn rerun_workflow(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    run_id: u64,
) -> Result<(), String> {
    rerun(api_base, target, credential, run_id, "rerun").await
}

/// Re-run failed jobs and their dependent jobs in one workflow run.
pub(crate) async fn rerun_failed_jobs(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    run_id: u64,
) -> Result<(), String> {
    rerun(api_base, target, credential, run_id, "rerun-failed-jobs").await
}

async fn rerun(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    run_id: u64,
    action: &str,
) -> Result<(), String> {
    let (status, value) = request(
        reqwest::Method::POST,
        format!(
            "{api_base}/repos/{}/{}/actions/runs/{run_id}/{action}",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(())
}

/// List the pull requests whose head is one branch, in the fact shape.
///
/// `state=all` so a push confirmed just after a merge still resolves; the
/// caller picks among the handful of results.
pub(crate) async fn list_pull_requests_for_head(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    head_branch: &str,
) -> Result<Vec<Value>, String> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/pulls?head={}:{}&state=all&per_page=5",
            target.owner, target.name, target.owner, head_branch
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    Ok(value
        .as_array()
        .map(|values| values.iter().map(fact_value).collect())
        .unwrap_or_default())
}

/// The PR-card digest for one branch: the branch's current pull request,
/// its checks keyed to the head it names, and its merge-queue state.
///
/// Simpler than the `gh` loader's verify-head dance on purpose: REST checks
/// are read for the exact head SHA the pull request named, so the two cannot
/// disagree the way two implicit `gh` resolutions can.
pub(crate) async fn pull_request_digest(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    head_branch: &str,
) -> Result<Option<PullRequestDigest>, String> {
    let (status, listed) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/pulls?head={}:{}&state=all&per_page=10",
            target.owner, target.name, target.owner, head_branch
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &listed));
    }
    let empty = Vec::new();
    let listed = listed.as_array().unwrap_or(&empty);
    // The branch's current pull request: the open one when it exists, and
    // the newest closed one otherwise — the same answer `gh pr view`
    // resolves a branch to.
    let current = listed
        .iter()
        .find(|value| value.get("state").and_then(Value::as_str) == Some("open"))
        .or_else(|| listed.first());
    let Some(number) = current
        .and_then(|value| value.get("number"))
        .and_then(Value::as_u64)
    else {
        return Ok(None);
    };
    let (status, detail) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/pulls/{number}",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &detail));
    }
    let head_sha = detail
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let checks = match &head_sha {
        Some(sha) => check_runs(api_base, target, credential, sha).await?,
        None => Vec::new(),
    };
    let state = if detail.get("merged").and_then(Value::as_bool) == Some(true) {
        "merged".to_owned()
    } else {
        detail
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("open")
            .to_ascii_lowercase()
    };
    let open = state == "open";
    let in_merge_queue = if open {
        merge_queue_state(api_base, target, credential, number).await
    } else {
        Some(false)
    };
    let text = |pointer: &str| {
        detail
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    Ok(Some(PullRequestDigest {
        number,
        url: text("/html_url"),
        merged: Some(state == "merged"),
        state,
        title: text("/title"),
        checks_summary: super::gh::summarize_checks(&checks),
        checks: (!checks.is_empty()).then_some(checks),
        draft: detail.get("draft").and_then(Value::as_bool),
        // REST has no review-decision projection; the field stays unstated
        // rather than approximated from raw reviews.
        review_decision: None,
        mergeable: match detail.get("mergeable") {
            Some(Value::Bool(true)) => Some("mergeable".to_owned()),
            Some(Value::Bool(false)) => Some("conflicting".to_owned()),
            _ => None,
        },
        merge_state_status: text("/mergeable_state"),
        head_branch: text("/head/ref"),
        base_branch: text("/base/ref"),
        head_sha,
        auto_merge_enabled: Some(
            detail
                .get("auto_merge")
                .is_some_and(|value| !value.is_null()),
        ),
        in_merge_queue,
    }))
}

/// One head's check runs, bucketed exactly as the `gh` table parser buckets.
async fn check_runs(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    sha: &str,
) -> Result<Vec<PullRequestCheck>, String> {
    Ok(check_run_values(api_base, target, credential, sha)
        .await?
        .iter()
        .filter_map(|run| {
            let name = run.get("name").and_then(Value::as_str)?.to_owned();
            let conclusion = run.get("conclusion").and_then(Value::as_str);
            let bucket = match conclusion {
                Some("success") => PullRequestCheckBucket::Pass,
                Some("neutral" | "cancelled" | "skipped") => PullRequestCheckBucket::Skipped,
                Some(_) => PullRequestCheckBucket::Fail,
                None => PullRequestCheckBucket::Pending,
            };
            let detail = conclusion
                .or_else(|| run.get("status").and_then(Value::as_str))
                .map(ToOwned::to_owned);
            let url = run
                .get("detailsUrl")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(PullRequestCheck {
                name,
                bucket,
                detail,
                url,
            })
        })
        .collect())
}

/// One head's check runs in the `statusCheckRollup` shape that Delivery
/// already parses from `gh pr list --json`.
async fn check_run_values(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    sha: &str,
) -> Result<Vec<Value>, String> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/commits/{sha}/check-runs?per_page=100",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(forge_message(status, &value));
    }
    let empty = Vec::new();
    let runs = value
        .get("check_runs")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    Ok(runs
        .iter()
        .filter_map(|run| {
            let name = run.get("name").and_then(Value::as_str)?;
            let conclusion = run.get("conclusion").cloned().unwrap_or(Value::Null);
            let pending_state = if conclusion.is_null() {
                Value::String("PENDING".to_owned())
            } else {
                Value::Null
            };
            let details_url = run
                .get("details_url")
                .filter(|value| value.as_str().is_some())
                .or_else(|| run.get("html_url"))
                .cloned()
                .unwrap_or(Value::Null);
            Some(serde_json::json!({
                "name": name,
                "status": run.get("status").cloned().unwrap_or(Value::Null),
                "state": pending_state,
                "conclusion": conclusion,
                "detailsUrl": details_url,
            }))
        })
        .collect())
}

/// Whether the pull request currently sits in a merge queue, from the same
/// timeline events the `gh` loader reads. `None` when the read fails — the
/// digest states nothing rather than guessing.
pub(crate) async fn merge_queue_state(
    api_base: &str,
    target: &CodeGitHubRepositoryTarget,
    credential: &GitCredential,
    number: u64,
) -> Option<bool> {
    let (status, value) = request(
        reqwest::Method::GET,
        format!(
            "{api_base}/repos/{}/{}/issues/{number}/timeline?per_page=100",
            target.owner, target.name
        ),
        credential,
        None,
    )
    .await
    .ok()?;
    if !status.is_success() {
        return None;
    }
    queue_membership_from_timeline(&value)
}

/// Latest queue transition wins: added after removed is in, the reverse is
/// out. An empty timeline is out, not unknown.
pub(crate) fn queue_membership_from_timeline(value: &Value) -> Option<bool> {
    let last = value.as_array()?.iter().rev().find_map(|event| {
        match event.get("event").and_then(Value::as_str) {
            Some(event @ ("added_to_merge_queue" | "removed_from_merge_queue")) => Some(event),
            _ => None,
        }
    });
    Some(last == Some("added_to_merge_queue"))
}

/// One REST pull request restated in the `gh --json` fact shape
/// ([`super::gh::PR_FACT_FIELDS`]), so the fact store parses one vocabulary
/// however the host was asked. `state` stays REST's own `open`/`closed`;
/// the parser already reads closed-with-merged-at as merged.
pub(crate) fn fact_value(pr: &Value) -> Value {
    let head_repository = pr.pointer("/head/repo").map_or(Value::Null, |repository| {
        serde_json::json!({
            "nameWithOwner": repository.get("full_name").cloned().unwrap_or(Value::Null),
            "name": repository.get("name").cloned().unwrap_or(Value::Null),
        })
    });
    let head_repository_owner = pr
        .pointer("/head/repo/owner/login")
        .cloned()
        .map_or(Value::Null, |login| serde_json::json!({ "login": login }));
    let mergeable = match pr.get("mergeable") {
        Some(Value::Bool(true)) => Value::String("MERGEABLE".to_owned()),
        Some(Value::Bool(false)) => Value::String("CONFLICTING".to_owned()),
        _ => Value::Null,
    };
    serde_json::json!({
        "number": pr.get("number").cloned().unwrap_or(Value::Null),
        "url": pr.get("html_url").cloned().unwrap_or(Value::Null),
        "title": pr.get("title").cloned().unwrap_or(Value::Null),
        "state": pr.get("state").cloned().unwrap_or(Value::Null),
        "isDraft": pr.get("draft").cloned().unwrap_or(Value::Null),
        "author": {
            "login": pr.pointer("/user/login").cloned().unwrap_or(Value::Null),
            "avatarUrl": pr.pointer("/user/avatar_url").cloned().unwrap_or(Value::Null),
        },
        "reviewDecision": Value::Null,
        "mergeable": mergeable,
        "mergeStateStatus": pr.get("mergeable_state").cloned().unwrap_or(Value::Null),
        "autoMergeRequest": pr.get("auto_merge").cloned().unwrap_or(Value::Null),
        "headRepository": head_repository,
        "headRepositoryOwner": head_repository_owner,
        "headRefName": pr.pointer("/head/ref").cloned().unwrap_or(Value::Null),
        "headRefOid": pr.pointer("/head/sha").cloned().unwrap_or(Value::Null),
        "baseRefName": pr.pointer("/base/ref").cloned().unwrap_or(Value::Null),
        "createdAt": pr.get("created_at").cloned().unwrap_or(Value::Null),
        "updatedAt": pr.get("updated_at").cloned().unwrap_or(Value::Null),
        "mergedAt": pr.get("merged_at").cloned().unwrap_or(Value::Null),
        "closedAt": pr.get("closed_at").cloned().unwrap_or(Value::Null),
        "labels": pr.get("labels").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
        "comments": pr.get("comments").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The REST shape lands byte-compatible with the `gh --json` vocabulary
    /// the fact parser reads (decision 62): a merged REST answer must read
    /// as merged through `closed` + `mergedAt`.
    #[test]
    fn a_rest_pull_request_restates_as_the_gh_fact_shape() {
        let rest = serde_json::json!({
            "number": 7,
            "html_url": "https://github.com/acme/demo/pull/7",
            "title": "Add the thing",
            "state": "closed",
            "draft": false,
            "user": { "login": "mira-chen" },
            "head": { "ref": "feature", "sha": "abc123" },
            "base": { "ref": "main" },
            "created_at": "2026-08-24T10:00:00Z",
            "updated_at": "2026-08-24T11:00:00Z",
            "merged_at": "2026-08-24T11:00:00Z",
            "closed_at": "2026-08-24T11:00:00Z",
        });
        let fact = fact_value(&rest);
        assert_eq!(fact["number"], 7);
        assert_eq!(fact["url"], "https://github.com/acme/demo/pull/7");
        assert_eq!(fact["state"], "closed");
        assert_eq!(fact["mergedAt"], "2026-08-24T11:00:00Z");
        assert_eq!(fact["author"]["login"], "mira-chen");
        assert_eq!(fact["headRefName"], "feature");
        assert_eq!(fact["headRefOid"], "abc123");
    }

    /// github.com maps to the public API origin; any other forge host keeps
    /// the GHES convention.
    #[test]
    fn the_api_base_follows_the_forge_host() {
        assert_eq!(default_api_base("github.com"), "https://api.github.com");
        assert_eq!(default_api_base("www.github.com"), "https://api.github.com");
        assert_eq!(
            default_api_base("ghe.acme.test"),
            "https://ghe.acme.test/api/v3"
        );
    }

    #[test]
    fn the_graphql_origin_follows_the_rest_base() {
        assert_eq!(
            graphql_url("https://api.github.com"),
            "https://api.github.com/graphql"
        );
        assert_eq!(
            graphql_url("https://ghe.acme.test/api/v3"),
            "https://ghe.acme.test/api/graphql"
        );
    }

    #[test]
    fn merge_queue_membership_follows_the_latest_timeline_event() {
        assert_eq!(
            queue_membership_from_timeline(&serde_json::json!([
                { "event": "added_to_merge_queue" }
            ])),
            Some(true)
        );
        assert_eq!(
            queue_membership_from_timeline(&serde_json::json!([
                { "event": "added_to_merge_queue" },
                { "event": "removed_from_merge_queue" }
            ])),
            Some(false)
        );
        assert_eq!(
            queue_membership_from_timeline(&serde_json::json!([
                { "event": "removed_from_merge_queue" },
                { "event": "committed" },
                { "event": "added_to_merge_queue" }
            ])),
            Some(true)
        );
        assert_eq!(
            queue_membership_from_timeline(&serde_json::json!([{ "event": "committed" }])),
            Some(false)
        );
        assert_eq!(queue_membership_from_timeline(&Value::Null), None);
    }
}
