//! `tidebreak tui` — interactive terminal chat.
//!
//! Boots the same in-process server `tidebreak serve` runs, spawns its accept
//! loop on a background task, and drives it over the HTTP+WebSocket API with
//! the per-launch bearer token — the contract the desktop webview consumes.
//! With `--server` it skips the boot and drives a server already running
//! instead. Logging is file-only: the TUI owns the terminal.

mod app;
mod composer;
mod markdown;
mod overlays;
mod render;
mod startup;
mod theme;

use crate::api::client::Client;
use tidebreak_core::{ChatId, Result};

/// How the session picks the chat to attach to.
pub enum Open {
    /// Resume this exact chat (`--chat <id>`).
    Chat(ChatId),
    /// Create a fresh chat without asking (`--new`).
    New,
    /// Offer the startup picker, falling back to a new chat when there is
    /// nothing to resume.
    Pick,
}

/// Boot the server, open (or resume) the chat, and run the interactive loop.
pub async fn run(open: Open, server: crate::connect::Server) -> Result<()> {
    // The session owns an embedded server's accept loop, and with it the
    // background workers; holding it for the whole TUI session is what keeps
    // the engine running. Attached, it owns nothing.
    let session = crate::connect::Session::open(&server).await?;
    // An embedded TUI is the same trusted client for its own connected-folder
    // tool calls that `-p` and `serve` are, for the same reason: it owns this
    // machine's broker state for the length of the session. Without this a turn
    // that read a connected folder would park here too. Attached, it starts
    // nothing — see [`crate::folder_executor`].
    let folder_executor = match session.client_executor_token() {
        Some(executor_token) => crate::folder_executor::FolderExecutor::new(
            session.client().clone(),
            Some(executor_token),
            &crate::profile_config()?.data_dir,
        )?
        .map(|executor| {
            // Any conversation, not just the one opened at startup: the session
            // can move between chats.
            tokio::spawn(executor.run(crate::folder_executor::Scope::AllChats))
        }),
        None => None,
    };
    let outcome = attach(session.client().clone(), open).await;
    if let Some(executor) = folder_executor {
        executor.abort();
    }
    outcome
}

/// Resolve which chat to attach to, then hand the terminal to the app.
async fn attach(client: Client, open: Open) -> Result<()> {
    let (chat, resumed) = match open {
        Open::Chat(chat) => {
            client.require_chat(chat).await?;
            (chat, true)
        }
        Open::New => (client.create_chat().await?, false),
        Open::Pick => match startup::pick_chat(&client).await? {
            startup::Startup::Resume(chat) => (chat, true),
            startup::Startup::New => (client.create_chat().await?, false),
            // The user left at the picker; no chat is created for a session
            // that never started.
            startup::Startup::Quit => return Ok(()),
        },
    };
    app::run(client, chat, resumed).await
}
