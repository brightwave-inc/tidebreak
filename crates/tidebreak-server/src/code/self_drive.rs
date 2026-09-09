//! Native tools let a conversation choose repositories without moving its own workspace.
use super::runtime::{CodeRuntime, ExternalMessage, NewSessionSettings};
use crate::error::ServerError;
use crate::obo_gateway::{GitCredentialLender, GitForgeAttributionRequest};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tidebreak_core::{
    ApprovalClass, CodeExternalGrant, ExternalSessionResolution, HarnessKind, OwnerId, Session,
    SessionId, SessionLifecycle, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolRegistry,
    ToolSpec,
};

#[derive(Default)]
pub struct SessionTools {
    runtime: OnceLock<Weak<CodeRuntime>>,
    starts: Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionTools {
    pub fn register(self: &Arc<Self>, tools: &mut ToolRegistry) {
        for name in [
            "code_repos",
            "code_session_create",
            "code_run_turn",
            "code_wait",
            "code_sessions",
        ] {
            tools.register(Box::new(SessionTool {
                host: self.clone(),
                name,
            }));
        }
    }
    pub fn attach(&self, runtime: &Arc<CodeRuntime>) {
        let _ = self.runtime.set(Arc::downgrade(runtime));
    }
}

struct SessionTool {
    host: Arc<SessionTools>,
    name: &'static str,
}
struct Authority {
    parent: Session,
    grant: Option<CodeExternalGrant>,
    channel: Option<String>,
    lender: Option<Arc<dyn GitCredentialLender>>,
}

async fn authority(runtime: &CodeRuntime, parent: SessionId) -> Result<Authority, ServerError> {
    let parent = tidebreak_core::db::code::get_session_all_owners(&runtime.db, parent)
        .await?
        .ok_or_else(|| ServerError::not_found("parent session not found"))?;
    if matches!(
        parent.lifecycle,
        SessionLifecycle::Ended | SessionLifecycle::Fenced
    ) {
        return Err(ServerError::conflict_kind(
            "parent_unavailable",
            "This conversation is ended or fenced.",
        ));
    }
    let bindings =
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &parent.owner, parent.id)
            .await?;
    let grant = if let Some(binding) = bindings.first() {
        if bindings
            .iter()
            .any(|other| other.grant_id != binding.grant_id)
        {
            return Err(ServerError::conflict_kind(
                "grant_scope",
                "This conversation has conflicting connection authorities.",
            ));
        }
        Some(
            tidebreak_core::db::code::get_external_grant(
                &runtime.db,
                &parent.owner,
                binding.grant_id,
            )
            .await?
            .filter(|grant| grant.revoked_at.is_none())
            .ok_or_else(|| ServerError::unauthorized("The external connection was revoked."))?,
        )
    } else {
        None
    };
    let context =
        tidebreak_core::db::code::session_context(&runtime.db, &parent.owner, parent.id).await?;
    let lender: Option<Arc<dyn GitCredentialLender>> = if let (Some(grant), Some(external)) = (
        &grant,
        runtime
            .harness_llm()
            .and_then(|relay| relay.external_delegations().cloned()),
    ) {
        Some(external.for_grant(&parent.owner, grant.id).await?)
    } else {
        runtime.git_credentials().cloned()
    };
    Ok(Authority {
        parent,
        grant,
        channel: context.and_then(|c| c.channel_id),
        lender,
    })
}

fn attribution(session: &Session) -> GitForgeAttributionRequest {
    match session.acts_as() {
        tidebreak_core::ActsAs::Person => GitForgeAttributionRequest::Person,
        tidebreak_core::ActsAs::Bot => GitForgeAttributionRequest::Installation,
    }
}

fn text<'a>(args: &'a Value, key: &str, max: usize) -> Result<&'a str, ServerError> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.contains('\0'))
        .ok_or_else(|| {
            ServerError::bad_request(format!(
                "{key} must be nonempty text of at most {max} bytes"
            ))
        })
}

#[async_trait]
impl Tool for SessionTool {
    fn spec(&self) -> ToolSpec {
        let (description, properties, required) = match self.name {
            "code_repos" => ("List repositories available to this conversation's personal or bot identity. Choose repositories from the task; the conversation does not need a default repository.", json!({}), json!([])),
            "code_session_create" => ("Start independent work in a repository and return its child session. Use a different request_key for each task and reuse it on retries. You may start children in different repositories. Read their results with code_wait before answering. Repository access and channel confirmation apply.", json!({
                "repository":{"type":"string","description":"GitHub owner/name"},
                "task":{"type":"string","maxLength":16000},
                "request_key":{"type":"string","maxLength":128,"description":"Stable key for this task, reused on retries."},
                "harness":{"type":"string","description":"Optional installed harness; defaults to claude_code."}
            }), json!(["repository","task","request_key"])),
            "code_run_turn" => ("Send a follow-up to one of this conversation's child sessions. Use a stable request_key to prevent duplicate turns on retry.", json!({"session_id":{"type":"string"},"text":{"type":"string","maxLength":16000},"request_key":{"type":"string","maxLength":128}}), json!(["session_id","text","request_key"])),
            "code_wait" => ("Read child results, waiting up to 20 seconds while they run. Results remain in requested order. If waiting is true, call again after other useful work. A child awaiting approval includes its pending cards.", json!({"session_ids":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string"}}}), json!(["session_ids"])),
            _ => ("List this conversation's child sessions, including their repositories and status.", json!({}), json!([])),
        };
        ToolSpec {
            name: self.name.into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        if matches!(self.name, "code_session_create" | "code_run_turn") {
            ApprovalClass::Sensitive
        } else {
            ApprovalClass::ReadOnly
        }
    }
    async fn execute(&self, ctx: &ToolCtx, args: Value) -> tidebreak_core::Result<ToolOutput> {
        let Some(runtime) = self.host.runtime.get().and_then(Weak::upgrade) else {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::ConfigurationRequired,
                "The session runtime is unavailable.",
            ));
        };
        match self.run(&runtime, ctx, args).await {
            Ok(value) => Ok(ToolOutput::text(value.to_string())),
            Err(error) => Ok(ToolOutput::failed(
                ToolErrorCategory::ToolFailed,
                format!("{}: {}", error.kind(), error.message()),
            )),
        }
    }
}

impl SessionTool {
    async fn run(
        &self,
        runtime: &Arc<CodeRuntime>,
        ctx: &ToolCtx,
        args: Value,
    ) -> Result<Value, ServerError> {
        let auth = authority(runtime, ctx.chat_id).await?;
        match self.name {
            "code_repos" => {
                let registered = runtime.list_repos(&auth.parent.owner).await?;
                let accessible = if let Some(lender) = auth.lender.as_ref() {
                    lender.list_repositories(&auth.parent.owner, attribution(&auth.parent)).await
                        .map_err(|e| ServerError::conflict_kind("git_forge_refused", super::clone::git_forge_refusal_message(&e)))?
                        .into_iter().map(|r| json!({"repository":r.full_name,"description":r.description,"private":r.private})).collect::<Vec<_>>()
                } else {
                    registered.iter().filter_map(|r| Some(json!({"repository":format!("{}/{}", r.origin_owner.as_ref()?, r.origin_name.as_ref()?)}))).collect()
                };
                Ok(
                    json!({"repositories": accessible.into_iter().take(250).collect::<Vec<_>>(), "acts_as":auth.parent.acts_as()}),
                )
            }
            "code_session_create" => self.start(runtime, &auth, &args).await,
            "code_run_turn" => {
                let id: SessionId = serde_json::from_value(args["session_id"].clone())
                    .map_err(|_| ServerError::bad_request("session_id is invalid"))?;
                let child = require_child(runtime, &auth, id).await?;
                send(
                    runtime,
                    &auth,
                    &child,
                    text(&args, "text", 16000)?,
                    text(&args, "request_key", 128)?,
                )
                .await?;
                snapshot(runtime, &auth.parent.owner, child).await
            }
            "code_wait" => {
                let ids: Vec<SessionId> = serde_json::from_value(args["session_ids"].clone())
                    .map_err(|_| {
                        ServerError::bad_request("session_ids must be an array of session IDs")
                    })?;
                if ids.is_empty() || ids.len() > 8 {
                    return Err(ServerError::bad_request(
                        "Name between one and eight child sessions.",
                    ));
                }
                let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let live = authority(runtime, ctx.chat_id).await?;
                    let mut children = Vec::new();
                    let mut waiting = false;
                    for id in &ids {
                        let child = require_child(runtime, &live, *id).await?;
                        let view = snapshot(runtime, &live.parent.owner, child).await?;
                        waiting |= view["running"].as_bool().unwrap_or(false);
                        children.push(view);
                    }
                    if !waiting || tokio::time::Instant::now() >= deadline {
                        return Ok(json!({"waiting":waiting,"sessions":children}));
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            _ => {
                let mut children = Vec::new();
                for child in tidebreak_core::db::code::child_sessions(
                    &runtime.db,
                    &auth.parent.owner,
                    auth.parent.id,
                )
                .await?
                {
                    children.push(snapshot(runtime, &auth.parent.owner, child).await?);
                }
                Ok(json!({"sessions": children}))
            }
        }
    }

    async fn start(
        &self,
        runtime: &Arc<CodeRuntime>,
        auth: &Authority,
        args: &Value,
    ) -> Result<Value, ServerError> {
        let origin = CodeRuntime::canonical_external_repository(text(args, "repository", 256)?)?;
        let task = text(args, "task", 16000)?;
        let key = text(args, "request_key", 128)?;
        let harness: HarnessKind = match args.get("harness") {
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| ServerError::bad_request("harness is invalid"))?,
            None => HarnessKind::ClaudeCode,
        };
        if harness.is_in_process() {
            return Err(ServerError::bad_request(
                "Choose a workspace harness for repository work.",
            ));
        }
        let lock = self
            .host
            .starts
            .lock()
            .expect("session starts")
            .entry(auth.parent.id)
            .or_default()
            .clone();
        let _start = lock.lock().await;
        let auth = authority(runtime, auth.parent.id).await?;
        let children = tidebreak_core::db::code::child_sessions(
            &runtime.db,
            &auth.parent.owner,
            auth.parent.id,
        )
        .await?;
        for child in &children {
            let context = tidebreak_core::db::code::session_context(
                &runtime.db,
                &auth.parent.owner,
                child.id,
            )
            .await?
            .expect("child context");
            if context.request_key.as_deref() == Some(key) {
                let workspace = runtime
                    .get_workspace(
                        &auth.parent.owner,
                        child.workspace_id.ok_or_else(|| {
                            ServerError::conflict_kind(
                                "workspace_missing",
                                "The child has no workspace.",
                            )
                        })?,
                    )
                    .await?;
                let repo = runtime
                    .get_repo(&auth.parent.owner, workspace.repo_id)
                    .await?;
                if format!(
                    "{}/{}",
                    repo.origin_owner.unwrap_or_default(),
                    repo.origin_name.unwrap_or_default()
                )
                .to_ascii_lowercase()
                    != origin.to_ascii_lowercase()
                {
                    return Err(ServerError::conflict_kind(
                        "request_key_reused",
                        "This request_key already names work in a different repository.",
                    ));
                }
                send(runtime, &auth, child, task, key).await?;
                return snapshot(runtime, &auth.parent.owner, child.clone()).await;
            }
        }
        if children.len() >= 16
            || children
                .iter()
                .filter(|c| {
                    !matches!(
                        c.lifecycle,
                        SessionLifecycle::Ended | SessionLifecycle::Fenced | SessionLifecycle::Idle
                    )
                })
                .count()
                >= 8
        {
            return Err(ServerError::conflict_kind("child_session_limit", "A conversation can start at most 16 children and run at most 8 at once. Wait for running children before starting more."));
        }
        if let Some(grant) = auth
            .grant
            .as_ref()
            .filter(|grant| grant.kind.is_workspace())
        {
            let channel = auth.channel.as_deref().ok_or_else(|| ServerError::conflict_kind("channel_scope_missing", "Start a new Slack conversation to record its channel before selecting a repository."))?;
            if !tidebreak_core::db::code::channel_repository_is_confirmed(
                &runtime.db,
                &auth.parent.owner,
                grant.id,
                channel,
                &origin,
            )
            .await?
            {
                tidebreak_core::db::code::ensure_pending_channel_repository(
                    &runtime.db,
                    &auth.parent.owner,
                    grant.id,
                    channel,
                    &origin,
                    &auth.parent.id.to_string(),
                    "Parent conversation",
                )
                .await?;
                return Err(ServerError::conflict_kind("repository_unconfirmed", format!("An administrator must confirm {origin} for this Slack channel in Tidebreak Channels settings. Ask for that confirmation, then retry this request_key.")));
            }
        }
        if let Some(lender) = &auth.lender {
            // Refuse a missing person connection rather than silently moving
            // work to the bot after the parent has fixed its identity.
            lender
                .git_forge_identity(&auth.parent.owner, attribution(&auth.parent))
                .await
                .map_err(|e| {
                    ServerError::conflict_kind(
                        "git_forge_refused",
                        super::clone::git_forge_refusal_message(&e),
                    )
                })?;
        }
        let repo = if let Some(grant) = &auth.grant {
            runtime
                .prepare_external_repository(
                    &auth.parent.owner,
                    grant.id,
                    &origin,
                    attribution(&auth.parent),
                )
                .await?
        } else {
            runtime.repo_by_origin(&auth.parent.owner, &origin).await?
        };
        let settings = NewSessionSettings {
            permission_mode: auth.parent.permission_mode,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            permission_mode_ceiling: None,
            acts_as: Some(auth.parent.acts_as()),
        };
        let child = if let Some(grant) = &auth.grant {
            let external_key = format!("child/{}/{}", auth.parent.id, key);
            let (resolution, _) = runtime
                .external_get_or_create(
                    &auth.parent.owner,
                    auth.parent.owner_kind.as_deref(),
                    grant.id,
                    &grant.channel_kind,
                    &external_key,
                    repo.id,
                    Some(task.chars().take(60).collect()),
                    harness,
                    settings,
                    None,
                    Some(auth.parent.acts_as()),
                )
                .await?;
            let id = match resolution {
                ExternalSessionResolution::Created(binding)
                | ExternalSessionResolution::Existing(binding) => binding.session_id,
                _ => {
                    return Err(ServerError::conflict_kind(
                        "child_unavailable",
                        "This child session ended or belongs to another connection.",
                    ))
                }
            };
            runtime.get_session(&auth.parent.owner, id).await?
        } else {
            let workspace = runtime
                .create_workspace(
                    &auth.parent.owner,
                    repo.id,
                    Some(task.chars().take(60).collect()),
                    None,
                    None,
                )
                .await?;
            runtime
                .create_session(
                    &auth.parent.owner,
                    auth.parent.owner_kind.as_deref(),
                    workspace.id,
                    harness,
                    settings,
                )
                .await?
        };
        tidebreak_core::db::code::set_session_context(
            &runtime.db,
            &auth.parent.owner,
            child.id,
            auth.channel.as_deref(),
            Some(auth.parent.id),
            Some(key),
        )
        .await?;
        send(runtime, &auth, &child, task, key).await?;
        snapshot(runtime, &auth.parent.owner, child).await
    }
}

async fn require_child(
    runtime: &CodeRuntime,
    auth: &Authority,
    id: SessionId,
) -> Result<Session, ServerError> {
    let context =
        tidebreak_core::db::code::session_context(&runtime.db, &auth.parent.owner, id).await?;
    if context.and_then(|c| c.parent_session_id) != Some(auth.parent.id) {
        return Err(ServerError::not_found("child session not found"));
    }
    runtime.get_session(&auth.parent.owner, id).await
}

async fn send(
    runtime: &CodeRuntime,
    auth: &Authority,
    child: &Session,
    message: &str,
    key: &str,
) -> Result<(), ServerError> {
    let actor = tidebreak_core::TurnActor {
        display: Some("Parent conversation".into()),
        ..Default::default()
    };
    if let Some(grant) = &auth.grant {
        runtime
            .external_submit_message(
                &auth.parent.owner,
                grant.id,
                child.id,
                ExternalMessage {
                    text: message.into(),
                    event_id: format!("child/{}/{}", auth.parent.id, key),
                    channel_ts: chrono::Utc::now().timestamp_micros().to_string(),
                    actor,
                    context: None,
                },
            )
            .await?;
    } else {
        let record = tidebreak_core::db::code::record_external_message(
            &runtime.db,
            &auth.parent.owner,
            child.id,
            &format!("child/{}/{}", auth.parent.id, key),
            &chrono::Utc::now().timestamp_micros().to_string(),
            message,
            &actor,
        )
        .await?;
        if let tidebreak_core::ExternalMessageRecord::Recorded(row) = record {
            runtime.promote_external_head(child.clone(), row.id).await?;
        }
    }
    Ok(())
}

async fn snapshot(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    session: Session,
) -> Result<Value, ServerError> {
    let page = tidebreak_core::db::code::list_events(&runtime.db, owner, session.id, 0, 64).await?;
    let output: String = page
        .events
        .iter()
        .filter_map(|e| match &e.event {
            tidebreak_core::Event::AssistantMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        .chars()
        .take(12000)
        .collect();
    let approvals =
        tidebreak_core::db::code::list_approvals(&runtime.db, owner, None, Some(session.id))
            .await?;
    let pending = approvals
        .into_iter()
        .filter(|a| a.state.is_pending())
        .collect::<Vec<_>>();
    let running = !matches!(
        session.lifecycle,
        SessionLifecycle::Idle | SessionLifecycle::Ended | SessionLifecycle::Fenced
    ) && pending.is_empty();
    Ok(
        json!({"session_id":session.id,"workspace_id":session.workspace_id,"location":session.execution_location,
        "status":session.lifecycle,"running":running,"attention":session.attention,"output":output,
        "output_truncated":page.truncated,"approvals":pending}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::db::code::{insert_repo, insert_session};
    use tidebreak_core::{CodeRepo, RepoId};

    async fn setup() -> (
        tempfile::TempDir,
        Arc<CodeRuntime>,
        Arc<SessionTools>,
        Session,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            tidebreak_core::DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("self-drive.db").display()
            ))
            .await
            .unwrap(),
        );
        let mut runtime = CodeRuntime::new(
            db,
            dir.path().into(),
            Some(dir.path().join("worktrees")),
            None,
            None,
            None,
            None,
            None,
        );
        runtime
            .adapters
            .register(Arc::new(crate::scripted_harness::ScriptedAdapter::new(
                crate::scripted_harness::plain_text_script(),
            )));
        let runtime = Arc::new(runtime);
        let host = Arc::new(SessionTools::default());
        host.attach(&runtime);
        let mut parent = crate::code::remote::fixtures::session_value();
        parent.workspace_id = None;
        parent.harness_kind = HarnessKind::Internal;
        insert_session(&runtime.db, &parent).await.unwrap();
        for name in ["one", "two"] {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            for args in [
                vec!["init", "-b", "main"],
                vec![
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.com",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial",
                ],
            ] {
                assert!(std::process::Command::new("git")
                    .args(args)
                    .current_dir(&root)
                    .output()
                    .unwrap()
                    .status
                    .success());
            }
            insert_repo(
                &runtime.db,
                &CodeRepo {
                    id: RepoId::new(),
                    owner: parent.owner.clone(),
                    root_path: root.display().to_string(),
                    display_name: name.into(),
                    default_base_ref: "main".into(),
                    branch_prefix: "thet/".into(),
                    setup_script: None,
                    archive_script: None,
                    quick_actions: vec![],
                    created_at: chrono::Utc::now(),
                    removed_at: None,
                    cloned_from: None,
                    origin_host: Some("github.com".into()),
                    origin_owner: Some("acme".into()),
                    origin_name: Some(name.into()),
                },
            )
            .await
            .unwrap();
        }
        (dir, runtime, host, parent)
    }

    #[tokio::test]
    async fn a_conversation_chooses_two_repositories_and_waits_without_a_parent_workspace() {
        let (_dir, runtime, host, parent) = setup().await;
        let ctx = ToolCtx::without_private_scratch(parent.id, None);
        let tool = SessionTool {
            host: host.clone(),
            name: "code_session_create",
        };
        let first_args = json!({"repository":"acme/one","task":"Inspect one","request_key":"one"});
        let first = tool.run(&runtime, &ctx, first_args.clone()).await.unwrap();
        let second = tool
            .run(
                &runtime,
                &ctx,
                json!({"repository":"acme/two","task":"Inspect two","request_key":"two"}),
            )
            .await
            .unwrap();
        assert_ne!(first["workspace_id"], second["workspace_id"]);
        let retry = tool.run(&runtime, &ctx, first_args).await.unwrap();
        assert_eq!(first["session_id"], retry["session_id"]);
        let wait = SessionTool {
            host: host.clone(),
            name: "code_wait",
        };
        let result = wait
            .run(
                &runtime,
                &ctx,
                json!({"session_ids":[second["session_id"], first["session_id"]]}),
            )
            .await
            .unwrap();
        assert_eq!(result["waiting"], false);
        assert_eq!(result["sessions"][0]["session_id"], second["session_id"]);
        assert!(!result["sessions"][0]["output"].as_str().unwrap().is_empty());
        let first_id = serde_json::from_value(first["session_id"].clone()).unwrap();
        assert_eq!(
            tidebreak_core::db::code::list_turns(&runtime.db, &parent.owner, first_id)
                .await
                .unwrap()
                .len(),
            1,
            "a retried tool call must not submit another turn"
        );
        assert!(runtime
            .get_session(&parent.owner, parent.id)
            .await
            .unwrap()
            .workspace_id
            .is_none());
        let mut stranger = parent.clone();
        stranger.id = SessionId::new();
        insert_session(&runtime.db, &stranger).await.unwrap();
        let foreign = ToolCtx::without_private_scratch(stranger.id, None);
        let denied = wait
            .run(
                &runtime,
                &foreign,
                json!({"session_ids":[first["session_id"]]}),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), "not_found");
    }

    #[tokio::test]
    async fn revoked_external_authority_cannot_discover_or_start_children() {
        let (_dir, runtime, host, parent) = setup().await;
        let (grant, _) = runtime
            .mint_adapter_grant(&parent.owner, "slack", "U", "W")
            .await
            .unwrap();
        tidebreak_core::db::code::bind_external_session(
            &runtime.db,
            &parent.owner,
            grant.id,
            "slack",
            "W/C/1",
            parent.id,
        )
        .await
        .unwrap();
        runtime
            .revoke_adapter_grant(&parent.owner, grant.id, "test revocation")
            .await
            .unwrap();
        let tool = SessionTool {
            host,
            name: "code_repos",
        };
        let denied = tool
            .run(
                &runtime,
                &ToolCtx::without_private_scratch(parent.id, None),
                json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            denied.kind(),
            "unauthorized" | "parent_unavailable"
        ));
    }
}
