//! OpenWave CLI — the headless daemon and MCP server.
//!
//! `openwave serve` boots the in-process HTTP/WebSocket surface on a loopback
//! port and prints the address and per-launch bearer token it minted, so a local
//! client (the desktop shell, or a script) can connect. Configuration comes from
//! the environment via [`Config::from_env`] (`OPENWAVE_PROFILE`,
//! `OPENWAVE_DATA_DIR`); the model API key comes from `ANTHROPIC_API_KEY`, and
//! `OPENWAVE_MCP_CONFIG` may name an external stdio-server configuration file.
//!
//! `openwave mcp <workspace>` serves the built-in read-only filesystem tools over
//! MCP stdio, confined to the explicit workspace directory.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;

use openwave_core::{AgentError, ChatId, Config, ListDir, ReadFile, Result, ToolCtx, ToolRegistry};

const USAGE: &str = "usage: openwave serve\n       openwave mcp <workspace>";

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
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        // Default to `serve` so a bare `openwave` runs the daemon.
        None => serve().await,
        Some(command) if command == OsStr::new("serve") => {
            if args.next().is_some() {
                usage_error("serve does not accept arguments");
            }
            serve().await
        }
        Some(command) if command == OsStr::new("mcp") => {
            let Some(workspace) = args.next() else {
                usage_error("mcp requires a workspace path");
            };
            if args.next().is_some() {
                usage_error("mcp accepts exactly one workspace path");
            }
            serve_mcp(workspace.into()).await
        }
        Some(other) => {
            usage_error(&format!("unknown command {other:?}"));
        }
    }
}

fn usage_error(message: &str) -> ! {
    eprintln!("openwave: {message}\n\n{USAGE}");
    std::process::exit(2);
}

/// Bind the server and run its accept loop, announcing where to reach it.
async fn serve() -> Result<()> {
    let config = Config::from_env()?;
    let server = openwave_server::bind_configured(config).await?;
    // The address and token are the client's entry point: the parent process that
    // launched the daemon reads them from stdout to connect. The token is a secret,
    // so an integrator should capture this process's stdout directly (a piped
    // child) rather than run the daemon under a logging supervisor.
    println!("openwave: listening on http://{}", server.local_addr());
    println!("openwave: token {}", server.token());
    server.serve().await
}

/// Serve the built-in read-only filesystem tools over MCP stdio.
async fn serve_mcp(workspace: PathBuf) -> Result<()> {
    let ctx = ToolCtx::try_new_legacy_workspace(ChatId::new(), None, workspace.clone()).map_err(
        |error| {
            AgentError::config(format!(
                "could not open MCP workspace {}: {error}",
                workspace.display()
            ))
        },
    )?;

    let tools = Arc::new(
        ToolRegistry::new()
            .with(Box::new(ReadFile))
            .with(Box::new(ListDir)),
    );
    let server = openwave_mcp::McpServer::new(tools, ctx);
    openwave_mcp::serve_stdio(server)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")))
}
