//! One conditional REST fetcher for pull-request state (decision 66).
//!
//! Every background pull-request read runs through here: one read of the
//! pull request, one of its head's check runs, one of its reviews, and —
//! only for base branches observed to run a merge queue — one timeline
//! read for queue membership. Each read sends the stored ETag, so an
//! unchanged answer is a 304 that GitHub does not count against the
//! primary rate limit: sustained cost tracks how often pull requests
//! change, not how often Tidebreak asks.
//!
//! The reads ride one of two transports ([`FetchTransport`]): `gh api` on
//! a machine with an authenticated `gh`, or the forge REST API with a
//! borrowed credential on a gateway-hosted machine that has none
//! (decision 65). Both answer in one [`RawHttpResponse`] shape, so the
//! conditional discipline is a single code path however the host is asked.
//!
//! One [`HostGate`] paces every call. It holds a small global concurrency,
//! spaces requests per host, and treats a secondary-rate-limit answer or a
//! `Retry-After` header as an order: the host parks for the stated window
//! and every caller respects the park rather than retrying hot.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::obo_gateway::GitCredential;
use tidebreak_core::{
    CodePullRequestFact, CodePullRequestState, PullRequestCheck, PullRequestCheckBucket,
    PullRequestDigest,
};

use super::gh::{run_gh, run_gh_http, summarize_checks, RawHttpResponse};

/// Ceiling on concurrent GitHub reads through the gate, across every host.
const GATE_PERMITS: usize = 4;
/// Minimum spacing between two gated reads of the same host.
const HOST_SPACING: Duration = Duration::from_millis(250);
/// Park applied when a limit answer names no `Retry-After`.
const DEFAULT_PARK: Duration = Duration::from_secs(60);
/// Ceiling on any park, so one hostile header cannot silence a host for
/// hours.
const MAX_PARK: Duration = Duration::from_secs(15 * 60);
/// Bound on one `gh api` call — the same bound the general runner uses.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on one direct REST response body — the same ceiling the hosted forge
/// reader applies, so a confused origin cannot grow the process.
const REST_RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

/// Ceiling on timeline pages one membership read walks. The timeline pages
/// oldest-first and membership is the last queue transition, so the walk
/// must reach the end; past this depth the read states nothing rather than
/// reporting an older transition as current.
const TIMELINE_PAGE_CAP: u32 = 30;

/// How one gated read reaches the forge: `gh api` where an authenticated
/// `gh` exists, or the REST API with a borrowed per-operation credential on
/// a gateway-hosted machine that has none (decision 65). Either way the
/// answer is the same [`RawHttpResponse`], so ETags, 304s, and parks are
/// one discipline.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FetchTransport<'a> {
    /// `gh api --include` run in `cwd` on `binary`.
    Gh { cwd: &'a Path, binary: &'a Path },
    /// A direct conditional GET against `api_base` with a borrowed
    /// credential as the bearer.
    Rest {
        api_base: &'a str,
        credential: &'a GitCredential,
    },
}

/// Pace and park GitHub reads, one state per host (decision 66).
#[derive(Debug)]
pub(crate) struct HostGate {
    permits: Arc<tokio::sync::Semaphore>,
    hosts: Mutex<HashMap<String, HostState>>,
}

#[derive(Debug, Default)]
struct HostState {
    parked_until: Option<tokio::time::Instant>,
    next_slot: Option<tokio::time::Instant>,
}

impl Default for HostGate {
    fn default() -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(GATE_PERMITS)),
            hosts: Mutex::new(HashMap::new()),
        }
    }
}

impl HostGate {
    /// Admission to speak to `host`: waits out the per-host spacing, then
    /// takes a global permit. `Err` carries how much park remains — the
    /// caller skips its read and keeps whatever state it already holds.
    pub(crate) async fn admit(&self, host: &str) -> Result<GatePermit, Duration> {
        let slot = {
            let mut hosts = self.hosts.lock().expect("host gate");
            let state = hosts.entry(host.to_ascii_lowercase()).or_default();
            let now = tokio::time::Instant::now();
            if let Some(until) = state.parked_until {
                if until > now {
                    return Err(until - now);
                }
                state.parked_until = None;
            }
            let slot = state.next_slot.map_or(now, |next| next.max(now));
            state.next_slot = Some(slot + HOST_SPACING);
            slot
        };
        tokio::time::sleep_until(slot).await;
        {
            let hosts = self.hosts.lock().expect("host gate");
            if let Some(state) = hosts.get(&host.to_ascii_lowercase()) {
                if let Some(until) = state.parked_until {
                    let now = tokio::time::Instant::now();
                    if until > now {
                        return Err(until - now);
                    }
                }
            }
        }
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| Duration::ZERO)?;
        Ok(GatePermit { _permit: permit })
    }

    /// Park every read of `host` for `duration`, capped at [`MAX_PARK`].
    pub(crate) fn park(&self, host: &str, duration: Duration) {
        let duration = duration.min(MAX_PARK);
        let until = tokio::time::Instant::now() + duration;
        let mut hosts = self.hosts.lock().expect("host gate");
        let state = hosts.entry(host.to_ascii_lowercase()).or_default();
        if state.parked_until.is_none_or(|current| current < until) {
            state.parked_until = Some(until);
        }
    }
}

/// Held for the duration of one gated read.
#[derive(Debug)]
pub(crate) struct GatePermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Why a gated read produced nothing.
#[derive(Debug)]
pub(crate) enum FetchFailure {
    /// The host is parked; retry after the carried duration.
    Parked(Duration),
    /// The host answered with a status the caller cannot act on.
    Refused(u16, String),
    /// The subprocess itself failed: spawn, timeout, or no HTTP answer.
    Transport(String),
}

impl std::fmt::Display for FetchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parked(remaining) => {
                write!(f, "the host is parked for {}s", remaining.as_secs())
            }
            Self::Refused(status, message) => write!(f, "the host answered {status}: {message}"),
            Self::Transport(message) => write!(f, "{message}"),
        }
    }
}

/// One conditional endpoint read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EndpointRead<T> {
    /// A 200 with new state and the ETag to send next time.
    Fresh { value: T, etag: Option<String> },
    /// A 304: whatever the caller stored still stands.
    NotModified,
    /// A 404: the resource is gone or never existed.
    Missing,
}

/// The digest-relevant fields of one REST pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestPull {
    pub number: u64,
    pub url: Option<String>,
    pub state: String,
    pub title: Option<String>,
    pub draft: Option<bool>,
    pub mergeable: Option<String>,
    pub merge_state_status: Option<String>,
    pub head_branch: Option<String>,
    pub base_branch: Option<String>,
    pub head_sha: Option<String>,
    pub auto_merge_enabled: Option<bool>,
}

/// What one review aggregation says, in the lowercased vocabulary
/// `gh pr view --json reviewDecision` used to supply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewTally {
    pub approvals: u64,
    pub changes_requested: u64,
}

/// What the base branch's rules say about merging (decision 66): whether
/// reviews are required, and whether a merge queue runs — the one signal
/// that decides if the timeline read is worth paying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BranchRules {
    pub required_approvals: u64,
    pub has_merge_queue: bool,
}

/// GET `repos/{owner}/{name}/pulls/{number}`, conditionally.
pub(crate) async fn read_pull_request(
    gate: &HostGate,
    transport: FetchTransport<'_>,
    host: &str,
    owner: &str,
    name: &str,
    number: u64,
    etag: Option<&str>,
) -> Result<EndpointRead<RestPull>, FetchFailure> {
    let path = format!("repos/{owner}/{name}/pulls/{number}");
    let response = gated_read(gate, transport, host, &path, etag).await?;
    match response.status {
        200 => match rest_pull_from_value(&parse_body(&response)?) {
            Some(pull) => Ok(EndpointRead::Fresh {
                value: pull,
                etag: response.etag,
            }),
            None => Err(FetchFailure::Transport(
                "the pull request answer carried no number".into(),
            )),
        },
        304 => Ok(EndpointRead::NotModified),
        404 => Ok(EndpointRead::Missing),
        status => Err(FetchFailure::Refused(status, bounded_body(&response))),
    }
}

/// GET `repos/{owner}/{name}/pulls?head=...`: the branch's current pull
/// request, when the workspace does not know one yet. The open one wins;
/// the newest closed one answers otherwise — the same resolution
/// `gh pr view` applied to a branch.
pub(crate) async fn read_pull_request_for_head(
    gate: &HostGate,
    transport: FetchTransport<'_>,
    host: &str,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<Option<RestPull>, FetchFailure> {
    let path = format!("repos/{owner}/{name}/pulls?head={owner}:{branch}&state=all&per_page=10");
    let response = gated_read(gate, transport, host, &path, None).await?;
    match response.status {
        200 => {
            let listed = parse_body(&response)?;
            let empty = Vec::new();
            let listed = listed.as_array().unwrap_or(&empty);
            let current = listed
                .iter()
                .find(|value| value.get("state").and_then(Value::as_str) == Some("open"))
                .or_else(|| listed.first());
            Ok(current.and_then(rest_pull_from_value))
        }
        404 => Ok(None),
        status => Err(FetchFailure::Refused(status, bounded_body(&response))),
    }
}

/// GET `repos/{owner}/{name}/commits/{sha}/check-runs`, conditionally.
pub(crate) async fn read_check_runs(
    gate: &HostGate,
    transport: FetchTransport<'_>,
    host: &str,
    owner: &str,
    name: &str,
    sha: &str,
    etag: Option<&str>,
) -> Result<EndpointRead<Vec<PullRequestCheck>>, FetchFailure> {
    let path = format!("repos/{owner}/{name}/commits/{sha}/check-runs?per_page=100");
    let response = gated_read(gate, transport, host, &path, etag).await?;
    match response.status {
        200 => {
            let value = parse_body(&response)?;
            Ok(EndpointRead::Fresh {
                value: checks_from_value(&value),
                etag: response.etag,
            })
        }
        304 => Ok(EndpointRead::NotModified),
        404 => Ok(EndpointRead::Missing),
        status => Err(FetchFailure::Refused(status, bounded_body(&response))),
    }
}

/// GET `repos/{owner}/{name}/pulls/{number}/reviews`, conditionally.
///
/// One page of one hundred: a pull request with more reviews than that has
/// long since answered the only question this read asks.
pub(crate) async fn read_reviews(
    gate: &HostGate,
    transport: FetchTransport<'_>,
    host: &str,
    owner: &str,
    name: &str,
    number: u64,
    etag: Option<&str>,
) -> Result<EndpointRead<ReviewTally>, FetchFailure> {
    let path = format!("repos/{owner}/{name}/pulls/{number}/reviews?per_page=100");
    let response = gated_read(gate, transport, host, &path, etag).await?;
    match response.status {
        200 => {
            let value = parse_body(&response)?;
            Ok(EndpointRead::Fresh {
                value: tally_reviews(&value),
                etag: response.etag,
            })
        }
        304 => Ok(EndpointRead::NotModified),
        404 => Ok(EndpointRead::Missing),
        status => Err(FetchFailure::Refused(status, bounded_body(&response))),
    }
}

/// GET `repos/{owner}/{name}/rules/branches/{branch}`: what merging into
/// this branch requires. `Missing` on hosts that predate rulesets.
pub(crate) async fn read_branch_rules(
    gate: &HostGate,
    transport: FetchTransport<'_>,
    host: &str,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<EndpointRead<BranchRules>, FetchFailure> {
    let path = format!("repos/{owner}/{name}/rules/branches/{branch}");
    let response = gated_read(gate, transport, host, &path, None).await?;
    match response.status {
        200 => {
            let value = parse_body(&response)?;
            Ok(EndpointRead::Fresh {
                value: rules_from_value(&value),
                etag: response.etag,
            })
        }
        304 => Ok(EndpointRead::NotModified),
        404 => Ok(EndpointRead::Missing),
        status => Err(FetchFailure::Refused(status, bounded_body(&response))),
    }
}

/// Whether the pull request sits in a merge queue, from the same timeline
/// events the retired `gh` loader read. Paid only for base branches whose
/// rules run a queue. `None` when the read fails: the digest states
/// nothing rather than guessing.
pub(crate) async fn read_merge_queue_membership(
    gate: &HostGate,
    transport: FetchTransport<'_>,
    host: &str,
    owner: &str,
    name: &str,
    number: u64,
) -> Option<bool> {
    let (cwd, binary) = match transport {
        FetchTransport::Gh { cwd, binary } => (cwd, binary),
        FetchTransport::Rest { .. } => {
            // The timeline pages oldest-first and membership is the last
            // queue transition, so walk to the end the way the `gh` arm's
            // `--paginate` does. A short page is the end.
            let mut membership = None;
            for page in 1..=TIMELINE_PAGE_CAP {
                let path = format!(
                    "repos/{owner}/{name}/issues/{number}/timeline?per_page=100&page={page}"
                );
                let response = gated_read(gate, transport, host, &path, None).await.ok()?;
                if response.status != 200 {
                    return None;
                }
                let value: Value = serde_json::from_str(&response.body).ok()?;
                let events = value.as_array()?;
                if let Some(last) = events.iter().rev().find_map(|event| {
                    match event.get("event").and_then(Value::as_str) {
                        Some(event @ ("added_to_merge_queue" | "removed_from_merge_queue")) => {
                            Some(event)
                        }
                        _ => None,
                    }
                }) {
                    membership = Some(last == "added_to_merge_queue");
                }
                if events.len() < 100 {
                    return Some(membership.unwrap_or(false));
                }
            }
            // Deeper than the cap: state nothing rather than guessing.
            return None;
        }
    };
    let _permit = gate.admit(host).await.ok()?;
    let path = format!("repos/{owner}/{name}/issues/{number}/timeline?per_page=100");
    let mut args = vec!["api"];
    let hostname = host_argument(host);
    if let Some(hostname) = &hostname {
        args.extend(["--hostname", hostname.as_str()]);
    }
    args.extend([
        path.as_str(),
        "--paginate",
        "--jq",
        ".[] | select(.event == \"added_to_merge_queue\" or .event == \"removed_from_merge_queue\") | .event",
    ]);
    let events = match run_gh(cwd, binary, &args, FETCH_TIMEOUT).await {
        Ok(events) => events,
        Err(error) => {
            // The paginated read cannot carry headers, so a limit answer
            // parks the host by its text rather than by `Retry-After`.
            if error.to_ascii_lowercase().contains("rate limit") {
                gate.park(host, DEFAULT_PARK);
            }
            return None;
        }
    };
    Some(queue_membership_from_events(&events))
}

/// The latest queue transition wins: a pull request added and then removed
/// is out, whatever order history reports the rest in.
pub(crate) fn queue_membership_from_events(events: &str) -> bool {
    events
        .lines()
        .map(str::trim)
        .rfind(|event| matches!(*event, "added_to_merge_queue" | "removed_from_merge_queue"))
        == Some("added_to_merge_queue")
}

/// The `reviewDecision` word the classifier already reads, derived from the
/// branch rules and the review tally. `review_required` needs the rules to
/// actually require approvals; without rules, approvals and objections
/// still speak, and silence stays `None`.
pub(crate) fn derive_review_decision(
    rules: Option<BranchRules>,
    tally: &ReviewTally,
) -> Option<String> {
    if tally.changes_requested > 0 {
        return Some("changes_requested".to_owned());
    }
    match rules {
        Some(rules) if rules.required_approvals > 0 => {
            if tally.approvals >= rules.required_approvals {
                Some("approved".to_owned())
            } else {
                Some("review_required".to_owned())
            }
        }
        _ => (tally.approvals > 0).then(|| "approved".to_owned()),
    }
}

/// Assemble the digest one refresh produced: the pull request's own fields,
/// the checks read for its exact head, the derived review decision, and
/// queue membership when it was worth reading.
pub(crate) fn digest_from_parts(
    pull: &RestPull,
    checks: &[PullRequestCheck],
    review_decision: Option<String>,
    in_merge_queue: Option<bool>,
) -> PullRequestDigest {
    PullRequestDigest {
        number: pull.number,
        url: pull.url.clone(),
        state: pull.state.clone(),
        title: pull.title.clone(),
        checks_summary: summarize_checks(checks),
        checks: (!checks.is_empty()).then(|| checks.to_vec()),
        draft: pull.draft,
        merged: Some(pull.state == "merged"),
        review_decision,
        mergeable: pull.mergeable.clone(),
        merge_state_status: pull.merge_state_status.clone(),
        head_branch: pull.head_branch.clone(),
        base_branch: pull.base_branch.clone(),
        head_sha: pull.head_sha.clone(),
        auto_merge_enabled: pull.auto_merge_enabled,
        in_merge_queue: if pull.state == "open" {
            in_merge_queue
        } else {
            Some(false)
        },
    }
}

/// One gated conditional GET of `path` on `host`, over either transport.
async fn gated_read(
    gate: &HostGate,
    transport: FetchTransport<'_>,
    host: &str,
    path: &str,
    etag: Option<&str>,
) -> Result<RawHttpResponse, FetchFailure> {
    let _permit = gate.admit(host).await.map_err(FetchFailure::Parked)?;
    let response = match transport {
        FetchTransport::Gh { cwd, binary } => {
            let mut args = vec!["api", "--include"];
            let hostname = host_argument(host);
            if let Some(hostname) = &hostname {
                args.extend(["--hostname", hostname.as_str()]);
            }
            let conditional = etag.map(|etag| format!("If-None-Match: {etag}"));
            if let Some(conditional) = &conditional {
                args.extend(["-H", conditional.as_str()]);
            }
            args.push(path);
            run_gh_http(cwd, binary, &args, FETCH_TIMEOUT)
                .await
                .map_err(FetchFailure::Transport)?
        }
        FetchTransport::Rest {
            api_base,
            credential,
        } => rest_conditional_get(api_base, credential, path, etag).await?,
    };
    apply_limit_answer(gate, host, &response);
    Ok(response)
}

/// One conditional GET against the forge REST API with a borrowed
/// credential (decision 65), answered in the same [`RawHttpResponse`] shape
/// `gh api --include` produces — status, ETag, pacing headers, body — so
/// the caller's 304 and park handling never knows which transport ran.
async fn rest_conditional_get(
    api_base: &str,
    credential: &GitCredential,
    path: &str,
    etag: Option<&str>,
) -> Result<RawHttpResponse, FetchFailure> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("tidebreak/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| {
            FetchFailure::Transport(format!("the forge REST client could not be built: {error}"))
        })?;
    let mut request = client
        .get(format!("{api_base}/{path}"))
        .bearer_auth(&credential.secret)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28");
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let mut response = request.send().await.map_err(|error| {
        FetchFailure::Transport(format!("the forge could not be reached: {error}"))
    })?;
    let header = |response: &reqwest::Response, name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .map(ToOwned::to_owned)
    };
    let status = response.status().as_u16();
    let etag = header(&response, "etag");
    let retry_after_secs = header(&response, "retry-after").and_then(|value| value.parse().ok());
    let ratelimit_remaining =
        header(&response, "x-ratelimit-remaining").and_then(|value| value.parse().ok());
    let ratelimit_reset_epoch =
        header(&response, "x-ratelimit-reset").and_then(|value| value.parse().ok());
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        FetchFailure::Transport(format!("the forge response broke mid-read: {error}"))
    })? {
        if bytes.len() + chunk.len() > REST_RESPONSE_LIMIT {
            return Err(FetchFailure::Transport(
                "the forge response passed the size ceiling".to_owned(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(RawHttpResponse {
        status,
        etag,
        retry_after_secs,
        ratelimit_remaining,
        ratelimit_reset_epoch,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// Park the host when the answer orders it: a `Retry-After`, a secondary
/// rate limit, or an exhausted primary window.
fn apply_limit_answer(gate: &HostGate, host: &str, response: &RawHttpResponse) {
    if response.status != 403 && response.status != 429 {
        return;
    }
    if let Some(seconds) = response.retry_after_secs {
        gate.park(host, Duration::from_secs(seconds.max(1)));
        return;
    }
    if response.ratelimit_remaining == Some(0) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let wait = response
            .ratelimit_reset_epoch
            .and_then(|reset| reset.checked_sub(now))
            .unwrap_or(DEFAULT_PARK.as_secs());
        gate.park(host, Duration::from_secs(wait.max(1)));
        return;
    }
    if response.body.to_ascii_lowercase().contains("rate limit") {
        gate.park(host, DEFAULT_PARK);
    }
}

/// `gh api --hostname` argument for a non-default forge host.
fn host_argument(host: &str) -> Option<String> {
    let lowered = host.to_ascii_lowercase();
    (lowered != "github.com" && lowered != "www.github.com" && !lowered.is_empty())
        .then_some(lowered)
}

fn parse_body(response: &RawHttpResponse) -> Result<Value, FetchFailure> {
    serde_json::from_str(&response.body)
        .map_err(|err| FetchFailure::Transport(format!("the answer was not JSON: {err}")))
}

fn bounded_body(response: &RawHttpResponse) -> String {
    let message = serde_json::from_str::<Value>(&response.body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| response.body.clone());
    let mut bounded: String = message.chars().take(200).collect();
    if bounded.len() < message.len() {
        bounded.push('…');
    }
    bounded
}

/// Advance the durable fact snapshot after a fresh pull read, before its new
/// ETag is stored. A later 304 reconstructs these fields from the fact, so
/// persisting the validator without the representation it validates would
/// roll title, lifecycle, or head state back to the previous slow-sweep
/// snapshot.
pub(crate) fn apply_fresh_pull_to_fact(
    fact: &mut CodePullRequestFact,
    pull: &RestPull,
    observed_at: chrono::DateTime<chrono::Utc>,
) {
    if let Some(url) = &pull.url {
        fact.url.clone_from(url);
    }
    if let Some(title) = &pull.title {
        fact.title.clone_from(title);
    }
    if let Some(state) = CodePullRequestState::from_str(&pull.state) {
        fact.state = state;
    }
    if let Some(draft) = pull.draft {
        fact.draft = draft;
    }
    if let Some(head_branch) = &pull.head_branch {
        fact.head_branch.clone_from(head_branch);
    }
    if let Some(base_branch) = &pull.base_branch {
        fact.base_branch.clone_from(base_branch);
    }
    fact.head_sha.clone_from(&pull.head_sha);
    fact.last_seen_at = observed_at;
}

/// The same digest-relevant fields, projected from a stored fact row when a
/// 304 says nothing moved since the row was written.
pub(crate) fn rest_pull_from_fact(fact: &CodePullRequestFact) -> RestPull {
    let live = fact.live.as_ref();
    RestPull {
        number: fact.number,
        url: Some(fact.url.clone()),
        state: match fact.state {
            CodePullRequestState::Open => "open",
            CodePullRequestState::Merged => "merged",
            CodePullRequestState::Closed => "closed",
        }
        .to_owned(),
        title: Some(fact.title.clone()),
        draft: Some(fact.draft),
        mergeable: live.and_then(|live| live.mergeable.clone()),
        merge_state_status: live.and_then(|live| live.merge_state_status.clone()),
        head_branch: (!fact.head_branch.is_empty()).then(|| fact.head_branch.clone()),
        base_branch: (!fact.base_branch.is_empty()).then(|| fact.base_branch.clone()),
        head_sha: fact.head_sha.clone(),
        auto_merge_enabled: live.and_then(|live| live.auto_merge_enabled),
    }
}

/// The digest-relevant fields of one REST pull request value, in the same
/// mapping the hosted forge reader applies (`forge_rest`).
pub(crate) fn rest_pull_from_value(value: &Value) -> Option<RestPull> {
    let number = value.get("number").and_then(Value::as_u64)?;
    let text = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
    };
    let state = if value.get("merged").and_then(Value::as_bool) == Some(true)
        || value
            .get("merged_at")
            .is_some_and(|merged| !merged.is_null())
    {
        "merged".to_owned()
    } else {
        value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("open")
            .to_ascii_lowercase()
    };
    Some(RestPull {
        number,
        url: text("/html_url"),
        state,
        title: text("/title"),
        draft: value.get("draft").and_then(Value::as_bool),
        mergeable: match value.get("mergeable") {
            Some(Value::Bool(true)) => Some("mergeable".to_owned()),
            Some(Value::Bool(false)) => Some("conflicting".to_owned()),
            _ => None,
        },
        merge_state_status: text("/mergeable_state").map(|status| status.to_ascii_lowercase()),
        head_branch: text("/head/ref"),
        base_branch: text("/base/ref"),
        head_sha: text("/head/sha"),
        auto_merge_enabled: Some(
            value
                .get("auto_merge")
                .is_some_and(|armed| !armed.is_null()),
        ),
    })
}

/// One head's check runs, bucketed exactly as the hosted forge reader
/// buckets them.
fn checks_from_value(value: &Value) -> Vec<PullRequestCheck> {
    let empty = Vec::new();
    let runs = value
        .get("check_runs")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    runs.iter()
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
                .get("html_url")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(PullRequestCheck {
                name,
                bucket,
                detail,
                url,
            })
        })
        .collect()
}

/// Each reviewer's newest standing review, tallied. `COMMENTED` never moves
/// a reviewer's standing, and a dismissal clears it.
fn tally_reviews(value: &Value) -> ReviewTally {
    let empty = Vec::new();
    let reviews = value.as_array().unwrap_or(&empty);
    let mut standing: HashMap<String, &str> = HashMap::new();
    for review in reviews {
        let Some(user) = review.pointer("/user/login").and_then(Value::as_str) else {
            continue;
        };
        match review.get("state").and_then(Value::as_str) {
            Some(state @ ("APPROVED" | "CHANGES_REQUESTED")) => {
                standing.insert(user.to_owned(), state);
            }
            Some("DISMISSED") => {
                standing.remove(user);
            }
            _ => {}
        }
    }
    let approvals = standing
        .values()
        .filter(|state| **state == "APPROVED")
        .count() as u64;
    let changes_requested = standing
        .values()
        .filter(|state| **state == "CHANGES_REQUESTED")
        .count() as u64;
    ReviewTally {
        approvals,
        changes_requested,
    }
}

/// What the branch's rules require, from the rules array the endpoint
/// answers with.
fn rules_from_value(value: &Value) -> BranchRules {
    let empty = Vec::new();
    let rules = value.as_array().unwrap_or(&empty);
    let mut required_approvals = 0;
    let mut has_merge_queue = false;
    for rule in rules {
        match rule.get("type").and_then(Value::as_str) {
            Some("pull_request") => {
                required_approvals = required_approvals.max(
                    rule.pointer("/parameters/required_approving_review_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
            }
            Some("merge_queue") => {
                has_merge_queue = true;
            }
            _ => {}
        }
    }
    BranchRules {
        required_approvals,
        has_merge_queue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rest_pull_request_maps_to_digest_fields() {
        let pull = rest_pull_from_value(&serde_json::json!({
            "number": 12,
            "html_url": "https://github.com/acme/demo/pull/12",
            "state": "open",
            "title": "Add the thing",
            "draft": false,
            "mergeable": true,
            "mergeable_state": "BLOCKED",
            "head": { "ref": "feature", "sha": "abc123" },
            "base": { "ref": "main" },
            "auto_merge": { "merge_method": "squash" },
        }))
        .unwrap();
        assert_eq!(pull.number, 12);
        assert_eq!(pull.state, "open");
        assert_eq!(pull.mergeable.as_deref(), Some("mergeable"));
        assert_eq!(pull.merge_state_status.as_deref(), Some("blocked"));
        assert_eq!(pull.head_sha.as_deref(), Some("abc123"));
        assert_eq!(pull.auto_merge_enabled, Some(true));

        let merged = rest_pull_from_value(&serde_json::json!({
            "number": 12,
            "state": "closed",
            "merged_at": "2026-08-24T11:00:00Z",
        }))
        .unwrap();
        assert_eq!(merged.state, "merged");
    }

    #[test]
    fn a_304_must_reuse_the_snapshot_advanced_by_the_fresh_pull() {
        let now = chrono::Utc::now();
        let mut fact = CodePullRequestFact {
            id: tidebreak_core::CodePullRequestId::new(),
            owner: tidebreak_core::OwnerId::local(),
            host: "github.com".into(),
            repo_owner: "acme".into(),
            repo_name: "demo".into(),
            number: 12,
            url: "https://github.com/acme/demo/pull/12".into(),
            title: "Stale title".into(),
            state: CodePullRequestState::Open,
            draft: false,
            author: None,
            head_branch: "feature".into(),
            base_branch: "main".into(),
            head_sha: Some("deadbeef".into()),
            created_at: now,
            updated_at: now,
            merged_at: None,
            closed_at: None,
            first_seen_at: now,
            last_seen_at: now,
            live: None,
        };
        let fresh = rest_pull_from_value(&serde_json::json!({
            "number": 12,
            "html_url": "https://github.com/acme/demo/pull/12",
            "state": "open",
            "title": "Fresh title",
            "draft": false,
            "head": { "ref": "feature", "sha": "feedfeed" },
            "base": { "ref": "main" },
        }))
        .unwrap();

        apply_fresh_pull_to_fact(&mut fact, &fresh, now);

        let after_304 = rest_pull_from_fact(&fact);
        assert_eq!(
            after_304.title, fresh.title,
            "a 304 must not roll the title back to the pre-200 fact snapshot"
        );
        assert_eq!(
            after_304.head_sha, fresh.head_sha,
            "a 304 must not reuse the pre-200 head SHA"
        );
    }

    #[test]
    fn review_standing_keeps_each_reviewers_newest_word() {
        let tally = tally_reviews(&serde_json::json!([
            { "user": { "login": "ada" }, "state": "CHANGES_REQUESTED" },
            { "user": { "login": "ada" }, "state": "APPROVED" },
            { "user": { "login": "brin" }, "state": "APPROVED" },
            { "user": { "login": "brin" }, "state": "DISMISSED" },
            { "user": { "login": "cody" }, "state": "COMMENTED" },
        ]));
        assert_eq!(
            tally,
            ReviewTally {
                approvals: 1,
                changes_requested: 0,
            }
        );
    }

    #[test]
    fn review_decision_speaks_the_classifier_vocabulary() {
        let rules = Some(BranchRules {
            required_approvals: 1,
            has_merge_queue: false,
        });
        let none = ReviewTally {
            approvals: 0,
            changes_requested: 0,
        };
        let approved = ReviewTally {
            approvals: 1,
            changes_requested: 0,
        };
        let objected = ReviewTally {
            approvals: 2,
            changes_requested: 1,
        };
        assert_eq!(
            derive_review_decision(rules, &none).as_deref(),
            Some("review_required")
        );
        assert_eq!(
            derive_review_decision(rules, &approved).as_deref(),
            Some("approved")
        );
        assert_eq!(
            derive_review_decision(rules, &objected).as_deref(),
            Some("changes_requested")
        );
        assert_eq!(derive_review_decision(None, &none), None);
        assert_eq!(
            derive_review_decision(None, &approved).as_deref(),
            Some("approved")
        );
    }

    #[test]
    fn queue_membership_follows_the_latest_transition() {
        assert!(queue_membership_from_events("added_to_merge_queue\n"));
        assert!(!queue_membership_from_events(
            "added_to_merge_queue\nremoved_from_merge_queue\n"
        ));
        assert!(queue_membership_from_events(
            "removed_from_merge_queue\nadded_to_merge_queue\n"
        ));
        assert!(!queue_membership_from_events(""));
    }

    #[test]
    fn branch_rules_read_reviews_and_queue() {
        let rules = rules_from_value(&serde_json::json!([
            { "type": "pull_request", "parameters": { "required_approving_review_count": 2 } },
            { "type": "merge_queue", "parameters": {} },
            { "type": "deletion" },
        ]));
        assert_eq!(rules.required_approvals, 2);
        assert!(rules.has_merge_queue);
        let bare = rules_from_value(&serde_json::json!([]));
        assert_eq!(bare.required_approvals, 0);
        assert!(!bare.has_merge_queue);
    }

    #[tokio::test(start_paused = true)]
    async fn a_park_refuses_reads_until_it_lapses() {
        let gate = HostGate::default();
        gate.park("github.com", Duration::from_secs(30));
        let refused = gate.admit("github.com").await;
        assert!(matches!(refused, Err(remaining) if remaining.as_secs() > 0));
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(gate.admit("github.com").await.is_ok());
        // Another host never shared the park.
        assert!(gate.admit("ghe.acme.test").await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn spacing_reserves_one_slot_per_read() {
        let gate = HostGate::default();
        let first = tokio::time::Instant::now();
        drop(gate.admit("github.com").await.unwrap());
        drop(gate.admit("github.com").await.unwrap());
        assert!(tokio::time::Instant::now() - first >= HOST_SPACING);
    }

    async fn serve_forge(router: axum::Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        addr
    }

    /// The hosted transport keeps the same conditional discipline as `gh`
    /// (decision 65 meets decision 66): the borrowed credential rides as the
    /// bearer, a stored ETag goes out as `If-None-Match`, and an unchanged
    /// answer comes back as a 304 rather than a paid re-read.
    #[tokio::test]
    async fn the_rest_transport_sends_the_stored_etag_and_reads_the_304() {
        type Seen = Arc<Mutex<Vec<(Option<String>, Option<String>)>>>;
        let seen: Seen = Arc::default();
        let recorded = Arc::clone(&seen);
        let pull = move |headers: axum::http::HeaderMap| {
            let recorded = Arc::clone(&recorded);
            async move {
                let header = |name: axum::http::HeaderName| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .map(ToOwned::to_owned)
                };
                let conditional = header(axum::http::header::IF_NONE_MATCH);
                recorded.lock().unwrap().push((
                    header(axum::http::header::AUTHORIZATION),
                    conditional.clone(),
                ));
                let unchanged = conditional.as_deref() == Some("W/\"pull-1\"");
                let body = if unchanged {
                    String::new()
                } else {
                    serde_json::json!({
                        "number": 12,
                        "html_url": "https://github.com/acme/demo/pull/12",
                        "state": "open",
                        "head": { "ref": "feature", "sha": "aaa" },
                        "base": { "ref": "main" },
                    })
                    .to_string()
                };
                axum::http::Response::builder()
                    .status(if unchanged { 304 } else { 200 })
                    .header("etag", "W/\"pull-1\"")
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }
        };
        let api = serve_forge(
            axum::Router::new().route("/repos/acme/demo/pulls/12", axum::routing::get(pull)),
        )
        .await;

        let credential = GitCredential {
            username: "x-access-token".into(),
            secret: "ghs_test_borrowed".into(),
        };
        let api_base = format!("http://{api}");
        let transport = FetchTransport::Rest {
            api_base: &api_base,
            credential: &credential,
        };
        let gate = HostGate::default();
        let first = read_pull_request(&gate, transport, "github.com", "acme", "demo", 12, None)
            .await
            .unwrap();
        let EndpointRead::Fresh { value, etag } = first else {
            panic!("expected fresh: {first:?}");
        };
        assert_eq!(value.number, 12);
        let etag = etag.expect("a 200 carries the etag to send next time");
        let second = read_pull_request(
            &gate,
            transport,
            "github.com",
            "acme",
            "demo",
            12,
            Some(&etag),
        )
        .await
        .unwrap();
        assert_eq!(second, EndpointRead::NotModified);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0],
            (Some("Bearer ghs_test_borrowed".into()), None),
            "the first read is unconditional and authenticated"
        );
        assert_eq!(
            seen[1],
            (
                Some("Bearer ghs_test_borrowed".into()),
                Some("W/\"pull-1\"".into())
            ),
            "the second read sends the stored ETag"
        );
    }

    /// A limit answer over the hosted transport parks the host exactly as
    /// one over `gh` does: the next read waits out the stated window.
    #[tokio::test]
    async fn a_rest_limit_answer_parks_the_host() {
        let limited = || async {
            axum::http::Response::builder()
                .status(403)
                .header("retry-after", "120")
                .body(axum::body::Body::from(
                    r#"{"message":"You have exceeded a secondary rate limit. Please wait."}"#,
                ))
                .unwrap()
        };
        let api = serve_forge(
            axum::Router::new().route("/repos/acme/demo/pulls/12", axum::routing::get(limited)),
        )
        .await;

        let credential = GitCredential {
            username: "x-access-token".into(),
            secret: "ghs_test_borrowed".into(),
        };
        let api_base = format!("http://{api}");
        let transport = FetchTransport::Rest {
            api_base: &api_base,
            credential: &credential,
        };
        let gate = HostGate::default();
        let refused =
            read_pull_request(&gate, transport, "github.com", "acme", "demo", 12, None).await;
        assert!(
            matches!(refused, Err(FetchFailure::Refused(403, _))),
            "{refused:?}"
        );
        let parked = gate.admit("github.com").await;
        assert!(
            matches!(parked, Err(remaining) if remaining.as_secs() > 100),
            "{parked:?}"
        );
    }

    /// The timeline pages oldest-first, so the membership read walks to the
    /// last page: a queue entry on page one that a later page removes reads
    /// as out, exactly as the `gh` arm's `--paginate` reports it.
    #[tokio::test]
    async fn the_rest_membership_read_walks_the_timeline_to_its_last_page() {
        let timeline = |axum::extract::Query(params): axum::extract::Query<
            std::collections::HashMap<String, String>,
        >| async move {
            let page: u32 = params
                .get("page")
                .map_or(1, |page| page.parse().expect("a numeric page"));
            let events = match page {
                1 => {
                    let mut events: Vec<serde_json::Value> = (0..99)
                        .map(|_| serde_json::json!({ "event": "labeled" }))
                        .collect();
                    events.push(serde_json::json!({ "event": "added_to_merge_queue" }));
                    events
                }
                2 => vec![
                    serde_json::json!({ "event": "labeled" }),
                    serde_json::json!({ "event": "removed_from_merge_queue" }),
                ],
                page => panic!("the walk must stop at the short page, read page {page}"),
            };
            axum::Json(serde_json::Value::Array(events))
        };
        let api = serve_forge(axum::Router::new().route(
            "/repos/acme/demo/issues/12/timeline",
            axum::routing::get(timeline),
        ))
        .await;

        let credential = GitCredential {
            username: "x-access-token".into(),
            secret: "ghs_test_borrowed".into(),
        };
        let api_base = format!("http://{api}");
        let transport = FetchTransport::Rest {
            api_base: &api_base,
            credential: &credential,
        };
        let gate = HostGate::default();
        let membership =
            read_merge_queue_membership(&gate, transport, "github.com", "acme", "demo", 12).await;
        assert_eq!(
            membership,
            Some(false),
            "the removal on the last page wins over the addition on the first"
        );
    }

    #[cfg(unix)]
    mod shim {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn write_executable(path: &std::path::Path, body: &str) {
            std::fs::write(path, body).unwrap();
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }

        #[tokio::test]
        async fn a_stored_etag_turns_an_unchanged_answer_into_a_304() {
            let dir = tempfile::TempDir::new().unwrap();
            let gh = dir.path().join("gh");
            write_executable(
                &gh,
                r#"#!/bin/sh
case "$*" in
  *If-None-Match*)
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"pull-1"\r\n\r\n'
    exit 1;;
  *)
    printf 'HTTP/2.0 200 OK\r\nEtag: W/"pull-1"\r\n\r\n'
    echo '{"number":12,"html_url":"https://github.com/acme/demo/pull/12","state":"open","head":{"ref":"f","sha":"aaa"},"base":{"ref":"main"}}'
    exit 0;;
esac
"#,
            );
            let gate = HostGate::default();
            let transport = FetchTransport::Gh {
                cwd: dir.path(),
                binary: &gh,
            };
            let first = read_pull_request(&gate, transport, "github.com", "acme", "demo", 12, None)
                .await
                .unwrap();
            let EndpointRead::Fresh { value, etag } = first else {
                panic!("expected fresh: {first:?}");
            };
            assert_eq!(value.number, 12);
            assert_eq!(value.head_sha.as_deref(), Some("aaa"));
            let etag = etag.expect("a 200 carries the etag to send next time");
            let second = read_pull_request(
                &gate,
                transport,
                "github.com",
                "acme",
                "demo",
                12,
                Some(&etag),
            )
            .await
            .unwrap();
            assert_eq!(second, EndpointRead::NotModified);
        }

        #[tokio::test]
        async fn a_secondary_limit_answer_parks_the_host() {
            let dir = tempfile::TempDir::new().unwrap();
            let gh = dir.path().join("gh");
            write_executable(
                &gh,
                "#!/bin/sh\nprintf 'HTTP/2.0 403 Forbidden\\r\\nRetry-After: 120\\r\\n\\r\\n'\necho '{\"message\":\"You have exceeded a secondary rate limit. Please wait.\"}'\nexit 1\n",
            );
            let gate = HostGate::default();
            let refused = read_pull_request(
                &gate,
                FetchTransport::Gh {
                    cwd: dir.path(),
                    binary: &gh,
                },
                "github.com",
                "acme",
                "demo",
                12,
                None,
            )
            .await;
            assert!(
                matches!(refused, Err(FetchFailure::Refused(403, _))),
                "{refused:?}"
            );
            // The next read waits out the stated window rather than going
            // out hot.
            let parked = gate.admit("github.com").await;
            assert!(
                matches!(parked, Err(remaining) if remaining.as_secs() > 100),
                "{parked:?}"
            );
            // Another host never shared the park.
            assert!(gate.admit("ghe.acme.test").await.is_ok());
        }
    }
}
