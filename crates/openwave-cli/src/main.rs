//! OpenWave CLI — the headless daemon and MCP server.
//!
//! `openwave serve` boots the in-process HTTP/WebSocket surface on a loopback
//! port and prints the address and per-launch bearer token it minted, so a local
//! client (the desktop shell, or a script) can connect. Configuration comes from
//! the environment via [`Config::from_env`] (`OPENWAVE_PROFILE`,
//! `OPENWAVE_DATA_DIR`, `OPENWAVE_CONTAINER_EXECUTION_ENABLED`, and
//! `OPENWAVE_CONTAINER_IMAGE`); the model API key comes from
//! `ANTHROPIC_API_KEY`, and `OPENWAVE_MCP_CONFIG` may name an external
//! stdio-server configuration file.
//!
//! `openwave mcp <workspace>` serves the built-in read-only filesystem tools over
//! MCP stdio, confined to the explicit workspace directory.
//!
//! `openwave rehome-secrets` rewrites the profile's stored credentials so their
//! keychain items belong to the running binary's code signature, which is what
//! stops macOS asking for credentials an earlier build created.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;

use openwave_core::{AgentError, ChatId, Config, ListDir, ReadFile, Result, ToolCtx, ToolRegistry};

const USAGE: &str =
    "usage: openwave serve\n       openwave mcp <workspace>\n       openwave rehome-secrets";

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
        Some(command) if command == OsStr::new("rehome-secrets") => {
            if args.next().is_some() {
                usage_error("rehome-secrets does not accept arguments");
            }
            rehome_secrets().await
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

/// Configuration for the profile this build talks to.
///
/// Debug builds keep their own keychain service, matching the desktop's
/// dev/release split: a dev daemon must not mutate release secret state.
fn profile_config() -> Result<Config> {
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut config = Config::from_env()?;
    #[cfg(debug_assertions)]
    {
        config.keychain_service = Some("openwave.dev".into());
    }
    Ok(config)
}

/// Bind the server and run its accept loop, announcing where to reach it.
async fn serve() -> Result<()> {
    let config = profile_config()?;
    let server = openwave_server::bind_configured(config).await?;
    // The address and token are the client's entry point: the parent process that
    // launched the daemon reads them from stdout to connect. The token is a secret,
    // so an integrator should capture this process's stdout directly (a piped
    // child) rather than run the daemon under a logging supervisor.
    println!("openwave: listening on http://{}", server.local_addr());
    println!("openwave: token {}", server.token());
    server.serve().await
}

/// Rewrite each stored credential so the item belongs to this binary's code
/// signature.
///
/// macOS keeps prompting for credentials an earlier, differently signed build
/// created — see [`openwave_server::secret_rehome`] for why an approval given at
/// that prompt does not survive the next rebuild. Run this through Cargo
/// (`cargo run -p openwave-cli -- rehome-secrets`) so the dev signing runner
/// applies; each credential asks for access once more, and then stops asking.
async fn rehome_secrets() -> Result<()> {
    use openwave_server::secret_rehome::RehomeOutcome;

    let config = profile_config()?;
    let mut touched = 0usize;
    let mut lost = 0usize;
    for (key, outcome) in openwave_server::rehome_configured_secrets(&config).await? {
        match outcome {
            RehomeOutcome::Absent => {}
            RehomeOutcome::Rehomed => {
                touched += 1;
                println!("openwave: re-homed {key}");
            }
            RehomeOutcome::Skipped(reason) => {
                touched += 1;
                eprintln!("openwave: left {key} as it was — {reason}");
            }
            RehomeOutcome::Lost(reason) => {
                touched += 1;
                lost += 1;
                eprintln!("openwave: lost {key} — {reason}; store this credential again");
            }
        }
    }
    if touched == 0 {
        println!("openwave: no stored credentials to re-home");
    }
    if lost > 0 {
        return Err(AgentError::msg(format!(
            "{lost} credential(s) were removed but could not be stored again"
        )));
    }
    Ok(())
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
