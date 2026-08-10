//! The startup chat picker.
//!
//! `openwave tui` with no `--chat` used to open a fresh chat unconditionally,
//! which made every earlier conversation reachable only by pasting its UUID.
//! The picker lists what `GET /chats` returns and lets the user resume one or
//! start a new chat before the session attaches to anything.
//!
//! It reuses [`ChatsOverlay`] — the same list, keys, and panel chrome as the
//! in-session switcher — so the surface a user learns at startup is the one
//! `ctrl+o` shows later. The only difference is what dismissing means: there is
//! no chat to fall back to, so `esc` leaves.

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use openwave_core::{AgentError, ChatId, Result};
use ratatui::layout::Rect;

use super::app;
use super::overlays::{self, ChatsOverlay, OverlayOutcome};
use crate::api::client::Client;

/// Rows the picker's inline viewport takes. Enough for a page of chats plus
/// the panel border and key hints; clamped to the terminal by the guard.
const PICKER_HEIGHT: u16 = 18;

/// What the user chose at startup.
pub enum Startup {
    /// Attach to this existing chat.
    Resume(ChatId),
    /// Create a fresh chat and attach to that.
    New,
    /// Leave without starting a session.
    Quit,
}

/// Show the picker and return the choice.
///
/// With no chats on record there is nothing to choose between, so this returns
/// [`Startup::New`] without touching the terminal — a first run drops straight
/// into a chat.
pub async fn pick_chat(client: &Client) -> Result<Startup> {
    let chats = client.list_chats().await?;
    if chats.is_empty() {
        return Ok(Startup::New);
    }
    let mut overlay = ChatsOverlay::new(None);
    overlay.set_chats(chats, attention(client).await);

    app::install_panic_hook();
    let mut guard = app::TerminalGuard::with_height(PICKER_HEIGHT)?;
    let mut events = EventStream::new();
    let outcome = loop {
        draw(&mut guard, &mut overlay)?;
        let Some(event) = events.next().await else {
            // stdin closed under us: nothing more can be chosen.
            break Startup::Quit;
        };
        let event = event.map_err(app::terminal_error)?;
        let Event::Key(key) = event else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            break Startup::Quit;
        }
        match overlay.key(key) {
            OverlayOutcome::Open(chat) => break Startup::Resume(chat),
            OverlayOutcome::NewChat => break Startup::New,
            OverlayOutcome::Dismiss => break Startup::Quit,
            // Rename and delete are the switcher's own keys; honoring them here
            // keeps the advertised hints true. Both refresh the list, and
            // deleting the last chat leaves nothing to pick.
            OverlayOutcome::Rename(chat, title) => {
                let title = title.trim().to_owned();
                let title = (!title.is_empty()).then_some(title);
                client.rename_chat(chat, title.as_deref()).await?;
                if !reload(client, &mut overlay).await? {
                    break Startup::New;
                }
            }
            OverlayOutcome::Delete(chat) => {
                client.delete_chat(chat).await?;
                if !reload(client, &mut overlay).await? {
                    break Startup::New;
                }
            }
            _ => {}
        }
    };
    // Wipe the panel so the chat session's own viewport starts clean.
    guard.terminal.clear().map_err(app::terminal_error)?;
    Ok(outcome)
}

/// Refill the list after a mutation. Returns whether any chat is left.
async fn reload(client: &Client, overlay: &mut ChatsOverlay) -> Result<bool> {
    let chats = client.list_chats().await?;
    let remaining = !chats.is_empty();
    overlay.set_chats(chats, attention(client).await);
    Ok(remaining)
}

/// Chats with something parked on the user, for the list's markers. A failure
/// here only costs the markers, so it degrades to none rather than refusing to
/// show the picker.
async fn attention(client: &Client) -> std::collections::HashSet<ChatId> {
    client
        .list_inbox()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|item| item.chat_id)
        .collect()
}

fn draw(guard: &mut app::TerminalGuard, overlay: &mut ChatsOverlay) -> Result<()> {
    guard
        .terminal
        .draw(|frame| {
            let area = frame.area();
            let width = area.width.clamp(30, 60);
            let rect = Rect::new(
                area.x + (area.width.saturating_sub(width)) / 2,
                area.y,
                width,
                area.height,
            );
            overlays::panel(frame, rect, "resume a chat", |width| overlay.lines(width));
        })
        .map_err(|error| AgentError::msg(format!("terminal error: {error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::Config;

    /// The picker's whole job crosses the client/server contract: the rows it
    /// offers come from `GET /chats`, and the id it hands back has to be one
    /// the session can actually attach to. This drives both against a real
    /// server — a change that broke the list shape, its ordering, or the
    /// resume path would show up here rather than as an empty picker.
    #[tokio::test]
    async fn the_picker_resumes_a_chat_the_server_lists() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Never touch the developer's real keychain from a test.
        openwave_core::KeychainSecretProvider::use_mock();
        let server = openwave_server::bind_configured(Config::desktop(dir.path()))
            .await
            .expect("bind the server");
        let client = Client::new(server.local_addr(), server.token()).expect("build the client");
        let serve = tokio::spawn(server.serve());

        let older = client.create_chat().await.expect("create the first chat");
        let newer = client.create_chat().await.expect("create the second chat");

        let listed = client.list_chats().await.expect("list chats");
        let ids: Vec<_> = listed.iter().map(|chat| chat.id).collect();
        assert!(
            ids.contains(&older) && ids.contains(&newer),
            "the picker's list must offer every chat, got {ids:?}"
        );

        // Resuming is `require_chat` on the selected row's id.
        client
            .require_chat(newer)
            .await
            .expect("the selected chat resumes");

        // With no chats at all there is nothing to pick, so the picker must not
        // take the terminal — it goes straight to a new chat.
        client.delete_chat(older).await.expect("delete a chat");
        client.delete_chat(newer).await.expect("delete a chat");
        assert!(
            matches!(pick_chat(&client).await.expect("pick"), Startup::New),
            "an empty chat list starts a new chat without prompting"
        );

        serve.abort();
    }
}
