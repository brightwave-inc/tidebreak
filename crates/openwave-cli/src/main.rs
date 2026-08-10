//! OpenWave CLI — the headless daemon and MCP server.
//!
//! `openwave serve` boots the in-process HTTP/WebSocket surface and prints the
//! address it bound, so a local client (the desktop shell, or a script) can
//! connect. It binds a loopback, ephemeral port by default, and prints the
//! per-launch bearer token it minted alongside the address — except on the
//! self-host profile, where that token authenticates nobody and the address
//! may be set with `OPENWAVE_LISTEN_ADDR`. Configuration comes from
//! the environment via [`Config::from_env`] (`OPENWAVE_PROFILE`,
//! `OPENWAVE_DATA_DIR`, `OPENWAVE_CONTAINER_EXECUTION_ENABLED`,
//! `OPENWAVE_CONTAINER_IMAGE`, and `OPENWAVE_LISTEN_ADDR`); the model API key
//! comes from `ANTHROPIC_API_KEY`, and `OPENWAVE_MCP_CONFIG` may name an
//! external stdio-server configuration file.
//!
//! `openwave mcp <workspace>` serves the built-in read-only filesystem tools over
//! MCP stdio, confined to the explicit workspace directory.
//!
//! `openwave rehome-secrets` rewrites the profile's stored credentials so their
//! keychain items belong to the running binary's code signature, which is what
//! stops macOS asking for credentials an earlier build created.
//!
//! `openwave tui [--chat <id> | --new]` runs an interactive terminal chat: it
//! boots the same in-process server as `serve` and drives it over the loopback
//! HTTP+WS API. With neither flag it opens a picker over the existing chats, so
//! a session can be resumed without knowing its id; `--new` skips straight to a
//! fresh chat, and `--chat` resumes one by id.
//!
//! `openwave -p "<prompt>"` runs one turn without a terminal: same engine, no
//! terminal. stdout carries the assistant's text (or, with
//! `--output-format json`, the turn's event stream as NDJSON), and the exit
//! status says whether the turn completed. `--permission-mode` sets the chat's
//! permission mode for the run, and under `--output-format json` a driving
//! process answers approvals, plans, and questions over stdin — see
//! [`print::protocol`].

use std::ffi::OsStr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use openwave_core::{
    AgentError, ChatId, Config, ListDir, Profile, ReadFile, Result, ToolCtx, ToolRegistry,
};

mod api;
mod print;
mod tui;

use print::OutputFormat;

const USAGE: &str = "usage: openwave serve\n       openwave mcp <workspace>\n       openwave \
                     rehome-secrets\n       openwave tui [--chat <id> | --new]\n       openwave -p \
                     <prompt> [--chat <id>] [--output-format text|json]\n                  \
                     [--permission-mode ask|auto|allow|plan]";

#[tokio::main]
async fn main() {
    match run().await {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            // Print the error's `Display` (e.g. "configuration error: …"), not
            // the `Debug` form the stdlib `Termination` impl would show.
            eprintln!("openwave: {error}");
            std::process::exit(1);
        }
    }
}

/// Dispatch one command, returning the process exit status. Only print mode
/// reports anything but `0`, since only it distinguishes a failure of the
/// command from a failure of the work the command drove.
async fn run() -> Result<i32> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        // Default to `serve` so a bare `openwave` runs the daemon.
        None => serve().await.map(|()| 0),
        Some(command) if command == OsStr::new("serve") => {
            if args.next().is_some() {
                usage_error("serve does not accept arguments");
            }
            serve().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("mcp") => {
            let Some(workspace) = args.next() else {
                usage_error("mcp requires a workspace path");
            };
            if args.next().is_some() {
                usage_error("mcp accepts exactly one workspace path");
            }
            serve_mcp(workspace.into()).await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("rehome-secrets") => {
            if args.next().is_some() {
                usage_error("rehome-secrets does not accept arguments");
            }
            rehome_secrets().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("tui") => {
            let mut open = None;
            while let Some(flag) = args.next() {
                if flag == OsStr::new("--chat") {
                    let Some(id) = args.next() else {
                        usage_error("tui --chat requires a chat id");
                    };
                    let Ok(chat) = ChatId::from_str(&id.to_string_lossy()) else {
                        usage_error("tui --chat expects a chat UUID");
                    };
                    if open.is_some() {
                        usage_error("tui takes either --chat <id> or --new, not both");
                    }
                    open = Some(tui::Open::Chat(chat));
                } else if flag == OsStr::new("--new") {
                    if open.is_some() {
                        usage_error("tui takes either --chat <id> or --new, not both");
                    }
                    open = Some(tui::Open::New);
                } else {
                    usage_error("tui accepts only --chat <id> or --new");
                }
            }
            tui::run(open.unwrap_or(tui::Open::Pick)).await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("-p") || command == OsStr::new("--print") => {
            let Some(prompt) = args.next() else {
                usage_error("-p requires a prompt");
            };
            let Some(prompt) = prompt.to_str().map(str::to_owned) else {
                usage_error("-p expects a UTF-8 prompt");
            };
            let mut chat = None;
            let mut format = OutputFormat::Text;
            let mut permission_mode = None;
            while let Some(flag) = args.next() {
                if flag == OsStr::new("--chat") {
                    let Some(id) = args.next() else {
                        usage_error("--chat requires a chat id");
                    };
                    match ChatId::from_str(&id.to_string_lossy()) {
                        Ok(id) => chat = Some(id),
                        Err(_) => usage_error("--chat expects a chat UUID"),
                    }
                } else if flag == OsStr::new("--output-format") {
                    let Some(value) = args.next() else {
                        usage_error("--output-format requires text or json");
                    };
                    match OutputFormat::parse(&value.to_string_lossy()) {
                        Some(value) => format = value,
                        None => usage_error("--output-format expects text or json"),
                    }
                } else if flag == OsStr::new("--permission-mode") {
                    let Some(value) = args.next() else {
                        usage_error("--permission-mode requires ask, auto, allow, or plan");
                    };
                    // The wire tokens are the chat's own permission-mode
                    // vocabulary; the CLI adds nothing to it.
                    match value.to_string_lossy().as_ref() {
                        mode @ ("ask" | "auto" | "allow" | "plan") => {
                            permission_mode = Some(mode.to_owned());
                        }
                        _ => usage_error("--permission-mode expects ask, auto, allow, or plan"),
                    }
                } else {
                    usage_error(&format!("unknown print-mode argument {flag:?}"));
                }
            }
            print::run(prompt, chat, format, permission_mode).await
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
    let profile = config.profile;
    // Tracing events land in `logs/openwave.log` under the profile data dir
    // (plus stderr in debug builds); see `openwave_server::logging`.
    openwave_server::logging::init_logging(&config.data_dir);
    let server = openwave_server::bind_configured(config).await?;
    // The address is the client's entry point: the parent process that launched
    // the daemon reads it from stdout to connect, and a container entrypoint
    // waits on the same line.
    println!("openwave: listening on http://{}", server.local_addr());
    // The token is a secret, so an integrator should capture this process's
    // stdout directly (a piped child) rather than run the daemon under a
    // logging supervisor. On self-host it is not printed at all: the
    // per-launch bearer names nobody on a shared deployment and authenticates
    // nobody there (see the server's `auth` module docs), so printing it into
    // a container's logs would only invite someone to try it.
    if profile != Profile::SelfHost {
        println!("openwave: token {}", server.token());
    }
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
