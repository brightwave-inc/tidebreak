//! `WS /updates` — the principal's digest channel, restated on connect.
//!
//! The channel is partitioned by owner in the bus rather than filtered here:
//! [`ScopedCode`] resolves the requesting principal before the upgrade, and
//! the receiver this socket holds is subscribed to that owner alone. A digest
//! for another owner is not dropped on the way out — it never arrives, and no
//! code in this file could publish it if it did. Decision 47 names the
//! alternative, filtering an install-wide stream at the route or in the
//! client, as the wrong implementation.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Extension;
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::OwnerId;

use crate::auth::{offered_handshake_subprotocol, GatewayAuthLease, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::attention::{list_accessible_digests, list_digests};
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
    let mut access_refresh = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut terminals = state.terminals.subscribe(&owner);
    match list_digests(&runtime.db, &owner).await {
        Ok(sessions) => {
            let notice = UpdateNotice::Snapshot {
                sessions: sessions.into_iter().map(SessionDigest::from).collect(),
            };
            if send_notice(&mut socket, &notice).await.is_err() {
                return;
            }
        }
        Err(_) => return,
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            _ = access_refresh.tick() => {
                match list_accessible_digests(&runtime.db, &owner).await {
                    Ok(sessions) => {
                        let notice = UpdateNotice::Snapshot {
                            sessions: sessions.into_iter().map(SessionDigest::from).collect(),
                        };
                        if send_notice(&mut socket, &notice).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
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
            update = live.recv() => match update {
                Ok(CodeLiveUpdate::Digest(digest)) => {
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
                    if send_notice(&mut socket, &UpdateNotice::turn_rewrite(notice))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    match list_digests(&runtime.db, &owner).await {
                        Ok(sessions) => {
                            let notice = UpdateNotice::Snapshot {
                                sessions: sessions
                                    .into_iter()
                                    .map(SessionDigest::from)
                                    .collect(),
                            };
                            if send_notice(&mut socket, &notice).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
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
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
        }
    }
}

async fn send_notice(socket: &mut WebSocket, notice: &UpdateNotice) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(notice) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}
