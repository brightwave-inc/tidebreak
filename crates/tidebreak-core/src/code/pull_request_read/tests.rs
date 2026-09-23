//! The merge's contract: examples for each read path that has regressed
//! stored state before, then randomized property tests with a fixed seed.

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::*;
use crate::code::{PullRequestCheck, PullRequestCheckBucket};

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap() + Duration::seconds(seconds)
}

fn snapshot(version: i64, head: &str) -> PullRequestSnapshot {
    PullRequestSnapshot {
        url: "https://github.com/acme/tools/pull/7".into(),
        title: format!("Version {version}"),
        state: CodePullRequestState::Open,
        draft: false,
        author: Some("octocat".into()),
        head_branch: "feature".into(),
        base_branch: "main".into(),
        head_sha: Some(head.into()),
        created_at: at(0),
        updated_at: at(version),
        merged_at: None,
        closed_at: None,
    }
}

fn read() -> PullRequestRead {
    PullRequestRead::new(OwnerId::local(), "github.com", "acme", "tools", 7)
}

fn object(
    snapshot: PullRequestSnapshot,
    observed_at: DateTime<Utc>,
    mergeability: Option<(&str, &str)>,
    etag: Option<&str>,
) -> PullRequestObjectRead {
    PullRequestObjectRead {
        snapshot,
        mergeability: mergeability.map(|(mergeable, state)| PullRequestMergeability {
            mergeable: Some(mergeable.into()),
            merge_state_status: Some(state.into()),
        }),
        auto_merge_enabled: Some(false),
        observed_at,
        etag: etag.map(ToOwned::to_owned),
    }
}

fn check(name: &str, bucket: PullRequestCheckBucket) -> PullRequestCheck {
    PullRequestCheck {
        name: name.into(),
        bucket,
        detail: None,
        url: None,
    }
}

fn checks(
    head: &str,
    list: Vec<PullRequestCheck>,
    observed_at: DateTime<Utc>,
) -> PullRequestChecksRead {
    PullRequestChecksRead {
        head_sha: Some(head.into()),
        checks: list,
        observed_at,
        etag: None,
    }
}

fn review(decision: Option<&str>, observed_at: DateTime<Utc>) -> PullRequestReviewRead {
    PullRequestReviewRead {
        decision: decision.map(ToOwned::to_owned),
        observed_at,
        etag: None,
    }
}

/// What the conditional fetcher stores for head `aaa`: a full read with a
/// failing check, a requested change, and a conflict.
fn fetched() -> StoredPullRequest {
    let mut full = read();
    full.object = Some(object(
        snapshot(10, "aaa"),
        at(100),
        Some(("conflicting", "dirty")),
        Some("W/\"pull-1\""),
    ));
    full.checks = Some(PullRequestChecksRead {
        etag: Some("W/\"checks-1\"".into()),
        ..checks(
            "aaa",
            vec![check("ci", PullRequestCheckBucket::Fail)],
            at(101),
        )
    });
    full.review = Some(PullRequestReviewRead {
        etag: Some("W/\"reviews-1\"".into()),
        ..review(Some("changes_requested"), at(102))
    });
    StoredPullRequest::first_sighting(&full, CodePullRequestId::new()).unwrap()
}

fn live(state: &StoredPullRequest) -> &CodePullRequestLiveState {
    state
        .fact
        .live
        .as_ref()
        .expect("the fetched row has a live tier")
}

/// Issues 3339 and 3364: a hosted REST list read loads neither reviews nor
/// mergeability. It must keep both, not stamp its absence over them.
#[test]
fn a_rest_list_read_keeps_the_review_decision_and_mergeability() {
    let stored = fetched();
    let mut list = read();
    list.object = Some(PullRequestObjectRead {
        mergeability: None,
        ..object(snapshot(10, "aaa"), at(200), None, None)
    });
    let merged = merge_pull_request_read(&stored, &list);
    assert_eq!(
        live(&merged).review_decision.as_deref(),
        Some("changes_requested")
    );
    assert_eq!(live(&merged).mergeable.as_deref(), Some("conflicting"));
    assert_eq!(live(&merged).merge_state_status.as_deref(), Some("dirty"));
    assert!(!merged.visibly_differs(&stored));
}

/// Issue 3312: a list read for every state does not ask for checks, so it
/// keeps the head's check runs rather than reporting none.
#[test]
fn a_read_that_did_not_load_checks_keeps_them() {
    let stored = fetched();
    let mut list = read();
    list.object = Some(object(
        snapshot(10, "aaa"),
        at(200),
        Some(("conflicting", "dirty")),
        None,
    ));
    let merged = merge_pull_request_read(&stored, &list);
    assert_eq!(live(&merged).checks, live(&stored).checks);
    assert_eq!(live(&merged).checks_summary, live(&stored).checks_summary);
    assert_eq!(merged.etags.checks.as_deref(), Some("W/\"checks-1\""));
}

/// A newer object on a new head clears what described the old head. The
/// failing check belonged to `aaa`; reporting it on `bbb` would start a fix
/// turn for a head nobody has checked yet.
#[test]
fn a_newer_head_clears_the_old_heads_checks_and_mergeability() {
    let stored = fetched();
    let mut pushed = read();
    pushed.object = Some(PullRequestObjectRead {
        mergeability: None,
        ..object(snapshot(20, "bbb"), at(200), None, None)
    });
    let merged = merge_pull_request_read(&stored, &pushed);
    assert_eq!(merged.fact.head_sha.as_deref(), Some("bbb"));
    assert_eq!(live(&merged).checks, None);
    assert_eq!(live(&merged).checks_summary, None);
    assert_eq!(live(&merged).mergeable, None);
    assert_eq!(live(&merged).merge_state_status, None);
    assert_eq!(merged.etags.checks, None);
    assert_eq!(
        merged.etags.pull, None,
        "the stored object no longer matches the validator"
    );
    // The review decision is not tied to a head.
    assert_eq!(
        live(&merged).review_decision.as_deref(),
        Some("changes_requested")
    );

    // Checks read for the old head land nowhere now.
    let mut late = read();
    late.object = Some(object(snapshot(10, "aaa"), at(210), None, None));
    late.checks = Some(checks(
        "aaa",
        vec![check("ci", PullRequestCheckBucket::Pass)],
        at(211),
    ));
    let after_late = merge_pull_request_read(&merged, &late);
    assert_eq!(after_late.fact.head_sha.as_deref(), Some("bbb"));
    assert_eq!(live(&after_late).checks, None);
}

/// Issues 2781, 2799, and 2817: a 304 that lands after a newer snapshot must
/// not reconstruct the older one over it.
#[test]
fn a_late_304_cannot_roll_back_a_newer_snapshot() {
    let stored = fetched();
    let not_modified = stored.restate_object(at(300));
    let mut newer = read();
    newer.object = Some(object(snapshot(30, "ccc"), at(250), None, None));
    let moved = merge_pull_request_read(&stored, &newer);
    let mut late = read();
    late.object = Some(not_modified);
    let merged = merge_pull_request_read(&moved, &late);
    assert_eq!(merged.fact.head_sha.as_deref(), Some("ccc"));
    assert_eq!(merged.fact.title, "Version 30");
    assert_eq!(
        merged.etags.pull, None,
        "a stale 304 does not restore its validator"
    );
}

#[test]
fn a_304_confirms_the_stored_object_and_keeps_its_validator() {
    let stored = fetched();
    let mut confirm = read();
    confirm.object = Some(stored.restate_object(at(400)));
    confirm.checks = stored.restate_checks(at(401));
    confirm.review = Some(stored.restate_review(at(402)));
    let merged = merge_pull_request_read(&stored, &confirm);
    assert!(!merged.visibly_differs(&stored));
    assert_eq!(merged.fact.last_seen_at, at(400));
    assert_eq!(live(&merged).observed_at, at(402));
    assert_eq!(merged.etags, stored.etags);
}

#[test]
fn a_stale_read_changes_nothing() {
    let stored = fetched();
    let mut stale = read();
    stale.object = Some(object(
        snapshot(5, "aaa"),
        at(50),
        Some(("mergeable", "clean")),
        Some("W/\"old\""),
    ));
    stale.checks = Some(checks("aaa", Vec::new(), at(51)));
    stale.review = Some(review(None, at(52)));
    assert_eq!(merge_pull_request_read(&stored, &stale), stored);
}

#[test]
fn a_loaded_empty_answer_clears_what_it_covers() {
    let stored = fetched();
    let mut fresh = read();
    fresh.object = Some(object(
        snapshot(10, "aaa"),
        at(500),
        Some(("mergeable", "clean")),
        None,
    ));
    fresh.checks = Some(checks("aaa", Vec::new(), at(501)));
    fresh.review = Some(review(None, at(502)));
    let merged = merge_pull_request_read(&stored, &fresh);
    assert_eq!(live(&merged).checks, Some(Vec::new()));
    assert_eq!(live(&merged).checks_summary.as_deref(), Some("no checks"));
    assert_eq!(live(&merged).review_decision, None);
    assert_eq!(live(&merged).mergeable.as_deref(), Some("mergeable"));
    // New values without validators leave none behind.
    assert_eq!(merged.etags.checks, None);
    assert_eq!(merged.etags.reviews, None);
    assert_eq!(merged.etags.pull, None);
}

#[test]
fn a_validator_survives_a_read_that_changed_nothing_it_covers() {
    let stored = fetched();
    let mut same = read();
    same.object = Some(object(
        snapshot(10, "aaa"),
        at(600),
        Some(("conflicting", "dirty")),
        None,
    ));
    same.review = Some(review(Some("changes_requested"), at(601)));
    let merged = merge_pull_request_read(&stored, &same);
    assert_eq!(merged.etags, stored.etags);
    assert_eq!(merged.fact.last_seen_at, at(600));
}

#[test]
fn only_a_read_that_loaded_the_object_mints_a_row() {
    let mut checks_only = read();
    checks_only.checks = Some(checks("aaa", Vec::new(), at(1)));
    assert!(StoredPullRequest::first_sighting(&checks_only, CodePullRequestId::new()).is_none());

    let stored = fetched();
    assert_eq!(stored.fact.first_seen_at, at(100));
    assert_eq!(stored.fact.last_seen_at, at(100));
    assert_eq!(stored.etags.pull.as_deref(), Some("W/\"pull-1\""));
    assert_eq!(
        live(&stored).checks_summary.as_deref(),
        Some("0 passing, 0 pending, 1 failing")
    );
}

#[test]
fn the_digest_projects_the_live_tier() {
    let digest = fetched().fact.digest();
    assert_eq!(digest.head_sha.as_deref(), Some("aaa"));
    assert_eq!(digest.review_decision.as_deref(), Some("changes_requested"));
    assert_eq!(digest.check_counts.map(|counts| counts.failing), Some(1));
    assert_eq!(digest.merged, Some(false));
}

// Randomized property tests. The workspace has no property-testing crate, so
// these draw cases from a fixed-seed generator: every run explores the same
// cases, and a failure names the case that broke.

const CASES: usize = 400;
const PERMUTATIONS: usize = 8;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap()
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            let other = self.below(index + 1);
            items.swap(index, other);
        }
    }
}

/// Hands out observation times no other observation shares, so every group
/// key is distinct.
struct Clock {
    used: std::collections::HashSet<i64>,
}

impl Clock {
    fn new() -> Self {
        Self {
            used: std::collections::HashSet::new(),
        }
    }

    /// A fresh time between `from` and `from + span` seconds.
    fn fresh(&mut self, rng: &mut Rng, from: i64, span: usize) -> DateTime<Utc> {
        loop {
            let offset = from + i64::try_from(rng.below(span)).unwrap();
            if self.used.insert(offset) {
                return at(offset);
            }
        }
    }
}

/// The versions GitHub served, oldest first. Each version names its head, and
/// once the head moves it never comes back.
fn random_versions(rng: &mut Rng) -> Vec<PullRequestSnapshot> {
    let count = 1 + rng.below(5);
    let mut head = 0;
    (0..count)
        .map(|index| {
            if index > 0 && rng.chance(45) {
                head += 1;
            }
            let version = i64::try_from(index).unwrap() * 10 + 10;
            PullRequestSnapshot {
                title: format!("Title {}", rng.below(3)),
                draft: rng.chance(20),
                state: if index + 1 == count && rng.chance(20) {
                    CodePullRequestState::Merged
                } else {
                    CodePullRequestState::Open
                },
                ..snapshot(version, &format!("h{head}"))
            }
        })
        .collect()
}

fn random_checks(rng: &mut Rng) -> Vec<PullRequestCheck> {
    let buckets = [
        PullRequestCheckBucket::Pass,
        PullRequestCheckBucket::Pending,
        PullRequestCheckBucket::Fail,
        PullRequestCheckBucket::Skipped,
    ];
    (0..rng.below(3))
        .map(|index| check(&format!("check {index}"), buckets[rng.below(buckets.len())]))
        .collect()
}

fn random_etag(rng: &mut Rng, kind: &str) -> Option<String> {
    rng.chance(40)
        .then(|| format!("W/\"{kind}-{}\"", rng.below(1000)))
}

/// Which head a head-bound observation names: almost always the object's
/// own, sometimes another one, which must never land.
fn observed_head(rng: &mut Rng, object: Option<&PullRequestObjectRead>) -> Option<String> {
    match object {
        Some(object) if rng.chance(90) => object.snapshot.head_sha.clone(),
        _ => Some(format!("h{}", rng.below(4))),
    }
}

/// One read a real reader could make: a random version of the object, a
/// random subset of the other groups, and fresh observation times from
/// `from` onward.
fn random_read(
    rng: &mut Rng,
    clock: &mut Clock,
    versions: &[PullRequestSnapshot],
    from: i64,
) -> PullRequestRead {
    let mut read = read();
    if rng.chance(85) {
        let version = versions[rng.below(versions.len())].clone();
        let mergeability = rng.chance(60).then(|| PullRequestMergeability {
            mergeable: [
                None,
                Some("mergeable"),
                Some("conflicting"),
                Some("unknown"),
            ][rng.below(4)]
            .map(ToOwned::to_owned),
            merge_state_status: [None, Some("clean"), Some("blocked"), Some("dirty")][rng.below(4)]
                .map(ToOwned::to_owned),
        });
        read.object = Some(PullRequestObjectRead {
            snapshot: version,
            mergeability,
            auto_merge_enabled: rng.chance(60).then(|| rng.chance(50)),
            observed_at: clock.fresh(rng, from, 10_000),
            etag: random_etag(rng, "pull"),
        });
    }
    if rng.chance(60) {
        read.checks = Some(PullRequestChecksRead {
            head_sha: observed_head(rng, read.object.as_ref()),
            checks: random_checks(rng),
            observed_at: clock.fresh(rng, from, 10_000),
            etag: random_etag(rng, "checks"),
        });
    }
    if rng.chance(50) {
        read.review = Some(PullRequestReviewRead {
            decision: [None, Some("approved"), Some("changes_requested")][rng.below(3)]
                .map(ToOwned::to_owned),
            observed_at: clock.fresh(rng, from, 10_000),
            etag: random_etag(rng, "reviews"),
        });
    }
    if rng.chance(40) {
        read.queue = Some(PullRequestQueueRead {
            head_sha: observed_head(rng, read.object.as_ref()),
            in_merge_queue: rng.chance(50),
            observed_at: clock.fresh(rng, from, 10_000),
        });
    }
    read
}

/// A row as some reads left it: a first sighting, then a few more reads.
fn random_state(
    rng: &mut Rng,
    clock: &mut Clock,
    versions: &[PullRequestSnapshot],
) -> StoredPullRequest {
    let mut first = random_read(rng, clock, versions, 1_000);
    if first.object.is_none() {
        first.object = Some(object(
            versions[0].clone(),
            clock.fresh(rng, 1_000, 10_000),
            None,
            None,
        ));
    }
    let mut state = StoredPullRequest::first_sighting(&first, CodePullRequestId::new()).unwrap();
    for _ in 0..rng.below(6) {
        let next = random_read(rng, clock, versions, 1_000);
        state = merge_pull_request_read(&state, &next);
    }
    state
}

/// One live field group, with the time it was observed.
#[derive(Debug, PartialEq, Eq)]
struct Group<T> {
    value: T,
    observed_at: Option<DateTime<Utc>>,
}

fn checks_group(
    state: &StoredPullRequest,
) -> Group<(Option<Vec<PullRequestCheck>>, Option<String>)> {
    let live = state.fact.live.as_ref();
    Group {
        value: (
            live.and_then(|live| live.checks.clone()),
            live.and_then(|live| live.checks_summary.clone()),
        ),
        observed_at: state.observed.checks,
    }
}

fn mergeability_group(state: &StoredPullRequest) -> Group<PullRequestMergeability> {
    Group {
        value: mergeability_of(&state.fact),
        observed_at: state.observed.mergeability,
    }
}

fn queue_group(state: &StoredPullRequest) -> Group<Option<bool>> {
    Group {
        value: state
            .fact
            .live
            .as_ref()
            .and_then(|live| live.in_merge_queue),
        observed_at: state.observed.queue,
    }
}

fn review_group(state: &StoredPullRequest) -> Group<Option<String>> {
    Group {
        value: state
            .fact
            .live
            .as_ref()
            .and_then(|live| live.review_decision.clone()),
        observed_at: state.observed.review,
    }
}

fn auto_merge_group(state: &StoredPullRequest) -> Group<Option<bool>> {
    Group {
        value: auto_merge_of(&state.fact),
        observed_at: state.observed.auto_merge,
    }
}

fn is_cleared<T: Default + PartialEq>(group: &Group<T>) -> bool {
    group.observed_at.is_none() && group.value == T::default()
}

/// Property: merging a partial read never erases a field the read did not
/// observe, and merging a not-modified read changes nothing a reader sees.
/// The one exception is the head-bound groups when the read moved the head:
/// those clear, because they described a commit that is no longer the head.
#[test]
fn partial_and_not_modified_reads_never_erase_established_fields() {
    let mut rng = Rng(0x71de_b4ea_0001);
    for case in 0..CASES {
        let mut clock = Clock::new();
        let versions = random_versions(&mut rng);
        let state = random_state(&mut rng, &mut clock, &versions);
        let partial = random_read(&mut rng, &mut clock, &versions, 1_000);
        let merged = merge_pull_request_read(&state, &partial);
        let head_moved = merged.fact.head_sha != state.fact.head_sha;
        let object = partial.object.as_ref();

        if object.is_none() {
            assert_eq!(
                PullRequestSnapshot::of_fact(&merged.fact),
                PullRequestSnapshot::of_fact(&state.fact),
                "case {case}: a read without the object kept the snapshot"
            );
            assert_eq!(
                merged.fact.last_seen_at, state.fact.last_seen_at,
                "case {case}"
            );
        }
        if object
            .and_then(|object| object.mergeability.as_ref())
            .is_none()
        {
            let group = mergeability_group(&merged);
            if head_moved {
                assert!(is_cleared(&group), "case {case}: {group:?}");
            } else {
                assert_eq!(group, mergeability_group(&state), "case {case}");
            }
        }
        if partial.checks.is_none() {
            let group = checks_group(&merged);
            if head_moved {
                assert!(is_cleared(&group), "case {case}: {group:?}");
            } else {
                assert_eq!(group, checks_group(&state), "case {case}");
            }
        }
        if partial.queue.is_none() {
            let group = queue_group(&merged);
            if head_moved {
                assert!(is_cleared(&group), "case {case}: {group:?}");
            } else {
                assert_eq!(group, queue_group(&state), "case {case}");
            }
        }
        if partial.review.is_none() {
            assert_eq!(review_group(&merged), review_group(&state), "case {case}");
        }
        if object
            .and_then(|object| object.auto_merge_enabled)
            .is_none()
        {
            assert_eq!(
                auto_merge_group(&merged),
                auto_merge_group(&state),
                "case {case}"
            );
        }
        assert_eq!(
            merged.fact.first_seen_at, state.fact.first_seen_at,
            "case {case}"
        );

        // A 304 restates exactly what the validators named.
        let mut not_modified = read();
        not_modified.object = Some(state.restate_object(clock.fresh(&mut rng, 20_000, 1_000)));
        not_modified.checks = state.restate_checks(clock.fresh(&mut rng, 20_000, 1_000));
        not_modified.review = Some(state.restate_review(clock.fresh(&mut rng, 20_000, 1_000)));
        let confirmed = merge_pull_request_read(&state, &not_modified);
        assert!(
            !confirmed.visibly_differs(&state),
            "case {case}: a not-modified read changed a visible field"
        );
        assert_eq!(confirmed.etags, state.etags, "case {case}");
    }
}

/// Property: a read older than the stored state in every group it observed
/// changes nothing at all.
#[test]
fn a_stale_read_never_regresses_state() {
    let mut rng = Rng(0x71de_b4ea_0002);
    for case in 0..CASES {
        let mut clock = Clock::new();
        let versions = random_versions(&mut rng);
        let state = random_state(&mut rng, &mut clock, &versions);
        // Every stored time is at least 1_000; stale times come from before.
        let mut stale = random_read(&mut rng, &mut clock, &versions, 0);
        for time in [
            stale.object.as_mut().map(|object| &mut object.observed_at),
            stale.checks.as_mut().map(|checks| &mut checks.observed_at),
            stale.review.as_mut().map(|review| &mut review.observed_at),
            stale.queue.as_mut().map(|queue| &mut queue.observed_at),
        ]
        .into_iter()
        .flatten()
        {
            *time = at(time.timestamp() - at(0).timestamp() - 20_000);
        }
        if let Some(object) = stale.object.as_mut() {
            // No newer version than the stored one.
            let stored_version = state.fact.updated_at;
            if object.snapshot.updated_at > stored_version {
                object.snapshot = PullRequestSnapshot::of_fact(&state.fact);
            }
            // A group the row never observed would be news, not a stale read.
            if state.observed.mergeability.is_none() {
                object.mergeability = None;
            }
            if state.observed.auto_merge.is_none() {
                object.auto_merge_enabled = None;
            }
        }
        if state.observed.checks.is_none() {
            stale.checks = None;
        }
        if state.observed.review.is_none() {
            stale.review = None;
        }
        if state.observed.queue.is_none() {
            stale.queue = None;
        }
        assert_eq!(
            merge_pull_request_read(&state, &stale),
            state,
            "case {case}: a stale read changed the row"
        );
    }
}

fn without_validators(mut state: StoredPullRequest) -> StoredPullRequest {
    state.etags = PullRequestEtags::default();
    state
}

/// Property: the same reads applied in any order leave the same state. The
/// validators are the exception: they name the request that produced a value
/// so the next fetch can be conditional, and which request that was depends
/// on arrival. Their own rule is checked separately: a stored validator always
/// travels with the values some read reported beside it.
#[test]
fn reads_in_any_order_converge() {
    let mut rng = Rng(0x71de_b4ea_0003);
    for case in 0..CASES {
        let mut clock = Clock::new();
        let versions = random_versions(&mut rng);
        let base = random_state(&mut rng, &mut clock, &versions);
        let mut reads: Vec<PullRequestRead> = (0..2 + rng.below(7))
            .map(|_| random_read(&mut rng, &mut clock, &versions, 1_000))
            .collect();
        let apply = |reads: &[PullRequestRead]| {
            reads.iter().fold(base.clone(), |state, read| {
                merge_pull_request_read(&state, read)
            })
        };
        let reference = apply(&reads);
        assert_validators_name_reported_values(&reference, &base, &reads, case);
        for _ in 0..PERMUTATIONS {
            rng.shuffle(&mut reads);
            let shuffled = apply(&reads);
            assert_eq!(
                without_validators(shuffled.clone()),
                without_validators(reference.clone()),
                "case {case}: arrival order changed the state"
            );
            assert_validators_name_reported_values(&shuffled, &base, &reads, case);
        }
    }
}

/// A stored validator is the base row's own or one some read carried, and the
/// stored values it covers are the ones that read reported with it.
fn assert_validators_name_reported_values(
    state: &StoredPullRequest,
    base: &StoredPullRequest,
    reads: &[PullRequestRead],
    case: usize,
) {
    if let Some(etag) = &state.etags.pull {
        let named = base.etags.pull.as_ref() == Some(etag)
            && pull_fields(&base.fact) == pull_fields(&state.fact)
            || reads.iter().any(|read| {
                read.object.as_ref().is_some_and(|object| {
                    object.etag.as_ref() == Some(etag)
                        && object.snapshot == PullRequestSnapshot::of_fact(&state.fact)
                })
            });
        assert!(
            named,
            "case {case}: the pull validator names no reported object"
        );
    }
    if let Some(etag) = &state.etags.checks {
        let stored = state
            .fact
            .live
            .as_ref()
            .and_then(|live| live.checks.clone());
        let named = base.etags.checks.as_ref() == Some(etag)
            && base.fact.live.as_ref().and_then(|live| live.checks.clone()) == stored
            || reads.iter().any(|read| {
                read.checks.as_ref().is_some_and(|checks| {
                    checks.etag.as_ref() == Some(etag) && Some(&checks.checks) == stored.as_ref()
                })
            });
        assert!(
            named,
            "case {case}: the checks validator names no reported checks"
        );
    }
    if let Some(etag) = &state.etags.reviews {
        let stored = state
            .fact
            .live
            .as_ref()
            .and_then(|live| live.review_decision.clone());
        let named = base.etags.reviews.as_ref() == Some(etag)
            && base
                .fact
                .live
                .as_ref()
                .and_then(|live| live.review_decision.clone())
                == stored
            || reads.iter().any(|read| {
                read.review.as_ref().is_some_and(|review| {
                    review.etag.as_ref() == Some(etag) && review.decision == stored
                })
            });
        assert!(
            named,
            "case {case}: the reviews validator names no reported decision"
        );
    }
}
