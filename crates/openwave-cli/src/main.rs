//! OpenWave CLI — the headless daemon.
//!
//! `openwave serve` boots the in-process HTTP/WebSocket surface on a loopback
//! port and prints the address and per-launch bearer token it minted, so a local
//! client (the desktop shell, or a script) can connect. Configuration comes from
//! the environment via [`Config::from_env`] (`OPENWAVE_PROFILE`,
//! `OPENWAVE_DATA_DIR`); the model API key comes from `ANTHROPIC_API_KEY`.

use openwave_core::{Config, Result};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // Print the error's `Display` (e.g. "configuration error: …"), not the
        // `Debug` form the stdlib `Termination` impl would show, and exit non-zero.
        eprintln!("openwave: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        // Default to `serve` so a bare `openwave` runs the daemon.
        Some("serve") | None => serve().await,
        Some(other) => {
            eprintln!("openwave: unknown command {other:?}\n\nusage: openwave serve");
            std::process::exit(2);
        }
    }
}

/// Bind the server and run its accept loop, announcing where to reach it.
async fn serve() -> Result<()> {
    let config = Config::from_env()?;
    let server = openwave_server::bind(config).await?;
    // The address and token are the client's entry point: the parent process that
    // launched the daemon reads them from stdout to connect. The token is a secret,
    // so an integrator should capture this process's stdout directly (a piped
    // child) rather than run the daemon under a logging supervisor.
    println!("openwave: listening on http://{}", server.local_addr());
    println!("openwave: token {}", server.token());
    server.serve().await
}
