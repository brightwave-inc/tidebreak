//! `create_app` — publish a local mini-app the user can reopen from their
//! library.
//!
//! The tool records a profile-scoped app: an untrusted HTML bundle published
//! write-once under the profile data directory, paired with a trusted manifest
//! naming the mounted MCP tools the app may call. Identity follows the output
//! tools' discipline — the app and revision ids derive from the durable call
//! id, so a retried call lands on the record it already created instead of
//! forking a second one.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::connected_app::ConnectedAppKind;
use crate::error::Result;
use crate::id::{AppId, AppRevisionId, TurnId};
use crate::local_app::{
    mounted_tool_under, publish_app_bundle, validate_app_manifest, AppManifest, AppRecord,
    CreateApp, NewAppRevision, MAX_APP_BUNDLE_BYTES,
};
use crate::preview::{ResultEntry, ResultEntryKind};
use crate::storage::Store;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;

/// `create_app` — create a profile-scoped local app or append a revision.
pub struct CreateAppTool {
    store: Arc<dyn Store>,
    profile_data_dir: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    #[schemars(
        description = "The complete self-contained HTML document the app renders. \
                       It runs in a sandboxed frame with no network access."
    )]
    bundle_html: String,
    #[schemars(
        description = "The app's manifest: its display name and the exact mounted \
                       MCP tools it may call, grouped by connected app. Each binding \
                       names a connected app by id (the tool description lists the \
                       configured ids) and the full mounted tool names under it."
    )]
    manifest: AppManifest,
    #[serde(default)]
    #[schemars(
        description = "An app_id returned by an earlier create_app call. Provide it \
                       to publish a new revision of that app; omit it to create a \
                       new app."
    )]
    app_id: Option<Uuid>,
}

impl CreateAppTool {
    /// A tool recording apps in `store` and bundle bytes under the profile
    /// data directory.
    #[must_use]
    pub fn new(store: Arc<dyn Store>, profile_data_dir: PathBuf) -> Self {
        Self {
            store,
            profile_data_dir,
        }
    }

    /// Verify the executing call is durably recorded in this conversation and
    /// return the turn that owns it.
    ///
    /// Identity derives from the call id, so the id must be one the host
    /// minted for this chat — not a value any argument could steer — before it
    /// is allowed to name profile-scoped records.
    async fn owned_call_turn(&self, ctx: &ToolCtx) -> std::result::Result<TurnId, ToolOutput> {
        let Some(call_id) = ctx.call_id else {
            return Err(ToolOutput::error(
                "create_app is only available from a recorded agent turn",
            ));
        };
        let calls = self
            .store
            .list_tool_calls(ctx.chat_id)
            .await
            .map_err(|_| ToolOutput::error("could not verify this call's identity"))?;
        calls
            .into_iter()
            .find(|call| call.id == call_id)
            .map(|call| call.turn_id)
            .ok_or_else(|| ToolOutput::error("this call is not recorded in this conversation"))
    }

    /// Check that every manifest binding names a configured `mcp_server`
    /// connected app and that each pinned tool is a mounted name under that
    /// app's namespace.
    async fn check_bindings(&self, manifest: &AppManifest) -> std::result::Result<(), String> {
        if manifest.bindings.is_empty() {
            return Ok(());
        }
        let connected = self
            .store
            .list_connected_apps()
            .await
            .map_err(|_| "could not read the configured connected apps".to_owned())?;
        for binding in &manifest.bindings {
            let Some(app) = connected.iter().find(|app| app.id == binding.app) else {
                let configured: Vec<String> = connected
                    .iter()
                    .filter(|app| app.kind == ConnectedAppKind::McpServer)
                    .map(|app| format!("{} ({})", app.id, app.name))
                    .collect();
                return Err(if configured.is_empty() {
                    format!(
                        "connected app {} is not configured, and no connected apps \
                         are; only an empty bindings list is valid",
                        binding.app
                    )
                } else {
                    format!(
                        "connected app {} is not configured; bind one of: {}",
                        binding.app,
                        configured.join(", ")
                    )
                });
            };
            if app.kind != ConnectedAppKind::McpServer {
                return Err(format!(
                    "connected app {} ({}) is a {} app; only mcp_server apps \
                     contribute mounted tools",
                    app.id, app.name, app.kind
                ));
            }
            for tool in &binding.tools {
                if mounted_tool_under(&app.name, tool).is_none() {
                    return Err(format!(
                        "tool {tool:?} is not mounted under connected app {} — its \
                         tools are named `mcp__{}__{{tool}}`",
                        app.id, app.name
                    ));
                }
            }
        }
        Ok(())
    }

    fn success(record: &AppRecord, ordinal: u32, created: bool) -> ToolOutput {
        let verb = if created {
            "Created app"
        } else {
            "Published a revision of app"
        };
        ToolOutput::text(format!(
            "{verb} {:?} (app_id {}, revision {ordinal}). Pass this app_id to create_app \
             to publish a further revision.",
            record.name, record.id,
        ))
        .with_entries(vec![ResultEntry::new(
            ResultEntryKind::Output,
            record.name.clone(),
        )
        .with_meta(format!("revision {ordinal}"))])
    }
}

#[async_trait]
impl Tool for CreateAppTool {
    fn spec(&self) -> ToolSpec {
        definitions::create_app()
    }

    fn approval_class(&self) -> ApprovalClass {
        // Publishes durable profile state the user can inspect and delete;
        // nothing leaves the machine and no pinned tool runs until the user
        // grants the app its bindings.
        ApprovalClass::Workspace
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: Arguments = match arguments::parse(args) {
            Ok(args) => args,
            Err(output) => return Ok(output),
        };
        let turn_id = match self.owned_call_turn(ctx).await {
            Ok(turn_id) => turn_id,
            Err(output) => return Ok(output),
        };
        // `ctx.call_id` was verified against the durable record just above.
        let call_id = ctx.call_id.expect("owned_call_turn requires a call id");
        if let Err(message) = validate_app_manifest(&args.manifest) {
            return Ok(ToolOutput::error(format!(
                "invalid app manifest: {message}"
            )));
        }
        // Resolve every binding against the configured connected apps at the
        // authoring door, so a wrong id or namespace fails here with a
        // teachable error instead of at the user's first open. The invoke
        // gate re-checks all of this live; this is legibility, not the gate.
        if let Err(message) = self.check_bindings(&args.manifest).await {
            return Ok(ToolOutput::error(message));
        }
        let bundle = args.bundle_html.as_bytes();
        if bundle.is_empty() {
            return Ok(ToolOutput::error("bundle_html must not be empty"));
        }
        if bundle.len() > MAX_APP_BUNDLE_BYTES {
            return Ok(ToolOutput::error(format!(
                "bundle_html is {} bytes; the limit is {MAX_APP_BUNDLE_BYTES}",
                bundle.len()
            )));
        }

        // Identity: both ids derive from the durable call id, so re-executing
        // the same call re-derives the same identities.
        let revision_id = AppRevisionId::for_call(call_id);
        let app_id = match args.app_id {
            Some(app_id) if !app_id.is_nil() => AppId::from(app_id),
            Some(_) => return Ok(ToolOutput::error("app_id must be a real app id")),
            None => AppId::for_call(call_id),
        };
        // Appending must address an app that exists before any bytes are
        // published under its id; the store re-checks inside the transaction.
        if args.app_id.is_some() {
            match self.store.get_app(app_id).await {
                Ok(Some(app)) if app.deleted_at.is_none() => {}
                Ok(Some(_)) => {
                    return Ok(ToolOutput::error(
                        "that app was deleted; create a new app instead",
                    ))
                }
                Ok(None) => {
                    return Ok(ToolOutput::error(
                        "no app with that app_id; omit app_id to create a new app",
                    ))
                }
                Err(_) => return Ok(ToolOutput::error("could not look up that app")),
            }
        }

        let byte_len = bundle.len() as u64;
        let sha256: [u8; 32] = Sha256::digest(bundle).into();
        // Recognize an exact retry before writing anything: the same call
        // republishing the same content reports the record it already made,
        // while a call id reused for different content is refused.
        match self.store.get_app_revision(revision_id).await {
            Ok(Some(recorded)) => {
                let exact = recorded.app_id == app_id
                    && recorded.byte_len == byte_len
                    && recorded.sha256 == sha256
                    && recorded.manifest == args.manifest;
                if !exact {
                    return Ok(ToolOutput::error(
                        "this call already published a different app revision",
                    ));
                }
                return Ok(match self.store.get_app(app_id).await {
                    Ok(Some(record)) => {
                        Self::success(&record, recorded.ordinal, recorded.ordinal == 1)
                    }
                    _ => ToolOutput::error("could not read back the app record"),
                });
            }
            Ok(None) => {}
            Err(_) => return Ok(ToolOutput::error("could not check for an earlier attempt")),
        }

        // Publish the bundle bytes before recording the row, so a recorded
        // revision always has its content: the write-once publication re-reads
        // and byte-compares on collision, making it safe to retry.
        let profile_dir = match std::fs::create_dir_all(&self.profile_data_dir).and_then(|()| {
            cap_std::fs::Dir::open_ambient_dir(&self.profile_data_dir, cap_std::ambient_authority())
        }) {
            Ok(dir) => dir,
            Err(_) => {
                return Ok(ToolOutput::error(
                    "the profile data directory is unavailable",
                ))
            }
        };
        if publish_app_bundle(&profile_dir, app_id, revision_id, bundle)
            .await
            .is_err()
        {
            return Ok(ToolOutput::error("could not publish the app bundle"));
        }

        let revision = NewAppRevision {
            id: revision_id,
            manifest: args.manifest,
            byte_len,
            sha256,
            turn_id: Some(turn_id),
            producing_run_id: None,
            chat_id: Some(ctx.chat_id),
            created_at: Utc::now(),
        };
        let recorded = if args.app_id.is_some() {
            self.store.append_app_revision(app_id, &revision).await
        } else {
            self.store
                .create_app(&CreateApp {
                    id: app_id,
                    revision,
                })
                .await
        };
        Ok(match recorded {
            Ok(record) => Self::success(&record, record.revision_count, args.app_id.is_none()),
            Err(error) => ToolOutput::error(format!("could not record the app: {error}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::db::DbStore;
    use crate::id::{CallId, ChatId, ConnectedAppId, TurnId};
    use crate::local_app::app_revision_relative_path;
    use crate::model::{Chat, ToolCallExecution, ToolCallRecord, ToolCallStatus};
    use crate::preview::ToolResultPreview;

    use super::*;

    struct Fixture {
        _directory: tempfile::TempDir,
        store: Arc<DbStore>,
        tool: CreateAppTool,
        chat_id: ChatId,
        turn_id: TurnId,
        profile_dir: PathBuf,
        sentry: ConnectedAppId,
    }

    async fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("create-app.db").display()
            ))
            .await
            .unwrap(),
        );
        // One configured connected app for bindings to resolve against.
        let sentry = ConnectedAppId::new();
        store
            .replace_connected_apps(
                ConnectedAppKind::McpServer,
                &[crate::connected_app::ConnectedApp {
                    id: sentry,
                    name: "sentry".into(),
                    kind: ConnectedAppKind::McpServer,
                    definition: json!({ "name": "sentry", "command": "sentry-mcp" }),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }],
            )
            .await
            .unwrap();
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "gpt-5", "make an app")
            .await
            .unwrap();
        let profile_dir = directory.path().join("profile-data");
        let tool = CreateAppTool::new(store.clone(), profile_dir.clone());
        Fixture {
            _directory: directory,
            store,
            tool,
            chat_id: chat.id,
            turn_id,
            profile_dir,
            sentry,
        }
    }

    impl Fixture {
        /// Record one durable `create_app` call and return the invocation
        /// context executing it, the way the turn loop would.
        async fn recorded_call(&self) -> (CallId, ToolCtx) {
            let call_id = CallId::new();
            self.store
                .accept_tool_call(&ToolCallRecord {
                    id: call_id,
                    chat_id: self.chat_id,
                    turn_id: self.turn_id,
                    provider_id: call_id.to_string(),
                    name: crate::local_app::CREATE_APP_TOOL.into(),
                    arguments: json!({}),
                    raw_arguments: None,
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    result_preview: None,
                    error_code: None,
                    error_detail: None,
                    client_executor_id: None,
                    client_lease_expires_at: None,
                    created_at: Utc::now(),
                    resolved_at: None,
                })
                .await
                .unwrap();
            let ctx = ToolCtx::without_private_scratch(self.chat_id, None).with_call_id(call_id);
            (call_id, ctx)
        }
    }

    fn arguments_bound_to(connected: ConnectedAppId, app_id: Option<Uuid>) -> Value {
        let mut arguments = json!({
            "bundle_html": "<!doctype html><h1>Triage</h1>",
            "manifest": {
                "name": "Sentry triage",
                "bindings": [
                    { "app": connected, "tools": ["mcp__sentry__list_issues"] }
                ],
            },
        });
        if let Some(app_id) = app_id {
            arguments["app_id"] = json!(app_id);
        }
        arguments
    }

    #[tokio::test]
    async fn identity_is_retry_stable_and_appends_take_the_app_id() {
        let fixture = fixture().await;
        let (call_id, ctx) = fixture.recorded_call().await;

        let first = fixture
            .tool
            .execute(&ctx, arguments_bound_to(fixture.sentry, None))
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.content);
        let app_id = AppId::for_call(call_id);
        assert!(first.content.contains(&app_id.to_string()));
        // The bundle bytes are on disk at the identity-derived path before the
        // record exists to point at them.
        let bundle_path = fixture.profile_dir.join(app_revision_relative_path(
            app_id,
            AppRevisionId::for_call(call_id),
        ));
        assert!(bundle_path.is_file());

        // Re-executing the exact call reports the same record instead of
        // forking a second app or a second revision.
        let retried = fixture
            .tool
            .execute(&ctx, arguments_bound_to(fixture.sentry, None))
            .await
            .unwrap();
        assert!(!retried.is_error, "{}", retried.content);
        assert!(retried.content.contains(&app_id.to_string()));
        assert_eq!(fixture.store.list_apps(10).await.unwrap().len(), 1);
        let record = fixture.store.get_app(app_id).await.unwrap().unwrap();
        assert_eq!(record.revision_count, 1);

        // The same call id must not be able to publish different content.
        let mut different = arguments_bound_to(fixture.sentry, None);
        different["bundle_html"] = json!("<!doctype html><h1>Changed</h1>");
        let refused = fixture.tool.execute(&ctx, different).await.unwrap();
        assert!(refused.is_error);

        // A later call appends to the named app rather than creating one.
        let (_, append_ctx) = fixture.recorded_call().await;
        let appended = fixture
            .tool
            .execute(
                &append_ctx,
                arguments_bound_to(fixture.sentry, Some(app_id.0)),
            )
            .await
            .unwrap();
        assert!(!appended.is_error, "{}", appended.content);
        assert!(appended.content.contains("revision 2"));
        assert_eq!(fixture.store.list_apps(10).await.unwrap().len(), 1);

        // Appending to an app that does not exist is refused, and refuses
        // before any bytes could be published under the bogus id.
        let (wrong_call, wrong_ctx) = fixture.recorded_call().await;
        let missing_app = Uuid::new_v4();
        let refused = fixture
            .tool
            .execute(
                &wrong_ctx,
                arguments_bound_to(fixture.sentry, Some(missing_app)),
            )
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(refused.content.contains("no app with that app_id"));
        assert!(!fixture
            .profile_dir
            .join(app_revision_relative_path(
                AppId::from(missing_app),
                AppRevisionId::for_call(wrong_call),
            ))
            .exists());
    }

    #[tokio::test]
    async fn refusals_are_tool_errors_not_panics() {
        let fixture = fixture().await;

        // A manifest pinning a name that could never match a mounted tool.
        let (_, ctx) = fixture.recorded_call().await;
        let mut bad_manifest = arguments_bound_to(fixture.sentry, None);
        bad_manifest["manifest"]["bindings"][0]["tools"] = json!(["list_issues"]);
        let refused = fixture.tool.execute(&ctx, bad_manifest).await.unwrap();
        assert!(refused.is_error);
        assert!(refused.content.contains("invalid app manifest"));

        // An oversized bundle is refused before anything is written.
        let (_, ctx) = fixture.recorded_call().await;
        let mut oversized = arguments_bound_to(fixture.sentry, None);
        oversized["bundle_html"] = json!("x".repeat(MAX_APP_BUNDLE_BYTES + 1));
        assert!(
            fixture
                .tool
                .execute(&ctx, oversized)
                .await
                .unwrap()
                .is_error
        );

        // A call id the conversation never recorded cannot mint identity.
        let foreign_ctx =
            ToolCtx::without_private_scratch(fixture.chat_id, None).with_call_id(CallId::new());
        let refused = fixture
            .tool
            .execute(&foreign_ctx, arguments_bound_to(fixture.sentry, None))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(refused.content.contains("not recorded"));
        assert_eq!(fixture.store.list_apps(10).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn the_projection_carries_the_name_and_ordinal_and_never_the_bundle() {
        let fixture = fixture().await;
        let (_, ctx) = fixture.recorded_call().await;
        let output = fixture
            .tool
            .execute(&ctx, arguments_bound_to(fixture.sentry, None))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);

        let preview = ToolResultPreview::build(crate::local_app::CREATE_APP_TOOL, &output)
            .expect("create_app projects an entries card");
        let json = serde_json::to_string(&preview).unwrap();
        assert!(json.contains("Sentry triage"));
        assert!(json.contains("revision 1"));
        // Renderer-safe: no bundle markup, no manifest bindings, no ids.
        assert!(!json.contains("doctype"));
        assert!(!json.contains("mcp__sentry__list_issues"));
        assert!(!json.contains(&AppId::for_call(ctx.call_id.unwrap()).to_string()));
    }
}
