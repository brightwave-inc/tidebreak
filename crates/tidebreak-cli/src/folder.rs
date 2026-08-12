//! `tidebreak folder …` — operator-provisioned consent for connected folders.
//!
//! Connecting a folder is host-machine consent. The desktop captures it in a
//! native picker and records it as [`ConsentMethod::FolderPicker`]; a headless
//! installation has no picker, so the person who controls the machine records
//! it here instead and the broker stamps [`ConsentMethod::OperatorConfig`].
//! Both land in the same broker state file, mint the same capability grants,
//! and are revocable from the same surfaces — the provenance is the only thing
//! that differs, and it is deliberately visible everywhere consent is listed.
//!
//! **These commands never run inside a turn.** They are standalone provisioning
//! that an operator invokes; there is no flag that answers a folder request the
//! agent made. A mid-turn `request_folder_access` in a headless run gets the
//! same typed refusal an undecided desktop prompt produces — settled by
//! [`crate::folder_executor`] (and by print mode when it embeds the engine).
//!
//! ## Locks, and why these commands do not take them
//!
//! A running `tidebreak serve` or desktop holds `tidebreak.lock` for the life of
//! the process, and any process that has opened the broker holds
//! `host-broker.lock` until that handle drops. Provisioning used to embed a
//! second server and open the broker under those locks, which meant it refused
//! whenever the profile was already owned — the exact moment an operator needs
//! to connect a folder for a live daemon.
//!
//! The durable authority is not the locks; it is the product store (SQLite WAL)
//! and the broker's own atomic state file. These commands therefore open the
//! product store directly and open the broker for the duration of one control
//! request only, then drop it. They never unlock a lock another process holds,
//! never claim to be the server, and never half-grant: product projection and
//! broker registration still converge or the approval is withdrawn.
//!
//! A second process that has already opened the broker for a long-lived handle
//! (the desktop sidecar, or a headless executor mid-operation) will still make
//! `open_broker` fail closed. The error names that condition; it does not offer
//! to steal the lock. Server-mediated HTTP provisioning is deliberately not the
//! path: the server has no broker, and root-attachment routes only move product
//! state — they cannot mint host grants.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Timelike, Utc};
use tidebreak_core::{
    AgentError, BeginRootAttachmentChange, BeginRootAttachmentChangeOutcome, Chat, ChatId, Config,
    DbStore, FinishRootAttachmentChangeOutcome, HostRootId, Profile, Result, RootAttachmentChange,
    RootAttachmentChangeAction, RootAttachmentChangeId, RootAttachmentChangePhase,
    RootAttachmentChangeTerminal, Store, MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};
use tidebreak_host_broker::{
    Broker, BrokerError, Capability, ConsentMethod, ControlEnvelope, ControlRequest, ControlResult,
    GrantStatementSummary, GrantSubject, OperationId, RegisterRootReceipt, RegisterRootRequest,
    RequestId, Response, RevokeGrantRequest, RevokeRootRequest, RootAttachmentMutationKind,
    RootAttachmentMutationReceipt, RootAttachmentMutationRequest, RootId, RootPolicy, Scope,
    SubjectKind, PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::print::OutputFormat;

/// One parsed `tidebreak folder` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Record standing operator consent for one folder, in one chat.
    Connect {
        chat: ChatId,
        path: PathBuf,
        format: OutputFormat,
    },
    /// Report every capability grant the broker holds, with its provenance.
    List {
        chat: Option<ChatId>,
        format: OutputFormat,
    },
    /// Withdraw one folder from one chat, and the approval it created.
    Disconnect {
        chat: ChatId,
        target: OsString,
        format: OutputFormat,
    },
}

/// Hand-rolled argument parsing, matching the rest of the CLI.
///
/// The chat is mandatory for the mutating commands because broker authority is
/// keyed to a conversation (or its project): there is no installation-wide
/// subject to hang a grant on, and inventing one would create reach no product
/// surface could show or withdraw.
pub fn parse(mut args: impl Iterator<Item = OsString>) -> std::result::Result<Command, String> {
    let Some(subcommand) = args.next() else {
        return Err("folder requires connect, list, or disconnect".to_owned());
    };
    if subcommand == OsStr::new("connect") || subcommand == OsStr::new("disconnect") {
        let connect = subcommand == OsStr::new("connect");
        let noun = if connect { "connect" } else { "disconnect" };
        let mut target = None;
        let mut chat = None;
        let mut format = None;
        while let Some(argument) = args.next() {
            if argument == OsStr::new("--chat") {
                let Some(value) = args.next() else {
                    return Err(format!("folder {noun} --chat requires a chat id"));
                };
                if chat.is_some() {
                    return Err(format!("folder {noun} accepts one --chat"));
                }
                chat = Some(parse_chat(&value)?);
            } else if argument == OsStr::new("--output-format") {
                let Some(value) = args.next() else {
                    return Err(format!(
                        "folder {noun} --output-format requires text or json"
                    ));
                };
                if format.is_some() {
                    return Err(format!("folder {noun} accepts one --output-format"));
                }
                format = Some(parse_format(&value)?);
            } else if target.is_none() && !starts_with_dash(&argument) {
                target = Some(argument);
            } else {
                return Err(format!("unexpected folder {noun} argument {argument:?}"));
            }
        }
        let Some(target) = target else {
            return Err(if connect {
                "folder connect requires a folder path".to_owned()
            } else {
                "folder disconnect requires a folder path or root id".to_owned()
            });
        };
        let Some(chat) = chat else {
            return Err(format!("folder {noun} requires --chat <id>"));
        };
        let format = format.unwrap_or(OutputFormat::Text);
        return Ok(if connect {
            Command::Connect {
                chat,
                path: PathBuf::from(target),
                format,
            }
        } else {
            Command::Disconnect {
                chat,
                target,
                format,
            }
        });
    }
    if subcommand == OsStr::new("list") {
        let mut chat = None;
        let mut format = None;
        while let Some(argument) = args.next() {
            if argument == OsStr::new("--chat") {
                let Some(value) = args.next() else {
                    return Err("folder list --chat requires a chat id".to_owned());
                };
                if chat.is_some() {
                    return Err("folder list accepts one --chat".to_owned());
                }
                chat = Some(parse_chat(&value)?);
            } else if argument == OsStr::new("--output-format") {
                let Some(value) = args.next() else {
                    return Err("folder list --output-format requires text or json".to_owned());
                };
                if format.is_some() {
                    return Err("folder list accepts one --output-format".to_owned());
                }
                format = Some(parse_format(&value)?);
            } else {
                return Err(format!("unexpected folder list argument {argument:?}"));
            }
        }
        return Ok(Command::List {
            chat,
            format: format.unwrap_or(OutputFormat::Text),
        });
    }
    Err(format!("unknown folder subcommand {subcommand:?}"))
}

fn starts_with_dash(argument: &OsStr) -> bool {
    argument.to_string_lossy().starts_with('-')
}

fn parse_chat(value: &OsStr) -> std::result::Result<ChatId, String> {
    ChatId::from_str(&value.to_string_lossy()).map_err(|_| "--chat expects a chat UUID".to_owned())
}

fn parse_format(value: &OsStr) -> std::result::Result<OutputFormat, String> {
    OutputFormat::parse(&value.to_string_lossy())
        .ok_or_else(|| "--output-format expects text or json".to_owned())
}

/// Run one folder command against the profile's data directory.
pub async fn run(command: Command) -> Result<()> {
    let config = crate::profile_config()?;
    // stdout carries the command's report; logs stay in the profile's log file.
    tidebreak_server::logging::init_logging_file_only(&config.data_dir);
    let data_dir = config.data_dir.clone();
    // Prefer a normal embed: that runs the desktop schema epoch path and is
    // the only safe first-boot of an idle profile. When serve/desktop already
    // holds `tidebreak.lock`, fall back to opening the product store beside it
    // — WAL allows the concurrent writer, and a live profile has already
    // passed the epoch gate. Broker handles are opened per control request so
    // `host-broker.lock` is not held across the whole command.
    let (store, _embedded) = open_product_store(config).await?;

    match command {
        Command::Connect { chat, path, format } => {
            connect(&store, &data_dir, chat, path, format).await
        }
        Command::List { chat, format } => list(&store, &data_dir, chat, format).await,
        Command::Disconnect {
            chat,
            target,
            format,
        } => disconnect(&store, &data_dir, chat, &target, format).await,
    }
}

/// Open the profile's product store, embedding a server when the data directory
/// is free and sharing it when it is not.
///
/// The embedded [`tidebreak_server::Server`] is returned so its instance lock and
/// accept loop stay alive for the command; drop order keeps the store usable.
/// Self-host has no local broker and no operator folder path: refuse it here
/// rather than half-open a remote store and fail on the broker later.
async fn open_product_store(
    config: Config,
) -> Result<(Arc<dyn Store>, Option<tidebreak_server::Server>)> {
    match config.profile {
        Profile::Desktop => {}
        // Self-host has no local broker. Any future profile is refused the
        // same way until it grows an explicit operator-folder path.
        _ => {
            return Err(AgentError::config(
                "`tidebreak folder` provisions host consent on this machine's broker; \
                 it is not available on this profile",
            ));
        }
    }
    match tidebreak_server::bind_configured(config.clone()).await {
        Ok(server) => {
            let store = server.store();
            Ok((store, Some(server)))
        }
        Err(error) if data_dir_held_by_another_process(&error) => {
            // The running owner already migrated and marked the schema. Open
            // the same SQLite file under WAL without taking `tidebreak.lock`.
            let store = DbStore::connect(&config.database_url()?).await?;
            Ok((Arc::new(store), None))
        }
        Err(error) => Err(error),
    }
}

/// Whether a bind failure is the data-directory instance lock, not a real
/// configuration problem. Matched on the stable phrase `InstanceLock` writes.
fn data_dir_held_by_another_process(error: &AgentError) -> bool {
    let message = error.to_string();
    message.contains("already running on the data directory")
}

/// Open the same durable broker state the desktop's sidecar opens.
///
/// The policy is assembled exactly as the sidecar assembles it: the host's
/// protected-location rules plus this profile's own data directory, which no
/// folder may ever cover. Local command reach is decided by the same host
/// probe, so a folder connected here carries the grants it would have carried
/// had the desktop's picker connected it on this machine.
///
/// Also the handle [`crate::folder_executor`] opens through the same path for
/// each host operation: one capability store, opened one way, whether an
/// operator is provisioning a folder or a turn is reading one. Callers must
/// drop the returned handle promptly — it holds `host-broker.lock`.
pub(crate) fn open_broker(data_dir: &Path) -> Result<Broker> {
    let home = std::env::home_dir()
        .ok_or_else(|| AgentError::config("could not resolve the current user's home directory"))?
        .canonicalize()
        .map_err(|error| {
            AgentError::config(format!("could not resolve the home directory: {error}"))
        })?;
    let data_dir = data_dir.canonicalize().map_err(|error| {
        AgentError::config(format!("could not resolve the data directory: {error}"))
    })?;
    let policy = RootPolicy::for_host(home)
        .and_then(|policy| policy.with_private_directory(&data_dir))
        .map_err(|error| {
            AgentError::config(format!("could not initialize the folder policy: {error}"))
        })?;
    let execute_commands = tidebreak_code_execution::LocalExecutionProvider::availability().is_ok();
    Broker::open_with_execute_commands(policy, &data_dir, execute_commands)
        .map_err(broker_open_error)
}

/// Map a broker-open failure into an operator-facing configuration error.
///
/// A lock held by another process is the expected contention case when the
/// desktop sidecar (or another long-lived broker handle) is already open. The
/// message names that condition and never suggests unlocking anything.
pub(crate) fn broker_open_error(error: BrokerError) -> AgentError {
    match error {
        BrokerError::Io(ref io_error) if io_error.kind() == io::ErrorKind::WouldBlock => {
            AgentError::config(
                "connected-folder state is held by another Tidebreak process \
                 (desktop sidecar or a live folder executor). Retry in a moment; \
                 do not remove host-broker.lock",
            )
        }
        other => AgentError::config(format!("could not open connected-folder state: {other}")),
    }
}

/// The broker subject a chat's host authority belongs to.
///
/// Identical to the desktop's derivation: a chat inside a project shares that
/// project's standing consent, and a loose chat owns its own.
fn subject_for(chat: &Chat) -> Result<GrantSubject> {
    match chat.project_id {
        Some(project_id) => GrantSubject::project(project_id.0),
        None => GrantSubject::conversation(chat.id.0),
    }
    .map_err(|_| AgentError::msg("chat has an invalid conversation identity"))
}

async fn load_chat(store: &Arc<dyn Store>, chat: ChatId) -> Result<Chat> {
    store
        .get_chat(chat)
        .await?
        .ok_or_else(|| AgentError::msg(format!("chat {chat} not found")))
}

/// Run one control request against a freshly opened broker handle, then drop
/// it so `host-broker.lock` is released before the next product-store step.
///
/// Opening per request is what lets provisioning share a data directory with a
/// running `serve`/desktop: the long-lived owner keeps `tidebreak.lock`, and
/// this process only briefly contends for the broker lock. Holding the handle
/// across an await would pin the lock under a concurrent folder executor.
fn control(data_dir: &Path, request: ControlRequest) -> Result<ControlResult> {
    let broker = open_broker(data_dir)?;
    let response = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(),
        request,
    });
    match response.response {
        Response::Ok(result) => Ok(result),
        // The broker's messages are already safe for display: they never carry
        // an absolute path or raw OS error text.
        Response::Error(error) => Err(AgentError::msg(format!(
            "connected-folder request refused ({:?}): {}",
            error.code, error.message
        ))),
    }
}

/// Resolve an operator-supplied folder argument to an absolute path.
///
/// Canonicalization here is deliberate and matches what the broker does with
/// the path afterwards ([`RootPolicy::open_root`] canonicalizes and pins a
/// descriptor): a symlink argument names the directory it resolves to, and a
/// path that does not exist is rejected before any state is touched. Doing it
/// twice is harmless — resolving an already-canonical path is a fixed point —
/// and it lets the CLI say "no such folder" instead of surfacing a broker
/// error code for a typo.
fn canonical_folder(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .map_err(|error| AgentError::msg(format!("could not open {}: {error}", path.display())))?;
    if !canonical.is_dir() {
        return Err(AgentError::msg(format!(
            "{} is not a folder",
            path.display()
        )));
    }
    Ok(canonical)
}

async fn connect(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    chat_id: ChatId,
    path: PathBuf,
    format: OutputFormat,
) -> Result<()> {
    let path = canonical_folder(&path)?;
    let chat = load_chat(store, chat_id).await?;
    let subject = subject_for(&chat)?;
    let executor_id = executor_identity(data_dir)?;
    // A change left awaiting the broker by an interrupted run blocks the chat.
    // Drive it to a terminal before starting new work rather than reporting a
    // conflict the operator cannot act on.
    settle_pending_changes(store, data_dir, &chat, subject, executor_id).await?;
    // Settling advances the projection's revision, so the CAS fence the attach
    // below carries has to come from a fresh read.
    let chat = load_chat(store, chat_id).await?;
    if subject_for(&chat)? != subject {
        return Err(AgentError::msg(
            "the chat's authority changed while the folder was being connected",
        ));
    }

    // Registration mints the standing grants and the broker-side attachment in
    // one durable operation; `OperatorConfig` is the provenance stamped on all
    // of them. The broker refuses any consent method it did not expect here,
    // so this is the only place the CLI can create host reach.
    let operation_id = OperationId::new();
    let root = match control(
        data_dir,
        ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id,
            subject,
            conversation_id: chat.id.0,
            path,
            consent_method: ConsentMethod::OperatorConfig,
        }),
    )? {
        ControlResult::RegisterRoot(result) => result.root,
        _ => return Err(unexpected_broker_response()),
    };
    // Read the durable receipt back rather than trusting the response: it is
    // the same authority the desktop's reconciler consults, and it reports a
    // registration that was revoked between commit and reply.
    match lookup_registration(data_dir, operation_id, subject, chat.id.0)? {
        RegisterRootReceipt::Completed { root: recorded } if recorded == root => {}
        _ => {
            return Err(AgentError::msg(
                "the folder was not durably connected; nothing was granted",
            ))
        }
    }

    // The product projection is what makes the folder reachable in a turn and
    // visible in the desktop's connected-folders panel. If it cannot be
    // committed, withdraw the approval instead of leaving standing host reach
    // no surface can show.
    let attached = attach_to_chat(store, data_dir, &chat, subject, root.root_id, executor_id).await;
    if let Err(error) = attached {
        let revoked = control(
            data_dir,
            ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id: OperationId::new(),
                subject,
                root_id: root.root_id,
            }),
        );
        return Err(match revoked {
            Ok(_) => AgentError::msg(format!("{error}; the folder approval was withdrawn")),
            Err(cleanup) => AgentError::msg(format!(
                "{error}; the folder approval could not be withdrawn either ({cleanup}) — review \
                 `tidebreak folder list`"
            )),
        });
    }

    if format == OutputFormat::Json {
        return emit_json(&serde_json::json!({
            "chat": chat_id,
            "root_id": root.root_id,
            "display_name": root.display_name,
            "consent_method": "operator_config",
        }));
    }
    println!(
        "tidebreak: connected {} to chat {} (root {}, operator configuration)",
        safe_label(&root.display_name),
        chat_id,
        root.root_id
    );
    Ok(())
}

fn lookup_registration(
    data_dir: &Path,
    operation_id: OperationId,
    subject: GrantSubject,
    conversation_id: Uuid,
) -> Result<RegisterRootReceipt> {
    match control(
        data_dir,
        ControlRequest::LookupRegisterRootReceipt(
            tidebreak_host_broker::LookupRegisterRootReceiptRequest {
                operation_id,
                subject,
                conversation_id,
            },
        ),
    )? {
        ControlResult::LookupRegisterRootReceipt(result) if result.operation_id == operation_id => {
            Ok(result.receipt)
        }
        _ => Err(unexpected_broker_response()),
    }
}

async fn list(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    chat: Option<ChatId>,
    format: OutputFormat,
) -> Result<()> {
    // `ListGrantStatements` is the same query the desktop's Permissions surface
    // reads (`list_capability_consents`), so the two shells cannot disagree
    // about what is granted or how it was granted.
    let ControlResult::ListGrantStatements { grants } =
        control(data_dir, ControlRequest::ListGrantStatements)?
    else {
        return Err(unexpected_broker_response());
    };
    let filter = match chat {
        Some(chat) => Some(subject_for(&load_chat(store, chat).await?)?),
        None => None,
    };
    let grants: Vec<_> = grants
        .into_iter()
        .filter(|grant| filter.is_none_or(|subject| subject == grant.subject))
        .collect();

    if format == OutputFormat::Json {
        return emit_json(&serde_json::json!({
            "grants": grants.iter().map(grant_json).collect::<Vec<_>>(),
        }));
    }

    let mut shown = 0usize;
    for grant in grants {
        let subject = match grant.subject.kind() {
            SubjectKind::Conversation => format!("chat {}", grant.subject.id()),
            SubjectKind::Project => format!("project {}", grant.subject.id()),
        };
        let scope = match &grant.scope {
            Scope::Subject => "all connected folders".to_owned(),
            Scope::Root { root_id } => folder_label(grant.root_display_name.as_deref(), *root_id),
            Scope::PathSubtree { root_id, relative } => format!(
                "{}/{}",
                folder_label(grant.root_display_name.as_deref(), *root_id),
                relative.as_str()
            ),
            _ => "unrecognized scope".to_owned(),
        };
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            grant.grant_id,
            subject,
            capability_label(grant.capability),
            scope,
            consent_label(grant.consent_method),
            grant.granted_at.to_rfc3339(),
        );
        shown += 1;
    }
    if shown == 0 {
        eprintln!("tidebreak: no connected-folder grants");
    }
    Ok(())
}

/// One grant as a stable driver-facing object: ids, capability, scope, and
/// consent provenance are structured fields rather than the text table's
/// columns.
fn grant_json(grant: &GrantStatementSummary) -> serde_json::Value {
    serde_json::json!({
        "grant_id": grant.grant_id,
        "subject": {
            "kind": match grant.subject.kind() {
                SubjectKind::Conversation => "conversation",
                SubjectKind::Project => "project",
            },
            "id": grant.subject.id(),
        },
        "capability": capability_json(grant.capability),
        "scope": scope_json(&grant.scope),
        "root_display_name": grant.root_display_name,
        "consent_method": consent_json(grant.consent_method),
        "granted_at": grant.granted_at,
    })
}

fn capability_json(capability: Capability) -> &'static str {
    match capability {
        Capability::ListRoots => "list_roots",
        Capability::ReadFiles => "read_files",
        Capability::WriteFiles => "write_files",
        Capability::ExecuteCommands => "execute_commands",
        _ => "unrecognized",
    }
}

fn consent_json(method: ConsentMethod) -> &'static str {
    match method {
        ConsentMethod::FolderPicker => "folder_picker",
        ConsentMethod::PermissionDialog => "permission_dialog",
        ConsentMethod::OperatorConfig => "operator_config",
        ConsentMethod::CarriedForward => "carried_forward",
        _ => "unrecognized",
    }
}

fn scope_json(scope: &Scope) -> serde_json::Value {
    match scope {
        Scope::Subject => serde_json::json!({ "kind": "subject" }),
        Scope::Root { root_id } => serde_json::json!({
            "kind": "root",
            "root_id": root_id,
        }),
        Scope::PathSubtree { root_id, relative } => serde_json::json!({
            "kind": "path_subtree",
            "root_id": root_id,
            "relative": relative.as_str(),
        }),
        _ => serde_json::json!({ "kind": "unrecognized" }),
    }
}

/// Write one JSON object on stdout, matching setup's one-object shape so a
/// driver can read either family the same way.
fn emit_json(value: &serde_json::Value) -> Result<()> {
    println!("{value}");
    Ok(())
}

fn folder_label(display_name: Option<&str>, root_id: RootId) -> String {
    display_name.map_or_else(|| format!("folder {root_id}"), safe_label)
}

/// A folder name is a host filename: it can contain tabs, newlines, and
/// bidirectional overrides. This listing is tab-separated and read by people
/// and scripts alike, so a name never gets to invent a column or a row.
fn safe_label(value: &str) -> String {
    value
        .chars()
        .take(80)
        .map(|character| {
            if character.is_control()
                || character == '\t'
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn capability_label(capability: Capability) -> &'static str {
    match capability {
        Capability::ListRoots => "list-folders",
        Capability::ReadFiles => "read",
        Capability::WriteFiles => "write",
        Capability::ExecuteCommands => "exec",
        _ => "unrecognized",
    }
}

fn consent_label(method: ConsentMethod) -> &'static str {
    match method {
        ConsentMethod::FolderPicker => "folder-picker",
        ConsentMethod::PermissionDialog => "permission-dialog",
        ConsentMethod::OperatorConfig => "operator-config",
        ConsentMethod::CarriedForward => "carried-forward",
        _ => "unrecognized",
    }
}

async fn disconnect(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    chat_id: ChatId,
    target: &OsStr,
    format: OutputFormat,
) -> Result<()> {
    let chat = load_chat(store, chat_id).await?;
    let subject = subject_for(&chat)?;
    let executor_id = executor_identity(data_dir)?;
    settle_pending_changes(store, data_dir, &chat, subject, executor_id).await?;
    let chat = load_chat(store, chat_id).await?;
    let root_id = resolve_target(data_dir, &chat, target)?;

    detach_from_chat(store, data_dir, &chat, subject, root_id, executor_id).await?;

    // Detaching leaves the host approval standing, which is right when another
    // conversation still holds it and wrong when nothing does — an operator
    // would have no way to withdraw it. Withdraw it exactly when this chat's
    // subject is the only one with root-scoped grants on the folder, and say
    // so plainly when it is not.
    let mut approval_revoked = false;
    let mut approval_shared = false;
    if sole_grant_holder(data_dir, root_id, subject)? {
        match control(
            data_dir,
            ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id: OperationId::new(),
                subject,
                root_id,
            }),
        )? {
            ControlResult::RevokeRoot(result) if result.revoked => {
                approval_revoked = true;
            }
            ControlResult::RevokeRoot(_) => {}
            _ => return Err(unexpected_broker_response()),
        }
    } else {
        approval_shared = true;
    }

    // `RevokeRoot` drops only root-scoped grants. `ListRoots` is subject-scoped
    // and would otherwise survive as a dangling list-folders row; the next
    // connect then mints a second one. Drop it once this subject holds no
    // root-scoped reach left to list. Must run before any format-specific
    // return — JSON used to early-return here and leave the orphan standing.
    revoke_orphan_list_roots(data_dir, subject)?;

    if format == OutputFormat::Json {
        return emit_json(&serde_json::json!({
            "chat": chat_id,
            "root_id": root_id,
            "approval_revoked": approval_revoked,
            "approval_shared": approval_shared,
        }));
    }

    println!("tidebreak: disconnected {root_id} from chat {chat_id}");
    if approval_revoked {
        println!("tidebreak: withdrew the folder approval");
    } else if approval_shared {
        eprintln!(
            "tidebreak: the folder approval still reaches other subjects and was left in place; \
             see `tidebreak folder list`"
        );
    }
    Ok(())
}

/// Withdraw subject-scoped `ListRoots` when the subject has no root reach left.
///
/// Registration always mints a fresh list-folders grant alongside the folder's
/// read/write/exec. `RevokeRoot` deliberately leaves subject-scoped grants
/// alone (they are not properties of one folder), so a sole-holder disconnect
/// used to leave list-folders standing and the next connect stacked another.
/// When nothing root-scoped remains for the subject, list-folders has nothing
/// to name and must go with the rest of the withdrawal.
fn revoke_orphan_list_roots(data_dir: &Path, subject: GrantSubject) -> Result<()> {
    let ControlResult::ListGrantStatements { grants } =
        control(data_dir, ControlRequest::ListGrantStatements)?
    else {
        return Err(unexpected_broker_response());
    };
    let still_holds_root = grants.iter().any(|grant| {
        grant.subject == subject
            && matches!(grant.scope, Scope::Root { .. } | Scope::PathSubtree { .. })
    });
    if still_holds_root {
        return Ok(());
    }
    for grant in grants {
        if grant.subject != subject
            || grant.capability != Capability::ListRoots
            || !matches!(grant.scope, Scope::Subject)
        {
            continue;
        }
        match control(
            data_dir,
            ControlRequest::RevokeGrant(RevokeGrantRequest {
                subject,
                grant_id: grant.grant_id,
            }),
        )? {
            ControlResult::RevokeGrant(_) => {}
            _ => return Err(unexpected_broker_response()),
        }
    }
    Ok(())
}

/// Whether `subject` is the only holder of root-scoped grants on `root_id`.
fn sole_grant_holder(data_dir: &Path, root_id: RootId, subject: GrantSubject) -> Result<bool> {
    let ControlResult::ListGrantStatements { grants } =
        control(data_dir, ControlRequest::ListGrantStatements)?
    else {
        return Err(unexpected_broker_response());
    };
    let mut holders = grants.iter().filter(|grant| match &grant.scope {
        Scope::Root { root_id: scoped } => *scoped == root_id,
        Scope::PathSubtree {
            root_id: scoped, ..
        } => *scoped == root_id,
        _ => false,
    });
    Ok(holders.all(|grant| grant.subject == subject))
}

/// Resolve a folder argument to one root attached to this chat.
///
/// A root id is exact. A path is matched by the leaf name the broker reports,
/// because the broker deliberately never exposes a root's absolute path — so an
/// ambiguous name is refused rather than guessed at.
fn resolve_target(data_dir: &Path, chat: &Chat, target: &OsStr) -> Result<RootId> {
    let attached = chat
        .root_attachments
        .iter()
        .map(|attachment| *attachment.root_id.as_uuid())
        .collect::<std::collections::HashSet<_>>();
    let ControlResult::ListApprovedRoots { roots } =
        control(data_dir, ControlRequest::ListApprovedRoots)?
    else {
        return Err(unexpected_broker_response());
    };
    let roots = roots
        .into_iter()
        .filter(|root| attached.contains(&root.root_id.as_uuid()))
        .collect::<Vec<_>>();

    let text = target.to_string_lossy();
    if let Ok(root_id) = RootId::from_str(&text) {
        return if roots.iter().any(|root| root.root_id == root_id) {
            Ok(root_id)
        } else {
            Err(AgentError::msg(format!(
                "folder {root_id} is not connected to this chat"
            )))
        };
    }

    let path = canonical_folder(Path::new(target))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| AgentError::msg("the folder path has no name to match"))?;
    let matches = roots
        .iter()
        .filter(|root| root.display_name == name)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [root] => Ok(root.root_id),
        [] => Err(AgentError::msg(format!(
            "no folder named {name} is connected to this chat"
        ))),
        many => Err(AgentError::msg(format!(
            "{} connected folders are named {name}; disconnect one by root id ({})",
            many.len(),
            many.iter()
                .map(|root| root.root_id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

async fn attach_to_chat(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    chat: &Chat,
    subject: GrantSubject,
    root_id: RootId,
    executor_id: Uuid,
) -> Result<()> {
    drive_change(
        store,
        data_dir,
        chat,
        subject,
        root_id,
        executor_id,
        RootAttachmentChangeAction::Attach,
    )
    .await
}

async fn detach_from_chat(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    chat: &Chat,
    subject: GrantSubject,
    root_id: RootId,
    executor_id: Uuid,
) -> Result<()> {
    let product_root = HostRootId::from_uuid(root_id.as_uuid())
        .map_err(|_| AgentError::msg("invalid connected-folder identity"))?;
    if !chat
        .root_attachments
        .iter()
        .any(|attachment| attachment.root_id == product_root)
    {
        return Err(AgentError::msg("that folder is not connected to this chat"));
    }
    drive_change(
        store,
        data_dir,
        chat,
        subject,
        root_id,
        executor_id,
        RootAttachmentChangeAction::Detach,
    )
    .await
}

/// Commit one product-side attachment intent, then converge the broker to it.
///
/// This is the desktop reconciler's sequence, minus its crash-recovery receipt
/// store: product intent is durable first, the broker mutation carries the
/// change id as its idempotency identity, and the terminal receipt is read back
/// before the change is finished. An interrupted run leaves a change awaiting
/// the broker, which the next invocation settles.
async fn drive_change(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    chat: &Chat,
    subject: GrantSubject,
    root_id: RootId,
    executor_id: Uuid,
    action: RootAttachmentChangeAction,
) -> Result<()> {
    let product_root = HostRootId::from_uuid(root_id.as_uuid())
        .map_err(|_| AgentError::msg("invalid connected-folder identity"))?;
    let request = BeginRootAttachmentChange {
        id: RootAttachmentChangeId::new(),
        chat_id: chat.id,
        executor_id,
        root_id: product_root,
        action,
        expected_attachment_revision: chat.attachment_revision,
        created_at: canonical_now(),
    };
    let change = match store.begin_root_attachment_change(&request).await? {
        BeginRootAttachmentChangeOutcome::Begun(change)
        | BeginRootAttachmentChangeOutcome::Existing(change) => change,
        BeginRootAttachmentChangeOutcome::ChatNotFound => {
            return Err(AgentError::msg("chat not found"))
        }
        BeginRootAttachmentChangeOutcome::ChatBusy => {
            return Err(AgentError::msg(
                "another connected-folder change is in progress for this chat",
            ))
        }
        outcome => {
            return Err(AgentError::msg(format!(
                "the connected-folder projection refused this change ({outcome:?})"
            )))
        }
    };
    settle_change(store, data_dir, subject, chat.id.0, executor_id, change).await
}

/// Bring one durable change to a terminal phase and report what it means.
async fn settle_change(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    subject: GrantSubject,
    conversation_id: Uuid,
    executor_id: Uuid,
    change: RootAttachmentChange,
) -> Result<()> {
    let change = if change.phase == RootAttachmentChangePhase::AwaitingBroker {
        let terminal = converge_broker(data_dir, subject, conversation_id, &change)?;
        match store
            .finish_root_attachment_change(change.id, executor_id, &terminal, canonical_now())
            .await?
        {
            FinishRootAttachmentChangeOutcome::Finished(change)
            | FinishRootAttachmentChangeOutcome::Existing(change) => change,
            outcome => {
                return Err(AgentError::msg(format!(
                    "the connected-folder projection could not be finished ({outcome:?})"
                )))
            }
        }
    } else {
        change
    };
    match change.phase {
        RootAttachmentChangePhase::Completed => Ok(()),
        RootAttachmentChangePhase::Failed => Err(AgentError::msg(change.failure.map_or_else(
            || "the connected-folder change failed".to_owned(),
            |failure| failure.message,
        ))),
        RootAttachmentChangePhase::AwaitingBroker => Err(AgentError::msg(
            "the connected-folder change has no durable host outcome yet",
        )),
    }
}

/// Dispatch (or recover) the broker half of one product change.
fn converge_broker(
    data_dir: &Path,
    subject: GrantSubject,
    conversation_id: Uuid,
    change: &RootAttachmentChange,
) -> Result<RootAttachmentChangeTerminal> {
    let root_id = RootId::from_uuid(*change.root_id.as_uuid())
        .map_err(|_| AgentError::msg("invalid connected-folder identity"))?;
    let operation_id = OperationId::from_uuid(*change.id.as_uuid())
        .map_err(|_| AgentError::msg("invalid connected-folder change identity"))?;
    let mutation = match change.action {
        RootAttachmentChangeAction::Attach => RootAttachmentMutationKind::Attach,
        RootAttachmentChangeAction::Detach => RootAttachmentMutationKind::Detach,
    };
    let mut receipt = lookup_mutation(
        data_dir,
        subject,
        conversation_id,
        root_id,
        operation_id,
        mutation,
    )?;
    if matches!(receipt, RootAttachmentMutationReceipt::Unknown) {
        let request = RootAttachmentMutationRequest {
            operation_id,
            subject,
            conversation_id,
            root_id,
            // Attach carries this command's own provenance; detach carries
            // none, which the broker requires.
            consent_method: match change.action {
                RootAttachmentChangeAction::Attach => Some(ConsentMethod::OperatorConfig),
                RootAttachmentChangeAction::Detach => None,
            },
        };
        let dispatched = control(
            data_dir,
            match change.action {
                RootAttachmentChangeAction::Attach => ControlRequest::AttachRoot(request),
                RootAttachmentChangeAction::Detach => ControlRequest::DetachRoot(request),
            },
        );
        // The receipt below is the authoritative post-effect check, exactly as
        // in the desktop reconciler: a failed dispatch may still have landed.
        match dispatched {
            Ok(ControlResult::AttachRoot(_) | ControlResult::DetachRoot(_)) | Err(_) => {}
            Ok(_) => return Err(unexpected_broker_response()),
        }
        receipt = lookup_mutation(
            data_dir,
            subject,
            conversation_id,
            root_id,
            operation_id,
            mutation,
        )?;
    }
    mutation_terminal(receipt, root_id, change.action)?.ok_or_else(|| {
        AgentError::msg("the connected-folder change has no durable host outcome yet")
    })
}

fn lookup_mutation(
    data_dir: &Path,
    subject: GrantSubject,
    conversation_id: Uuid,
    root_id: RootId,
    operation_id: OperationId,
    mutation: RootAttachmentMutationKind,
) -> Result<RootAttachmentMutationReceipt> {
    match control(
        data_dir,
        ControlRequest::LookupRootAttachmentReceipt(
            tidebreak_host_broker::LookupRootAttachmentReceiptRequest {
                operation_id,
                subject,
                conversation_id,
                root_id,
                mutation,
            },
        ),
    )? {
        ControlResult::LookupRootAttachmentReceipt(result)
            if result.operation_id == operation_id =>
        {
            Ok(result.receipt)
        }
        _ => Err(unexpected_broker_response()),
    }
}

/// Map a durable broker receipt onto the product's terminal vocabulary.
///
/// A rejected mutation still settles on what the broker holds now rather than
/// reporting "unknown": nothing re-drives a terminal change, and an unknown
/// observation would leave the conversation permanently unresolvable.
fn mutation_terminal(
    receipt: RootAttachmentMutationReceipt,
    root_id: RootId,
    action: RootAttachmentChangeAction,
) -> Result<Option<RootAttachmentChangeTerminal>> {
    let desired = action == RootAttachmentChangeAction::Attach;
    let expected = match action {
        RootAttachmentChangeAction::Attach => RootAttachmentMutationKind::Attach,
        RootAttachmentChangeAction::Detach => RootAttachmentMutationKind::Detach,
    };
    match receipt {
        RootAttachmentMutationReceipt::Unknown => Ok(None),
        RootAttachmentMutationReceipt::Completed {
            result,
            currently_attached,
        } => {
            if result.root_id != root_id || result.mutation != expected {
                return Err(AgentError::msg(
                    "the host receipt contradicts the requested connected-folder change",
                ));
            }
            Ok(Some(if currently_attached == desired {
                RootAttachmentChangeTerminal::Completed {
                    broker_changed: result.changed,
                    broker_currently_attached: currently_attached,
                }
            } else {
                RootAttachmentChangeTerminal::Failed {
                    broker_changed: Some(result.changed),
                    broker_currently_attached: Some(currently_attached),
                    failure: safe_failure(
                        "broker_attachment_superseded",
                        "The folder attachment changed before synchronization completed.",
                    ),
                }
            }))
        }
        RootAttachmentMutationReceipt::Failed {
            currently_attached, ..
        } if currently_attached == desired => Ok(Some(RootAttachmentChangeTerminal::Completed {
            broker_changed: false,
            broker_currently_attached: currently_attached,
        })),
        RootAttachmentMutationReceipt::Failed {
            currently_attached, ..
        } => Ok(Some(RootAttachmentChangeTerminal::Failed {
            broker_changed: Some(false),
            broker_currently_attached: Some(currently_attached),
            failure: safe_failure(
                "broker_attachment_failed",
                "The host could not complete this connected-folder change.",
            ),
        })),
        _ => Err(AgentError::msg("unsupported host attachment receipt")),
    }
}

fn safe_failure(code: &str, message: &str) -> tidebreak_core::RootAttachmentChangeFailure {
    tidebreak_core::RootAttachmentChangeFailure {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable: false,
    }
}

/// Settle changes this CLI executor left awaiting the broker for one chat.
///
/// The desktop's recovery loop only sees changes owned by its own executor, so
/// a run interrupted between product intent and its host mutation is this
/// command's to finish — and until it is finished, the chat refuses new ones.
async fn settle_pending_changes(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    chat: &Chat,
    subject: GrantSubject,
    executor_id: Uuid,
) -> Result<()> {
    let pending = store
        .list_pending_root_attachment_changes(executor_id, MAX_PENDING_ROOT_ATTACHMENT_CHANGES)
        .await?;
    for change in pending {
        if change.chat_id != chat.id {
            continue;
        }
        settle_change(store, data_dir, subject, chat.id.0, executor_id, change).await?;
    }
    Ok(())
}

/// This CLI's stable native-executor identity for this profile.
///
/// It must survive across invocations: only the executor that began a change
/// may finish it, and only the executor that claimed a client tool call may
/// recover it, so a fresh identity per run would strand interrupted work in
/// either case. Both uses are the same claim — "this profile's CLI owns that
/// pending work" — so they share one identity rather than inventing a second.
/// It is not a credential, but it lives with the rest of the private profile
/// state and inherits its permissions.
pub(crate) fn executor_identity(data_dir: &Path) -> Result<Uuid> {
    let path = data_dir.join("cli-folder-executor");
    let read = |path: &Path| -> Result<Uuid> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            AgentError::config(format!(
                "could not read the folder-executor identity: {error}"
            ))
        })?;
        Uuid::from_str(text.trim())
            .map_err(|_| AgentError::config("the stored folder-executor identity is unreadable"))
    };
    if path.exists() {
        return read(&path);
    }
    write_private(&path, &Uuid::new_v4().to_string()).map_err(|error| {
        AgentError::config(format!(
            "could not record the folder-executor identity: {error}"
        ))
    })?;
    // Always read the identity back: a concurrent invocation may have written
    // first, and the one on disk is the one that owns pending work.
    read(&path)
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        }
        // Another invocation won the race; its identity is the one to use.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Microsecond-truncated, matching the timestamp grain the store persists.
fn canonical_now() -> DateTime<Utc> {
    let now = Utc::now();
    now.with_nanosecond((now.nanosecond() / 1_000) * 1_000)
        .unwrap_or(now)
}

fn unexpected_broker_response() -> AgentError {
    AgentError::msg("the connected-folder store returned an unexpected response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> impl Iterator<Item = OsString> {
        values
            .iter()
            .map(|value| OsString::from(*value))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The mutating commands must name a conversation: broker authority has no
    /// installation-wide subject, and a grant nothing can show is a one-way
    /// door. A path that looks like a flag must not be swallowed as one either.
    #[test]
    fn mutating_commands_require_an_explicit_chat() {
        let chat = ChatId::new();
        assert_eq!(
            parse(args(&["connect", "/srv/data", "--chat", &chat.to_string()])).unwrap(),
            Command::Connect {
                chat,
                path: PathBuf::from("/srv/data"),
                format: OutputFormat::Text,
            }
        );
        assert!(parse(args(&["connect", "/srv/data"])).is_err());
        assert!(parse(args(&["disconnect", "/srv/data"])).is_err());
        assert!(parse(args(&["connect", "--chat", &chat.to_string()])).is_err());
        assert!(parse(args(&["connect", "--yes", "--chat", &chat.to_string()])).is_err());
        assert_eq!(
            parse(args(&["list"])).unwrap(),
            Command::List {
                chat: None,
                format: OutputFormat::Text,
            }
        );
        assert!(parse(args(&["approve"])).is_err());
    }

    /// Drivers need a stable opt-in on every folder verb. The default stays
    /// text so interactive use is unchanged; json is accepted anywhere among
    /// the other flags, once.
    #[test]
    fn folder_commands_accept_output_format() {
        let chat = ChatId::new();
        let chat_s = chat.to_string();
        assert_eq!(
            parse(args(&[
                "list",
                "--chat",
                &chat_s,
                "--output-format",
                "json"
            ]))
            .unwrap(),
            Command::List {
                chat: Some(chat),
                format: OutputFormat::Json,
            }
        );
        assert_eq!(
            parse(args(&[
                "connect",
                "/srv/data",
                "--output-format",
                "json",
                "--chat",
                &chat_s,
            ]))
            .unwrap(),
            Command::Connect {
                chat,
                path: PathBuf::from("/srv/data"),
                format: OutputFormat::Json,
            }
        );
        assert_eq!(
            parse(args(&[
                "disconnect",
                "/srv/data",
                "--chat",
                &chat_s,
                "--output-format",
                "text",
            ]))
            .unwrap(),
            Command::Disconnect {
                chat,
                target: OsString::from("/srv/data"),
                format: OutputFormat::Text,
            }
        );
        assert!(parse(args(&["list", "--output-format"])).is_err());
        assert!(parse(args(&["list", "--output-format", "yaml"])).is_err());
        assert!(parse(args(&[
            "list",
            "--output-format",
            "json",
            "--output-format",
            "text"
        ]))
        .is_err());
    }
}
