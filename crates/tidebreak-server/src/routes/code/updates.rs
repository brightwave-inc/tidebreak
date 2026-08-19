//! `WS /code/updates` — the principal's digest channel, restated on connect.
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
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::OwnerId;

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::attention::list_digests;
use crate::code::bus::CodeLiveUpdate;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::state::AppState;

use super::types::{CodeSessionDigest, CodeUpdateNotice};

pub async fn code_updates(
    State(state): State<AppState>,
    code: ScopedCode,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let owner = code.owner().clone();
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_updates(socket, state, owner)))
}

async fn stream_updates(mut socket: WebSocket, state: AppState, owner: OwnerId) {
    let Some(runtime) = state.code.clone() else {
        return;
    };
    let mut live = runtime.bus.subscribe_updates(&owner);
    let mut terminals = state.terminals.subscribe(&owner);
    match list_digests(&runtime.db, &owner).await {
        Ok(sessions) => {
            let notice = CodeUpdateNotice::Snapshot {
                sessions: sessions.into_iter().map(CodeSessionDigest::from).collect(),
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
            update = live.recv() => match update {
                Ok(CodeLiveUpdate::Digest(digest)) => {
                    if send_notice(&mut socket, &CodeUpdateNotice::digest(*digest))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(CodeLiveUpdate::CloneProgress(progress)) => {
                    if send_notice(&mut socket, &CodeUpdateNotice::clone_progress(progress))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    match list_digests(&runtime.db, &owner).await {
                        Ok(sessions) => {
                            let notice = CodeUpdateNotice::Snapshot {
                                sessions: sessions
                                    .into_iter()
                                    .map(CodeSessionDigest::from)
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
                        &CodeUpdateNotice::TerminalActivity {
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

async fn send_notice(socket: &mut WebSocket, notice: &CodeUpdateNotice) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(notice) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}
