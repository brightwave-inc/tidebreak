//! Scriptable setup: `tidebreak provider|model|settings|mcp-server|chat|agent-run`.
//!
//! `chat steer` posts more user text into an active turn; `turn` is the durable
//! turn identity from the chat event stream.
//!
//! Every command here is a thin client of a route the server already serves —
//! the same ones the desktop settings pages call — so configuring Tidebreak
//! from a script and configuring it from the app converge on one
//! implementation. Nothing in this module decides anything: it opens a session
//! (embedded by default, attached with `--server`), makes the call, and renders
//! the answer.
//!
//! Secrets are read from stdin or from a named environment variable, never
//! from a command-line argument: argv is readable by every process on the
//! machine, and a key pasted into a shell command also lands in its history.

use std::io::Read as _;

use tidebreak_core::{AgentActivityDetail, AgentError, AgentRunId, ChatId, Result, TurnId};

use crate::api::client::Client;
use crate::api::wire::{McpServerInfo, McpServersInfo};
use crate::print::OutputFormat;

/// Where a command reads secret material from.
pub enum SecretSource {
    /// Everything on stdin, up to EOF (the default).
    Stdin,
    /// The value of a named environment variable.
    Env(String),
}

impl SecretSource {
    /// Read the secret, refusing an empty one — an empty key stored silently
    /// is a provider that fails later for no visible reason.
    fn read(&self) -> Result<String> {
        let value = match self {
            Self::Stdin => {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .map_err(|error| AgentError::msg(format!("could not read stdin: {error}")))?;
                buffer
            }
            Self::Env(name) => std::env::var(name)
                .map_err(|_| AgentError::msg(format!("{name} is not set in the environment")))?,
        };
        // Trailing newlines come with `echo`, heredocs, and editors; a key
        // never legitimately has surrounding whitespace.
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(AgentError::msg(match self {
                Self::Stdin => "no secret on stdin".to_owned(),
                Self::Env(name) => format!("{name} is empty"),
            }));
        }
        Ok(value)
    }
}

/// One setup command, already parsed.
pub enum Command {
    /// Every provider kind, with credential status.
    ProviderList,
    /// Store an API key for a provider and enable it.
    ProviderSetKey { kind: String, secret: SecretSource },
    /// Remove a provider's stored credential.
    ProviderRemoveKey { kind: String },
    /// The model catalog and its availability.
    ModelList,
    /// Every model role, its selection, and what it resolves to.
    ModelRoles,
    /// Pin a role to a catalog key, or clear it back to automatic.
    ModelSelect {
        role: String,
        selection: Option<String>,
    },
    /// Runtime settings plus web-search and code-execution configuration.
    SettingsShow,
    /// Select the host web-search provider, or turn host search off.
    WebSearchSelect { provider: Option<String> },
    /// Store a web-search provider's key.
    WebSearchSetKey {
        provider: String,
        secret: SecretSource,
    },
    /// Remove a web-search provider's key.
    WebSearchRemoveKey { provider: String },
    /// Select the code-execution backend, or disable execution.
    ExecSelect { provider: Option<String> },
    /// Store a code-execution provider's key.
    ExecSetKey {
        provider: String,
        secret: SecretSource,
    },
    /// Remove a code-execution provider's key.
    ExecRemoveKey { provider: String },
    /// Every mounted MCP server.
    McpList,
    /// Add one user-configured MCP server.
    McpAdd { definition: serde_json::Value },
    /// Remove one user-configured MCP server by name.
    McpRemove { name: String },
    /// Every chat, most recently active first.
    ChatList,
    /// A fresh chat (server-side defaults seed the rest).
    ChatCreate,
    /// Delete a chat outright.
    ChatDelete { chat: ChatId },
    /// Steer an active turn with more user text.
    ///
    /// `turn` is the durable turn identity from the chat's event stream (not
    /// an agent-run id). A fresh steer id is minted per call.
    ChatSteer {
        chat: ChatId,
        turn: TurnId,
        content: String,
    },
    /// Background (and foreground) agent runs for one chat.
    AgentRunList { chat: ChatId },
    /// One run's status plus its ordered activity timeline.
    AgentRunShow { chat: ChatId, run: AgentRunId },
    /// Ask a background run to stop.
    AgentRunCancel { chat: ChatId, run: AgentRunId },
}

/// Run one setup command against the profile's server, and shut down anything
/// this process started for it.
///
/// The embedded server is the same one `tidebreak serve` and `-p` bind, on the
/// same profile and data directory, so a credential stored here is the
/// credential the next turn resolves. With `--server` the command runs against
/// a server already holding that data directory instead — the same routes, the
/// same effect on the same profile.
pub async fn run(
    command: Command,
    format: OutputFormat,
    server: crate::connect::Server,
) -> Result<()> {
    let session = crate::connect::Session::open(&server).await?;
    execute(session.client(), command, format).await
}

/// Make the call and render it. Split from [`run`] so a test can drive several
/// commands against one server, the way a script drives one profile.
async fn execute(client: &Client, command: Command, format: OutputFormat) -> Result<()> {
    match command {
        Command::ProviderList => {
            let providers = client.list_providers().await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "providers": providers_json(&providers) }));
            }
            for provider in providers {
                let credential = match (provider.has_credential, provider.auth_mode) {
                    (false, _) => "no credential".to_owned(),
                    (true, Some(mode)) => format!("credential: {}", mode.as_str()),
                    (true, None) => "credential: stored".to_owned(),
                };
                let base_url = provider
                    .base_url
                    .map(|url| format!("  {url}"))
                    .unwrap_or_default();
                let enabled = if provider.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                println!("{:<20} {enabled:<9} {credential}{base_url}", provider.kind);
            }
        }
        Command::ProviderSetKey { kind, secret } => {
            let key = secret.read()?;
            let info = client.set_provider_api_key(&kind, &key).await?;
            if format == OutputFormat::Json {
                return emit(&info);
            }
            println!("tidebreak: stored the {kind} API key and enabled the provider");
        }
        Command::ProviderRemoveKey { kind } => {
            client.delete_provider_credential(&kind).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "kind": kind, "has_credential": false }));
            }
            println!("tidebreak: removed the {kind} credential");
        }
        Command::ModelList => {
            let catalog = client.list_models().await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "models": catalog.models.iter().map(|model| serde_json::json!({
                        "key": model.key,
                        "id": model.id,
                        "display_name": model.display_name,
                        "provider": model.provider,
                        "available": model.available,
                        "context_window": model.context_window,
                        "reasoning_efforts": model.reasoning_efforts,
                    })).collect::<Vec<_>>(),
                }));
            }
            for model in catalog.models {
                let availability = if model.available {
                    "available"
                } else {
                    "unavailable"
                };
                println!(
                    "{:<44} {availability:<12} {}",
                    model.key, model.display_name
                );
            }
        }
        Command::ModelRoles => {
            let catalog = client.list_models().await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "roles": catalog.roles.iter().map(|role| serde_json::json!({
                        "role": role.role,
                        "selection": role.selection,
                        "resolved_key": role.resolved_key,
                    })).collect::<Vec<_>>(),
                }));
            }
            for role in catalog.roles {
                let selection = role.selection.as_deref().unwrap_or("automatic");
                let resolved = role.resolved_key.as_deref().unwrap_or("nothing");
                println!("{:<12} {selection:<44} resolves to {resolved}", role.role);
            }
        }
        Command::ModelSelect { role, selection } => {
            let info = client.set_model_role(&role, selection.as_deref()).await?;
            if format == OutputFormat::Json {
                return emit(&info);
            }
            match selection {
                Some(selection) => println!("tidebreak: {role} now uses {selection}"),
                None => println!("tidebreak: {role} is back to automatic"),
            }
        }
        Command::SettingsShow => {
            let settings = client.get_settings().await?;
            let web_search = client.get_web_search_config().await?;
            let web_search_credentials = client.get_web_search_credentials().await?;
            let code_execution = client.get_code_execution_config().await?;
            let code_execution_credentials = client.get_code_execution_credentials().await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "settings": settings,
                    "web_search": { "config": web_search, "credentials": web_search_credentials },
                    "code_execution": {
                        "config": code_execution,
                        "credentials": code_execution_credentials,
                    },
                }));
            }
            println!("model                {}", show(&settings["model"]));
            println!("model credential     {}", show(&settings["has_api_key"]));
            println!(
                "background agents    {}",
                show(&settings["max_active_background_agents"])
            );
            println!("web search           {}", show(&web_search["provider"]));
            println!("  mode               {}", show(&web_search["mode"]));
            println!("  available          {}", show(&web_search["available"]));
            println!(
                "  keys stored        {}",
                credentialed(&web_search_credentials)
            );
            println!("code execution       {}", show(&code_execution["provider"]));
            println!(
                "  available          {}",
                show(&code_execution["available"])
            );
            if let Some(reason) = code_execution.get("unavailable_reason") {
                println!("  unavailable        {}", show(reason));
            }
            println!(
                "  keys stored        {}",
                credentialed(&code_execution_credentials)
            );
        }
        Command::WebSearchSelect { provider } => {
            let info = client.set_web_search_provider(provider.as_deref()).await?;
            if format == OutputFormat::Json {
                return emit(&info);
            }
            match provider {
                Some(provider) => println!("tidebreak: web search now uses {provider}"),
                None => println!("tidebreak: host web search is off"),
            }
        }
        Command::WebSearchSetKey { provider, secret } => {
            let key = secret.read()?;
            let readiness = client.set_web_search_credential(&provider, &key).await?;
            if format == OutputFormat::Json {
                return emit(&readiness);
            }
            println!("tidebreak: stored the {provider} web-search key");
        }
        Command::WebSearchRemoveKey { provider } => {
            let readiness = client.delete_web_search_credential(&provider).await?;
            if format == OutputFormat::Json {
                return emit(&readiness);
            }
            println!("tidebreak: removed the {provider} web-search key");
        }
        Command::ExecSelect { provider } => {
            let info = client
                .set_code_execution_provider(provider.as_deref())
                .await?;
            if format == OutputFormat::Json {
                return emit(&info);
            }
            match provider {
                Some(provider) => println!("tidebreak: code execution now uses {provider}"),
                None => println!("tidebreak: code execution is off"),
            }
        }
        Command::ExecSetKey { provider, secret } => {
            let key = secret.read()?;
            let readiness = client
                .set_code_execution_credential(&provider, &key)
                .await?;
            if format == OutputFormat::Json {
                return emit(&readiness);
            }
            println!("tidebreak: stored the {provider} code-execution key");
        }
        Command::ExecRemoveKey { provider } => {
            let readiness = client.delete_code_execution_credential(&provider).await?;
            if format == OutputFormat::Json {
                return emit(&readiness);
            }
            println!("tidebreak: removed the {provider} code-execution key");
        }
        Command::McpList => {
            let listing = client.get_mcp_servers().await?;
            if format == OutputFormat::Json {
                return emit(&listing);
            }
            for server in decode_mcp(&listing)?.servers {
                print_mcp_server(&server);
            }
        }
        Command::McpAdd { definition } => {
            let name = definition["name"].as_str().unwrap_or_default().to_owned();
            let mut servers = configured_servers(&client.get_mcp_servers().await?)?;
            if servers
                .iter()
                .any(|server| server["name"].as_str() == Some(name.as_str()))
            {
                return Err(AgentError::msg(format!(
                    "an MCP server named {name} is already configured; remove it first"
                )));
            }
            servers.push(definition);
            let listing = client
                .put_mcp_servers(serde_json::Value::Array(servers))
                .await?;
            if format == OutputFormat::Json {
                return emit(&listing);
            }
            println!("tidebreak: added the MCP server {name}");
        }
        Command::McpRemove { name } => {
            let listing = client.get_mcp_servers().await?;
            if let Some(plugin) = decode_mcp(&listing)?.servers.iter().find_map(|server| {
                (server.definition.name == name)
                    .then(|| server.definition.plugin.clone())
                    .flatten()
            }) {
                return Err(AgentError::msg(format!(
                    "the MCP server {name} comes from the plugin {plugin}; \
                     turn that plugin off instead"
                )));
            }
            let mut servers = configured_servers(&listing)?;
            let before = servers.len();
            servers.retain(|server| server["name"].as_str() != Some(name.as_str()));
            if servers.len() == before {
                return Err(AgentError::msg(format!("no MCP server named {name}")));
            }
            let listing = client
                .put_mcp_servers(serde_json::Value::Array(servers))
                .await?;
            if format == OutputFormat::Json {
                return emit(&listing);
            }
            println!("tidebreak: removed the MCP server {name}");
        }
        Command::ChatList => {
            let chats = client.list_chats().await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "chats": chats.iter().map(|chat| serde_json::json!({
                        "id": chat.id,
                        "title": chat.title,
                        "model": chat.model,
                        "permission_mode": chat.permission_mode,
                        "created_at": chat.created_at,
                    })).collect::<Vec<_>>(),
                }));
            }
            if chats.is_empty() {
                eprintln!("tidebreak: no chats");
                return Ok(());
            }
            for chat in chats {
                let title = chat.title.as_deref().unwrap_or("(untitled)");
                let model = chat.model.as_deref().unwrap_or("-");
                println!("{:<36}  {title:<40}  {model}", chat.id);
            }
        }
        Command::ChatCreate => {
            let chat = client.create_chat().await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "id": chat }));
            }
            // stdout is the id alone so a script can capture it; the label
            // rides stderr the same way `-p` announces a freshly created chat.
            println!("{chat}");
            eprintln!("tidebreak: chat {chat}");
        }
        Command::ChatDelete { chat } => {
            client.delete_chat(chat).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "id": chat, "deleted": true }));
            }
            println!("tidebreak: deleted chat {chat}");
        }
        Command::ChatSteer {
            chat,
            turn,
            content,
        } => {
            // Mint a steer id, then interrupt the active turn with the new
            // user text.
            let steer_id = TurnId::new();
            client.steer(chat, turn, steer_id, &content).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "chat": chat,
                    "turn": turn,
                    "steer_id": steer_id,
                    "steered": true,
                }));
            }
            println!("tidebreak: steered turn {turn}");
        }
        Command::AgentRunList { chat } => {
            let runs = client.list_agent_runs(chat).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "runs": runs.iter().map(agent_run_json).collect::<Vec<_>>(),
                }));
            }
            if runs.is_empty() {
                eprintln!("tidebreak: no agent runs in this chat");
                return Ok(());
            }
            for run in runs {
                let tier = run.tier.as_str();
                let task = run
                    .task
                    .as_deref()
                    .map(first_line)
                    .unwrap_or_else(|| "-".to_owned());
                let outs: Vec<&str> = run
                    .submitted_outputs
                    .iter()
                    .map(|output| output.filename.as_str())
                    .collect();
                let outs = if outs.is_empty() {
                    "-".to_owned()
                } else {
                    outs.join(",")
                };
                println!(
                    "{:<36}  {tier:<10}  {:<12}  outs={outs}  {task}",
                    run.id,
                    run.status.as_str()
                );
            }
        }
        Command::AgentRunShow { chat, run } => {
            let runs = client.list_agent_runs(chat).await?;
            let Some(snapshot) = runs.into_iter().find(|entry| entry.id == run) else {
                return Err(AgentError::msg(format!(
                    "no agent run {run} in chat {chat}"
                )));
            };
            let activity = client.list_agent_run_activity(chat, run).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "run": agent_run_json(&snapshot),
                    "activity": activity.iter().map(|item| serde_json::json!({
                        "kind": item.kind.as_str(),
                        "outcome": item.outcome,
                        "at": item.at,
                        "detail": item.detail,
                    })).collect::<Vec<_>>(),
                }));
            }
            println!("id                  {}", snapshot.id);
            if let Some(parent) = snapshot.parent_id {
                println!("parent              {parent}");
            }
            println!("tier                {}", snapshot.tier.as_str());
            println!(
                "execution_location  {}",
                snapshot.execution_location.as_str()
            );
            println!(
                "code_execution_provider  {}",
                snapshot.code_execution_provider.as_str()
            );
            println!("status              {}", snapshot.status.as_str());
            if let Some(error) = &snapshot.last_error_code {
                println!("last_error_code     {error}");
            }
            if let Some(started) = snapshot.started_at {
                println!("started_at          {started}");
            }
            if let Some(finished) = snapshot.finished_at {
                println!("finished_at         {finished}");
            }
            if !snapshot.submitted_outputs.is_empty() {
                let names: Vec<_> = snapshot
                    .submitted_outputs
                    .iter()
                    .map(|output| output.filename.as_str())
                    .collect();
                println!("submitted_outputs   {}", names.join(", "));
            }
            if let Some(task) = &snapshot.task {
                println!("task\n{task}");
            }
            if let Some(terminal) = &snapshot.terminal_text {
                println!("terminal_text\n{terminal}");
            }
            if activity.is_empty() {
                eprintln!("tidebreak: no activity recorded for this run");
            } else {
                println!("activity ({} steps)", activity.len());
                for item in activity {
                    let headline = activity_headline(item.detail.as_ref());
                    println!(
                        "  {}  {}  {}{}",
                        item.at,
                        item.kind.as_str(),
                        item.outcome.as_str(),
                        headline
                    );
                }
            }
        }
        Command::AgentRunCancel { chat, run } => {
            client.cancel_agent_run(chat, run).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "chat": chat,
                    "run": run,
                    "cancelled": true,
                }));
            }
            println!("tidebreak: cancelling agent run {run}");
        }
    }
    Ok(())
}

/// Compact JSON object for one agent-run snapshot — enough for a driver to
/// branch on status and collect submitted filenames without the full dump.
fn agent_run_json(run: &crate::api::wire::AgentRunSnapshot) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "parent_id": run.parent_id,
        "tier": run.tier,
        "execution_location": run.execution_location,
        "code_execution_provider": run.code_execution_provider,
        "status": run.status,
        "task": run.task,
        "started_at": run.started_at,
        "finished_at": run.finished_at,
        "last_error_code": run.last_error_code,
        "submitted_outputs": run.submitted_outputs.iter().map(|output| serde_json::json!({
            "output_id": output.output_id,
            "filename": output.filename,
        })).collect::<Vec<_>>(),
        "terminal_text": run.terminal_text,
        "spawn_call_id": run.spawn_call_id,
        "created_at": run.created_at,
    })
}

/// First line of a multi-line task, truncated for a one-line listing.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or(text).trim();
    const LIMIT: usize = 72;
    if line.chars().count() <= LIMIT {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(LIMIT.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A short suffix for one activity row: the command or query when present.
fn activity_headline(detail: Option<&AgentActivityDetail>) -> String {
    match detail {
        Some(AgentActivityDetail::Exec { command, args, .. }) => {
            let args = args.iter().take(4).cloned().collect::<Vec<_>>().join(" ");
            if args.is_empty() {
                return format!("  {command}");
            }
            let clipped = if args.len() > 60 {
                format!("{}…", &args[..60])
            } else {
                args
            };
            format!("  {command} {clipped}")
        }
        Some(AgentActivityDetail::Search { query }) => {
            let clipped = if query.len() > 72 {
                format!("{}…", &query[..72])
            } else {
                query.clone()
            };
            format!("  {clipped}")
        }
        Some(AgentActivityDetail::File { .. }) | None => String::new(),
    }
}

/// Write one JSON object on stdout, matching print mode's one-object-per-line
/// shape so both surfaces can be read by the same consumer.
fn emit(value: &serde_json::Value) -> Result<()> {
    println!("{value}");
    Ok(())
}

/// Render one settings field for a person: an absent or null value reads as
/// `none` rather than as `null`.
fn show(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "none".to_owned(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The provider names in a `{ credentials: [{provider, has_credential}] }`
/// readiness body that actually hold a key.
fn credentialed(readiness: &serde_json::Value) -> String {
    if readiness["storage_available"] == serde_json::Value::Bool(false) {
        return readiness["unavailable_reason"]
            .as_str()
            .map(|reason| format!("unavailable ({reason})"))
            .unwrap_or_else(|| "unavailable".to_owned());
    }
    let stored: Vec<&str> = readiness["credentials"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["has_credential"] == serde_json::Value::Bool(true))
                .filter_map(|row| row["provider"].as_str())
                .collect()
        })
        .unwrap_or_default();
    if stored.is_empty() {
        "none".to_owned()
    } else {
        stored.join(", ")
    }
}

fn print_mcp_server(server: &McpServerInfo) {
    let definition = &server.definition;
    let transport = definition
        .command
        .as_deref()
        .or(definition.url.as_deref())
        .or(definition.gateway_endpoint.as_deref())
        .unwrap_or("-");
    let source = match &definition.plugin {
        Some(plugin) => format!(" (from the {plugin} plugin)"),
        None => String::new(),
    };
    let state = if definition.enabled {
        server.health.as_str()
    } else {
        "disabled"
    };
    println!(
        "{:<24} {state:<14} {:>3} tools  {transport}{source}",
        definition.name, server.tool_count
    );
    if let Some(diagnostic) = &server.diagnostic {
        println!("    {diagnostic}");
    }
}

fn decode_mcp(listing: &serde_json::Value) -> Result<McpServersInfo> {
    serde_json::from_value(listing.clone())
        .map_err(|error| AgentError::msg(format!("could not read the MCP server list: {error}")))
}

/// The user-configured servers from a `GET /mcp/servers` body, in the shape
/// `PUT /mcp/servers` accepts.
///
/// The route takes a complete replacement set of *user-configured* servers, so
/// editing one means sending the others back untouched. Two things have to come
/// off first: plugin-sourced servers, which the runtime rebuilds and the route
/// refuses in a body, and the live projection fields (health, tool counts),
/// which are not part of a definition.
fn configured_servers(listing: &serde_json::Value) -> Result<Vec<serde_json::Value>> {
    const PROJECTED: [&str; 4] = ["health", "tool_count", "diagnostic", "curated"];
    let servers = listing["servers"]
        .as_array()
        .ok_or_else(|| AgentError::msg("the MCP server list had no servers array"))?;
    Ok(servers
        .iter()
        .filter(|server| server["plugin"].is_null())
        .map(|server| {
            let mut server = server.clone();
            if let Some(fields) = server.as_object_mut() {
                for field in PROJECTED {
                    fields.remove(field);
                }
            }
            server
        })
        .collect())
}

/// Provider rows as JSON, carrying only what the listing shows — the
/// credential itself never leaves the server.
fn providers_json(providers: &[crate::api::wire::ProviderInfo]) -> Vec<serde_json::Value> {
    providers
        .iter()
        .map(|provider| {
            serde_json::json!({
                "kind": provider.kind,
                "enabled": provider.enabled,
                "has_credential": provider.has_credential,
                "auth_mode": provider.auth_mode,
                "base_url": provider.base_url,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::Config;
    use tidebreak_server::wire::ProviderKind;

    /// The point of the whole family: a credential stored through the CLI is
    /// live on the profile the next command (and the next turn) resolves
    /// against. Two commands run against one server, exactly as a setup script
    /// runs them against one profile — a write that landed somewhere the read
    /// path does not look would show up here.
    #[tokio::test]
    async fn a_key_set_through_the_cli_shows_up_as_a_stored_credential() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Never touch the developer's real keychain from a test.
        tidebreak_core::KeychainSecretProvider::use_mock();
        let server = tidebreak_server::bind_configured(Config::desktop(dir.path()))
            .await
            .expect("bind the server");
        let client = Client::new(server.local_addr(), server.token()).expect("build the client");
        let serve = tokio::spawn(server.serve());

        let before = client.list_providers().await.expect("list providers");
        let anthropic = before
            .iter()
            .find(|provider| provider.kind == ProviderKind::Anthropic)
            .expect("anthropic is a known provider");
        assert!(
            !anthropic.has_credential && !anthropic.enabled,
            "a fresh profile has no stored Anthropic credential"
        );

        // The env path rather than stdin: a test cannot hand the process a
        // stdin of its own, and both paths converge on `SecretSource::read`.
        std::env::set_var("TIDEBREAK_TEST_PROVIDER_KEY", "sk-test-123");
        execute(
            &client,
            Command::ProviderSetKey {
                kind: "anthropic".to_owned(),
                secret: SecretSource::Env("TIDEBREAK_TEST_PROVIDER_KEY".to_owned()),
            },
            OutputFormat::Text,
        )
        .await
        .expect("store the key");

        let after = client.list_providers().await.expect("list providers");
        let anthropic = after
            .iter()
            .find(|provider| provider.kind == ProviderKind::Anthropic)
            .expect("anthropic is a known provider");
        assert!(
            anthropic.has_credential && anthropic.enabled,
            "the stored key must make the provider credentialed and enabled, got {anthropic:?}"
        );

        execute(
            &client,
            Command::ProviderRemoveKey {
                kind: "anthropic".to_owned(),
            },
            OutputFormat::Text,
        )
        .await
        .expect("remove the key");
        let after = client.list_providers().await.expect("list providers");
        assert!(
            !after
                .iter()
                .any(|provider| provider.kind == ProviderKind::Anthropic && provider.has_credential),
            "removing the credential must take it out of the listing"
        );

        serve.abort();
    }

    /// `chat create` must return an id that `chat list` (and therefore
    /// `--chat`) can see on the same profile — otherwise scripts that capture
    /// the id and continue with `-p --chat` fail for no visible reason.
    #[tokio::test]
    async fn a_chat_created_through_the_cli_shows_up_in_the_listing() {
        let dir = tempfile::tempdir().expect("temp dir");
        tidebreak_core::KeychainSecretProvider::use_mock();
        let server = tidebreak_server::bind_configured(Config::desktop(dir.path()))
            .await
            .expect("bind the server");
        let client = Client::new(server.local_addr(), server.token()).expect("build the client");
        let serve = tokio::spawn(server.serve());

        assert!(
            client.list_chats().await.expect("list").is_empty(),
            "a fresh profile has no chats"
        );

        execute(&client, Command::ChatCreate, OutputFormat::Text)
            .await
            .expect("create a chat");
        let listed = client.list_chats().await.expect("list after create");
        assert_eq!(
            listed.len(),
            1,
            "create must leave exactly one chat visible"
        );
        let chat = listed[0].id;

        execute(&client, Command::ChatDelete { chat }, OutputFormat::Text)
            .await
            .expect("delete the chat");
        assert!(
            client
                .list_chats()
                .await
                .expect("list after delete")
                .is_empty(),
            "delete must take the chat out of the listing"
        );

        serve.abort();
    }
}
