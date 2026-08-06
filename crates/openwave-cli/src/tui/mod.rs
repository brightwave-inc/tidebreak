//! `openwave tui` — interactive terminal chat.
//!
//! Boots the same in-process server `openwave serve` runs, spawns its accept
//! loop on a background task, and drives it over the loopback HTTP+WebSocket
//! API with the per-launch bearer token — the contract the desktop webview
//! consumes. Logging is file-only: the TUI owns the terminal.

mod app;
mod client;
mod render;
mod wire;

use client::Client;
use openwave_core::{ChatId, Result};

/// Boot the server, open (or resume) the chat, and run the interactive loop.
pub async fn run(chat: Option<ChatId>) -> Result<()> {
    let config = crate::profile_config()?;
    openwave_server::logging::init_logging_file_only(&config.data_dir);
    let server = openwave_server::bind_configured(config).await?;
    let addr = server.local_addr();
    let token = server.token().to_owned();
    // `serve` takes ownership of the Server, whose drop aborts the background
    // workers (turn worker etc.); keeping this JoinHandle alive for the whole
    // session is what keeps the engine running.
    let serve = tokio::spawn(server.serve());

    let client = Client::new(addr, &token)?;
    let chat = match chat {
        Some(chat) => {
            client.require_chat(chat).await?;
            chat
        }
        None => client.create_chat().await?,
    };
    let result = app::run(client, chat).await;
    serve.abort();
    result
}
