//! `WS /updates` — the principal's digest channel, restated on connect.
//!
//! The channel is partitioned by owner in the bus rather than filtered here:
//! [`ScopedCode`] resolves the requesting principal before the upgrade, and
//! the receiver this socket holds is subscribed to that principal alone. A
//! digest addressed to someone else is not dropped on the way out — it never
//! arrives. Queued session notices are revalidated after access changes. Decision 47
//! names the alternative, filtering an install-wide stream at the route or in
//! the client, as the wrong implementation.
//!
//! What is addressed to a principal is wider than their own sessions now.
//! Decision 0086 gives a session readers besides its owner, so the snapshot
//! this socket restates is every live session the principal may read, and the
//! publisher decides who a digest reaches. An `AccessChanged` notice restates
//! that snapshot, which is how a revoked session leaves a live client's list
//! without waiting for a reconnect.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Extension;
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::OwnerId;

use crate::auth::{offered_handshake_subprotocol, GatewayAuthLease, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::attention::list_accessible_digests;
use crate::code::bus::CodeLiveUpdate;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::routes::events::{gateway_auth_revalidation_timer, wait_for_gateway_auth_revalidation};
use crate::state::AppState;

use super::types::{SessionDigest, UpdateNotice};

pub async fn code_updates(
    State(state): State<AppState>,
    code: ScopedCode,
    headers: axum::http::HeaderMap,
    auth_lease: Option<Extension<GatewayAuthLease>>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let owner = code.owner().clone();
    let auth_lease = auth_lease.map(|Extension(lease)| lease);
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_updates(socket, state, owner, auth_lease)))
}

async fn stream_updates(
    mut socket: WebSocket,
    state: AppState,
    owner: OwnerId,
    auth_lease: Option<GatewayAuthLease>,
) {
    let mut auth_revalidation = gateway_auth_revalidation_timer(auth_lease.as_ref());
    let Some(runtime) = state.code.clone() else {
        return;
    };
    let mut live = runtime.bus.subscribe_updates(&owner);
    let mut grant_notices = runtime.grant_revocations().subscribe();
    let mut terminals = state.terminals.subscribe(&owner);
    if send_snapshot(&mut socket, &runtime, &owner).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            _ = wait_for_gateway_auth_revalidation(&mut auth_revalidation) => {
                let invalid = match auth_lease.as_ref() {
                    Some(lease) => !lease.revalidate(&state).await,
                    None => false,
                };
                if invalid {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            },
            notice = grant_notices.recv() => {
                if matches!(notice, Err(RecvError::Closed)) {
                    break;
                }
                if send_snapshot(&mut socket, &runtime, &owner).await.is_err() {
                    break;
                }
            },
            update = live.recv() => match update {
                Ok(CodeLiveUpdate::Digest(digest)) => {
                    if !super::session_events::reader_still_authorized(
                        &runtime.db, Some(&owner), digest.session,
                    ).await {
                        continue;
                    }
                    if send_notice(&mut socket, &UpdateNotice::digest(*digest))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(CodeLiveUpdate::CloneProgress(progress)) => {
                    if send_notice(&mut socket, &UpdateNotice::clone_progress(progress))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(CodeLiveUpdate::HarnessInstall(progress)) => {
                    if send_notice(&mut socket, &UpdateNotice::harness_install(progress))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(CodeLiveUpdate::Delivery) => {
                    if send_notice(&mut socket, &UpdateNotice::Delivery)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(CodeLiveUpdate::TurnRewrite(notice)) => {
                    if !super::session_events::reader_still_authorized(
                        &runtime.db, Some(&owner), notice.session,
                    ).await {
                        continue;
                    }
                    if send_notice(&mut socket, &UpdateNotice::turn_rewrite(notice))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(CodeLiveUpdate::AccessChanged(_)) => {
                    // A row was granted or revoked, or visibility moved. The
                    // snapshot is what says which sessions this principal may
                    // see now, so restate it rather than patch one digest.
                    if send_snapshot(&mut socket, &runtime, &owner).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    if send_snapshot(&mut socket, &runtime, &owner).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            notice = terminals.recv() => match notice {
                Ok(notice) => {
                    if send_notice(
                        &mut socket,
                        &UpdateNotice::TerminalActivity {
                            workspace_id: notice.workspace_id,
                            terminal_id: notice.terminal_id,
                        },
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    let workspaces = match tidebreak_core::db::code::list_workspaces(&runtime.db, &owner, None).await {
                        Ok(workspaces) => workspaces,
                        Err(_) => break,
                    };
                    let mut failed = false;
                    for workspace in workspaces {
                        for terminal in state.terminals.list(workspace.id) {
                            if !terminal.ended
                                && send_notice(
                                    &mut socket,
                                    &UpdateNotice::TerminalActivity {
                                        workspace_id: workspace.id,
                                        terminal_id: terminal.id,
                                    },
                                )
                                .await
                                .is_err()
                            {
                                failed = true;
                                break;
                            }
                        }
                        if failed {
                            break;
                        }
                    }
                    if failed {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Restate every live session this principal may read.
///
/// The snapshot is the whole answer to "what may I see", so a client that
/// gains or loses access needs no reconciliation of its own.
async fn send_snapshot(
    socket: &mut WebSocket,
    runtime: &crate::code::runtime::CodeRuntime,
    owner: &OwnerId,
) -> Result<(), ()> {
    let sessions = list_accessible_digests(&runtime.db, owner)
        .await
        .map_err(|_| ())?;
    let notice = UpdateNotice::Snapshot {
        sessions: authorized_snapshot_sessions(&runtime.db, owner, sessions).await,
    };
    send_notice(socket, &notice).await.map_err(|_| ())
}

async fn send_notice(socket: &mut WebSocket, notice: &UpdateNotice) -> Result<(), axum::Error> {
    let json = serde_json::to_string(notice).map_err(axum::Error::new)?;
    socket.send(Message::Text(json.into())).await
}

/// Drop access lost while the snapshot's digest queries were running.
async fn authorized_snapshot_sessions(
    store: &tidebreak_core::DbStore,
    principal: &OwnerId,
    sessions: Vec<crate::code::bus::SessionDigest>,
) -> Vec<SessionDigest> {
    let mut authorized = Vec::with_capacity(sessions.len());
    for digest in sessions {
        if super::session_events::reader_still_authorized(store, Some(principal), digest.session)
            .await
        {
            authorized.push(SessionDigest::from(digest));
        }
    }
    authorized
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::db::code::{
        bind_external_session, delegated_child_external_key, grant_session_access, insert_session,
        mint_external_grant, revoke_session_access, set_session_context, MintGrantSubject,
    };
    use tidebreak_core::{
        Attention, AttentionSource, CodeGrantKind, DbStore, ExecutionLocation, HarnessKind,
        PermissionMode, Session, SessionAccessLevel, SessionId, SessionKind, SessionLifecycle,
        SessionVisibility,
    };

    fn session(owner: &OwnerId) -> Session {
        Session {
            visibility: SessionVisibility::Private,
            id: SessionId::new(),
            owner: owner.clone(),
            owner_kind: None,
            workspace_id: None,
            kind: SessionKind::Interactive,
            harness_kind: HarnessKind::Internal,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: chrono::Utc::now(),
            execution_location: ExecutionLocation::Machine,
            acts_as: None,
        }
    }

    #[tokio::test]
    async fn snapshot_drops_child_access_revoked_after_digest_build() {
        let dir = tempfile::tempdir().unwrap();
        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("snapshot-access.db").display()
        ))
        .await
        .unwrap();
        let owner = OwnerId::new("user:alice").unwrap();
        let reader = OwnerId::new("user:bob").unwrap();
        let parent = session(&owner);
        let child = session(&owner);
        let own = session(&reader);
        for session in [&parent, &child, &own] {
            insert_session(&db, session).await.unwrap();
        }
        let grant = mint_external_grant(
            &db,
            &owner,
            MintGrantSubject {
                channel_kind: "slack",
                external_identity: "U-alice",
                workspace_identity: "W",
                kind: CodeGrantKind::Person,
            },
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .await
        .unwrap();
        set_session_context(&db, &owner, child.id, None, Some(parent.id), Some("review"))
            .await
            .unwrap();
        for (session, key) in [
            (parent.id, "W/C/1".to_owned()),
            (child.id, delegated_child_external_key(parent.id, "review")),
        ] {
            bind_external_session(&db, &owner, grant.id, "slack", &key, session)
                .await
                .unwrap();
        }
        grant_session_access(
            &db,
            &owner,
            parent.id,
            "principal:user:bob",
            SessionAccessLevel::View,
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
        let built = list_accessible_digests(&db, &reader).await.unwrap();
        assert_eq!(built.len(), 3);
        assert!(built.iter().any(|digest| digest.session == child.id));
        assert_eq!(
            authorized_snapshot_sessions(&db, &reader, built.clone())
                .await
                .len(),
            3
        );

        assert!(
            revoke_session_access(&db, &owner, parent.id, "principal:user:bob")
                .await
                .unwrap()
        );
        let authorized = authorized_snapshot_sessions(&db, &reader, built).await;
        assert_eq!(authorized.len(), 1);
        assert_eq!(authorized[0].session, own.id);
    }
}
