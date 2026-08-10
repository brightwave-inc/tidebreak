//! `openwave tui` — interactive terminal chat.
//!
//! Boots the same in-process server `openwave serve` runs, spawns its accept
//! loop on a background task, and drives it over the loopback HTTP+WebSocket
//! API with the per-launch bearer token — the contract the desktop webview
//! consumes. Logging is file-only: the TUI owns the terminal.

mod app;
mod composer;
mod markdown;
mod overlays;
mod render;
mod startup;
mod theme;

use crate::api::client::Client;
use openwave_core::{ChatId, Result};

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
pub async fn run(open: Open) -> Result<()> {
    let config = crate::profile_config()?;
    openwave_server::logging::init_logging_file_only(&config.data_dir);
    let server = openwave_server::bind_configured(config).await?;
    let addr = server.local_addr();
    let token = server.token().to_owned();
    // `serve` takes ownership of the Server, whose drop aborts the background
    // workers (turn worker etc.); keeping this JoinHandle alive for the whole
    // session is what keeps the engine running.
    let serve = tokio::spawn(server.serve());

    let result = attach(Client::new(addr, &token)?, open).await;
    serve.abort();
    result
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
