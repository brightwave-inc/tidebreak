//! Authenticated inference identities for installed harnesses hosted by Tidebreak.

use super::{CachedToken, OboGateway, INFERENCE_AUDIENCE};
use std::collections::HashMap;
use std::sync::Arc;
use tidebreak_core::{AgentError, HarnessKind, OwnerId, Result, SessionId};
use tidebreak_harness::HarnessProbe;

/// Server-owned facts about one installed harness, never supplied by an inference request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HarnessIdentity {
    pub(super) session: SessionId,
    pub(super) kind: HarnessKind,
    pub(super) version: String,
}

impl HarnessIdentity {
    /// Unknown binaries and versions retain ordinary, subscription-ineligible OBO.
    pub(crate) fn from_probe(
        session: SessionId,
        kind: HarnessKind,
        probe: &HarnessProbe,
    ) -> Option<Self> {
        if kind.is_in_process()
            || session.as_uuid().is_nil()
            || !probe.found
            || probe.binary_path.is_none()
        {
            return None;
        }
        let version = installed_version(kind, probe.version.as_deref()?)?;
        Some(Self {
            session,
            kind,
            version: version.to_owned(),
        })
    }
}

/// Strip only the decoration emitted by the known CLI's version command.
fn installed_version(kind: HarnessKind, reported: &str) -> Option<&str> {
    let reported = reported.trim_matches(|character: char| character.is_ascii_whitespace());
    let version = match kind {
        HarnessKind::Internal => return None,
        HarnessKind::ClaudeCode => reported.strip_suffix(" (Claude Code)").unwrap_or(reported),
        HarnessKind::Codex => reported.strip_prefix("codex-cli ").unwrap_or(reported),
        HarnessKind::Grok => {
            let reported = reported.strip_prefix("grok ").unwrap_or(reported);
            match reported.split_once(' ') {
                Some((version, metadata))
                    if metadata.starts_with('(') || metadata.starts_with('[') =>
                {
                    version
                }
                _ => reported,
            }
        }
        HarnessKind::Opencode => reported,
    };
    let version = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+_".contains(&byte))
    {
        return None;
    }
    // A numeric major.minor.patch prevents placeholders such as "unknown" or
    // the name of a different CLI from acquiring subscription eligibility.
    let core = version.split(['-', '+', '_']).next()?;
    let mut parts = core.split('.');
    for _ in 0..3 {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    }
    parts.next().is_none().then_some(version)
}

type TokenSlot = Arc<tokio::sync::Mutex<Option<CachedToken>>>;
pub(super) type HarnessTokenSlots = HashMap<HarnessIdentity, TokenSlot>;

impl OboGateway {
    /// Cache separately for each owner, session, engine, and installed version.
    pub(crate) async fn bearer_for_harness(
        &self,
        owner: &OwnerId,
        harness: &HarnessIdentity,
    ) -> Result<String> {
        // Older and standalone installations have no registered client secret.
        // They keep ordinary OBO; a refused authenticated exchange never retries here.
        if self.machine_credentials.is_none() {
            return self.bearer_for(owner).await;
        }
        let user = self
            .users
            .lock()
            .map_err(|_| {
                AgentError::msg("on-behalf-of inference state is unavailable in this process")
            })?
            .get(owner)
            .cloned()
            .ok_or_else(|| {
                AgentError::SignInRequired(
                    "this machine holds no live Model Gateway session for you; sign in again"
                        .into(),
                )
            })?;
        let slot = user
            .harness_tokens
            .lock()
            .map_err(|_| AgentError::msg("harness inference state is unavailable in this process"))?
            .entry(harness.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
            .clone();
        let mut cached = slot.lock().await;
        if let Some(current) = cached.as_ref().filter(|token| token.is_fresh()) {
            return Ok(current.token.to_string());
        }
        let subject = user
            .subject
            .lock()
            .map_err(|_| {
                AgentError::msg("on-behalf-of inference state is unavailable in this process")
            })?
            .clone();
        let minted = self
            .exchange_with_harness(&subject, INFERENCE_AUDIENCE, Some(harness))
            .await?;
        let token = minted.token.to_string();
        *cached = Some(minted);
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_versions_accept_known_cli_output_and_reject_unknown_identity() {
        for (kind, input, expected) in [
            (HarnessKind::ClaudeCode, "2.1.233 (Claude Code)", "2.1.233"),
            (HarnessKind::ClaudeCode, "2.1.234", "2.1.234"),
            (HarnessKind::Codex, "codex-cli 0.147.0", "0.147.0"),
            (HarnessKind::Codex, "0.147.0", "0.147.0"),
            (HarnessKind::Opencode, "1.19.2", "1.19.2"),
            (
                HarnessKind::Grok,
                "grok 1.0.4 (d846eb93d94d) [stable]",
                "1.0.4",
            ),
            (HarnessKind::Grok, "1.0.4", "1.0.4"),
            (
                HarnessKind::Codex,
                "v0.147.0-alpha.1+build_2",
                "0.147.0-alpha.1+build_2",
            ),
        ] {
            assert_eq!(installed_version(kind, input), Some(expected), "{input}");
        }
        for kind in HarnessKind::ALL {
            for input in [
                "",
                "unknown",
                "scripted",
                "next",
                "1.2",
                "1.2.3/evil",
                "different-cli 1.2.3",
                "1.2.3\n4.5.6",
                "1.2.3\u{00a0}",
            ] {
                assert_eq!(installed_version(*kind, input), None, "{kind}: {input:?}");
            }
        }
        assert_eq!(installed_version(HarnessKind::Internal, "1.2.3"), None);
        assert_eq!(
            installed_version(HarnessKind::Codex, &format!("1.2.3-{}", "a".repeat(128))),
            None
        );
    }

    #[test]
    fn identity_requires_the_current_probe_to_find_a_versioned_binary() {
        let mut probe = HarnessProbe {
            found: true,
            binary_path: Some("/installed/claude".into()),
            version: Some("2.1.234 (Claude Code)".into()),
            authenticated: Some(false),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        };
        let session = SessionId::new();
        assert!(HarnessIdentity::from_probe(session, HarnessKind::ClaudeCode, &probe).is_some());
        assert!(HarnessIdentity::from_probe(session, HarnessKind::Internal, &probe).is_none());
        assert!(HarnessIdentity::from_probe(
            SessionId::from(uuid::Uuid::nil()),
            HarnessKind::ClaudeCode,
            &probe
        )
        .is_none());
        probe.found = false;
        assert!(HarnessIdentity::from_probe(session, HarnessKind::ClaudeCode, &probe).is_none());
        probe.found = true;
        probe.binary_path = None;
        assert!(HarnessIdentity::from_probe(session, HarnessKind::ClaudeCode, &probe).is_none());
        probe.binary_path = Some("/installed/claude".into());
        probe.version = None;
        assert!(HarnessIdentity::from_probe(session, HarnessKind::ClaudeCode, &probe).is_none());
    }

    type RecordedExchanges = Arc<std::sync::Mutex<Vec<HashMap<String, String>>>>;

    async fn gateway(
        registered: bool,
        refuse: bool,
        lifetime: u64,
    ) -> (
        Arc<OboGateway>,
        RecordedExchanges,
        tokio::task::JoinHandle<()>,
    ) {
        gateway_with_ack(registered, refuse, lifetime, None).await
    }

    async fn gateway_with_ack(
        registered: bool,
        refuse: bool,
        lifetime: u64,
        acknowledgement: Option<serde_json::Value>,
    ) -> (
        Arc<OboGateway>,
        RecordedExchanges,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::response::IntoResponse;
        let recorded = RecordedExchanges::default();
        let seen = recorded.clone();
        let router = axum::Router::new().route("/oauth/token", axum::routing::post(
            move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                let seen = seen.clone();
                let acknowledgement = acknowledgement.clone();
                async move {
                    let mut seen = seen.lock().unwrap();
                    seen.push(form.clone());
                    if refuse {
                        return (axum::http::StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({
                            "error": "invalid_grant", "error_description": "engine binding refused",
                        }))).into_response();
                    }
                    let mut body = acknowledgement.unwrap_or_else(|| serde_json::json!({
                        "engine": form.get("engine"),
                        "engine_version": form.get("engine_version"),
                        "engine_session_id": form.get("engine_session_id"),
                    }));
                    body["access_token"] = serde_json::json!(format!("inference-{}", seen.len()));
                    body["expires_in"] = serde_json::json!(lifetime);
                    axum::Json(body).into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut gateway = OboGateway::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "tidebreak:test-machine".into(),
        )
        .unwrap();
        if registered {
            gateway.machine_credentials =
                Some(("tidebreak-test-client".into(), "test-client-secret".into()));
        }
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (Arc::new(gateway), recorded, server)
    }

    fn identity(session: SessionId, kind: HarnessKind, version: &str) -> HarnessIdentity {
        HarnessIdentity {
            session,
            kind,
            version: version.into(),
        }
    }

    #[tokio::test]
    async fn authenticated_engine_tokens_are_single_flight_and_isolated_by_owner_session_engine_version(
    ) {
        let (gateway, recorded, server) = gateway(true, false, 3600).await;
        let alice = OwnerId::new("user:alice").unwrap();
        let bob = OwnerId::new("user:bob").unwrap();
        gateway.record_caller(&alice, "alice-subject".into());
        gateway.record_caller(&bob, "bob-subject".into());
        let session = SessionId::new();
        let claude = identity(session, HarnessKind::ClaudeCode, "2.1.234");
        let ordinary = gateway.bearer_for(&alice).await.unwrap();
        let (first, concurrent) = tokio::join!(
            gateway.bearer_for_harness(&alice, &claude),
            gateway.bearer_for_harness(&alice, &claude),
        );
        let first = first.unwrap();
        assert_eq!(first, concurrent.unwrap());
        assert_ne!(first, ordinary);
        for next in [
            identity(SessionId::new(), HarnessKind::ClaudeCode, "2.1.234"),
            identity(session, HarnessKind::Codex, "0.147.0"),
            identity(session, HarnessKind::ClaudeCode, "2.1.235"),
        ] {
            assert_ne!(
                gateway.bearer_for_harness(&alice, &next).await.unwrap(),
                first
            );
        }
        assert_ne!(
            gateway.bearer_for_harness(&bob, &claude).await.unwrap(),
            first
        );
        assert_eq!(gateway.bearer_for(&alice).await.unwrap(), ordinary);
        let seen = recorded.lock().unwrap();
        assert_eq!(seen.len(), 6);
        assert_eq!(
            seen[0].len(),
            4,
            "ordinary OBO has no client or engine fields"
        );
        for form in &seen[1..] {
            assert_eq!(form.len(), 9);
            assert_eq!(form["audience"], "llm");
            assert_eq!(form["client_id"], "tidebreak-test-client");
            assert_eq!(form["client_secret"], "test-client-secret");
        }
        assert_eq!(seen[1]["engine"], "claude_code");
        assert_eq!(seen[1]["engine_version"], "2.1.234");
        assert_eq!(seen[1]["engine_session_id"], session.to_string());
        assert_eq!(seen[1]["subject_token"], "alice-subject");
        assert_eq!(seen[5]["subject_token"], "bob-subject");
        server.abort();
    }

    #[tokio::test]
    async fn an_expiring_engine_token_refreshes_with_the_current_owner_subject() {
        let (gateway, recorded, server) = gateway(true, false, 0).await;
        let owner = OwnerId::local();
        let harness = identity(SessionId::new(), HarnessKind::Codex, "0.147.0");
        gateway.record_caller(&owner, "subject-before-refresh".into());
        let first = gateway.bearer_for_harness(&owner, &harness).await.unwrap();
        gateway.record_caller(&owner, "subject-after-refresh".into());
        let next = gateway.bearer_for_harness(&owner, &harness).await.unwrap();
        assert_ne!(first, next);
        let seen = recorded.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1]["subject_token"], "subject-after-refresh");
        assert_eq!(seen[0]["engine_session_id"], seen[1]["engine_session_id"]);
        server.abort();
    }

    #[tokio::test]
    async fn no_registered_client_keeps_ordinary_obo_and_a_refused_engine_never_falls_back() {
        let owner = OwnerId::local();
        let harness = identity(SessionId::new(), HarnessKind::ClaudeCode, "2.1.234");
        for (registered, refuse) in [(false, false), (true, true)] {
            let (gateway, recorded, server) = gateway(registered, refuse, 3600).await;
            gateway.record_caller(&owner, "owner-subject".into());
            let result = gateway.bearer_for_harness(&owner, &harness).await;
            assert_eq!(result.is_err(), refuse);
            let seen = recorded.lock().unwrap();
            assert_eq!(seen.len(), 1, "a refusal must not retry ordinary OBO");
            assert_eq!(seen[0].contains_key("engine"), registered);
            assert_eq!(seen[0].contains_key("client_secret"), registered);
            server.abort();
        }
    }

    #[tokio::test]
    async fn missing_or_mismatched_gateway_acknowledgement_is_never_used_or_cached() {
        let owner = OwnerId::local();
        let harness = identity(SessionId::new(), HarnessKind::ClaudeCode, "2.1.234");
        let expected = serde_json::json!({
            "engine": "claude_code", "engine_version": "2.1.234", "engine_session_id": harness.session,
        });
        let mut acknowledgements = vec![serde_json::json!({})];
        for (field, wrong) in [
            ("engine", "codex".to_owned()),
            ("engine_version", "2.1.235".to_owned()),
            ("engine_session_id", SessionId::new().to_string()),
        ] {
            let mut changed = expected.clone();
            changed[field] = serde_json::json!(wrong);
            acknowledgements.push(changed);
        }
        for acknowledgement in acknowledgements {
            let (gateway, recorded, server) =
                gateway_with_ack(true, false, 3600, Some(acknowledgement)).await;
            gateway.record_caller(&owner, "owner-subject".into());
            for _ in 0..2 {
                let error = gateway
                    .bearer_for_harness(&owner, &harness)
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("did not confirm"));
            }
            assert_eq!(
                recorded.lock().unwrap().len(),
                2,
                "each request exchanges once and never caches an unbound token"
            );
            server.abort();
        }
    }
}
