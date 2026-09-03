//! `create_app` — publish a local mini-app the user can reopen from their
//! library.
//!
//! The tool records a profile-scoped app: an untrusted HTML bundle published
//! write-once under the profile data directory, paired with a trusted manifest
//! naming the connected-app operations the app may call. Identity follows the
//! output tools' discipline — the app and revision ids derive from the durable
//! call id, so a retried call lands on the record it already created instead
//! of forking a second one.

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
    publish_app_bundle, validate_app_manifest, AppBinding, AppManifest, AppRecord, CreateApp,
    NewAppRevision, MAX_APP_BUNDLE_BYTES,
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
    /// Authoring-time lookup of approved connected folders, when this
    /// embedding has one. Absent, folder bindings refuse at the door.
    folders: Option<Arc<dyn crate::local_app::ApprovedFolderSource>>,
    /// Authoring-time lookup of the gateway's connected apps, when this
    /// embedding has one. Absent, gateway bindings refuse at the door.
    gateway_apps: Option<Arc<dyn crate::local_app::GatewayAppSource>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    #[schemars(
        description = "The complete self-contained HTML document the app renders. \
                       It runs in a sandboxed frame with no network access."
    )]
    bundle_html: String,
    #[schemars(description = "The app's manifest: its display name and the exact \
                       capabilities it may call, grouped by binding. A binding \
                       either names a rest_api connected app by id with the \
                       declared OpenAPI operationIds it may execute \
                       (`{app, operation_ids}`), an approved connected folder \
                       by root id with an access level \
                       (`{folder, access: \"read\"|\"read_write\"}`), or a \
                       connected app of the model gateway by its gateway id \
                       with the operation ids it declares \
                       (`{gateway_app, operation_ids}`). The tool \
                       description lists the available ids. Mounted MCP tools \
                       cannot be bound.")]
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
            folders: None,
            gateway_apps: None,
        }
    }

    /// Let the door resolve folder bindings against the host's approved
    /// connected folders (docs/folder-bindings.md).
    #[must_use]
    pub fn with_approved_folders(
        mut self,
        folders: Arc<dyn crate::local_app::ApprovedFolderSource>,
    ) -> Self {
        self.folders = Some(folders);
        self
    }

    /// Let the door resolve gateway bindings against the connected apps the
    /// model gateway holds for this profile.
    #[must_use]
    pub fn with_gateway_apps(
        mut self,
        gateway_apps: Arc<dyn crate::local_app::GatewayAppSource>,
    ) -> Self {
        self.gateway_apps = Some(gateway_apps);
        self
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

    /// Check that every manifest binding names a configured connected app,
    /// approved folder, or entitled gateway app and speaks a live vocabulary:
    /// operation bindings resolve to a `rest_api` record with each pinned
    /// `operationId` declared by its ingested catalog, and gateway bindings
    /// resolve to an app the gateway currently declares, with each pinned
    /// operation id in the catalog it declares for it.
    async fn check_bindings(&self, manifest: &AppManifest) -> std::result::Result<(), String> {
        if manifest.bindings.is_empty() {
            return Ok(());
        }
        let connected = self
            .store
            .list_connected_apps()
            .await
            .map_err(|_| "could not read the configured connected apps".to_owned())?;
        // Resolved once for the whole manifest: a manifest may carry several
        // gateway bindings and the lookup is a live read across the network.
        // `None` covers both no seam and no session — the door cannot tell
        // them apart and does not need to, because the fix for both is the
        // same sentence.
        let gateway_apps = match &self.gateway_apps {
            Some(source)
                if manifest
                    .bindings
                    .iter()
                    .any(|binding| binding.gateway_app().is_some()) =>
            {
                source.entitled_apps().await
            }
            _ => None,
        };
        for binding in &manifest.bindings {
            // A gateway binding names an app only the gateway can resolve:
            // no definition, no catalog, and no credential for it exists on
            // this machine.
            if let AppBinding::GatewayOperations(binding) = binding {
                let Some(entitled) = &gateway_apps else {
                    return Err(
                        "this profile has no gateway session, so gateway app bindings \
                         cannot be authored"
                            .to_owned(),
                    );
                };
                let Some(app) = entitled.iter().find(|app| app.id == binding.gateway_app) else {
                    let available: Vec<String> = entitled
                        .iter()
                        .map(|app| format!("{} ({})", app.id, app.name))
                        .collect();
                    return Err(if available.is_empty() {
                        format!(
                            "gateway app {:?} is not available to this profile, and no \
                             gateway apps are",
                            binding.gateway_app
                        )
                    } else {
                        format!(
                            "gateway app {:?} is not available to this profile; bind one \
                             of: {}",
                            binding.gateway_app,
                            available.join(", ")
                        )
                    });
                };
                for operation_id in &binding.operation_ids {
                    if !app.operation_ids.contains(operation_id) {
                        return Err(format!(
                            "operation {operation_id:?} is not declared by gateway app \
                             {} ({})",
                            app.id, app.name
                        ));
                    }
                }
                continue;
            }
            // A folder binding resolves against the host's approved
            // connected folders — when this embedding has none, the door
            // says so instead of admitting an ungrantable pin.
            if let AppBinding::Folder(binding) = binding {
                let Some(folders) = &self.folders else {
                    return Err(
                        "connected folders are not available here, so folder bindings \
                         cannot be authored"
                            .to_owned(),
                    );
                };
                let approved = folders.approved_folders().await;
                if !approved.iter().any(|(id, _)| *id == binding.folder) {
                    let available: Vec<String> = approved
                        .iter()
                        .map(|(id, name)| format!("{id} ({name})"))
                        .collect();
                    return Err(if available.is_empty() {
                        format!(
                            "folder {} is not an approved connected folder, and none \
                             are approved",
                            binding.folder
                        )
                    } else {
                        format!(
                            "folder {} is not an approved connected folder; bind one \
                             of: {}",
                            binding.folder,
                            available.join(", ")
                        )
                    });
                }
                continue;
            }
            let binding_app = binding
                .app()
                .expect("folder and gateway bindings returned above");
            let Some(app) = connected.iter().find(|app| app.id == binding_app) else {
                let configured: Vec<String> = connected
                    .iter()
                    .map(|app| format!("{} ({}, {})", app.id, app.name, app.kind))
                    .collect();
                return Err(if configured.is_empty() {
                    format!(
                        "connected app {binding_app} is not configured, and no connected \
                         apps are; only an empty bindings list is valid"
                    )
                } else {
                    format!(
                        "connected app {binding_app} is not configured; bind one of: {}",
                        configured.join(", ")
                    )
                });
            };
            match binding {
                // Handled above — neither a folder nor a gateway binding has
                // a local connected app to resolve.
                AppBinding::Folder(_) | AppBinding::GatewayOperations(_) => {}
                AppBinding::Operations(binding) => {
                    if app.kind != ConnectedAppKind::RestApi {
                        return Err(format!(
                            "connected app {} ({}) is a {} app; only rest_api apps \
                             contribute bindable operations",
                            app.id, app.name, app.kind
                        ));
                    }
                    // A lenient read of the definition's catalog: the typed
                    // shape and its validation live server-side, and an
                    // authoring cross-check only needs the declared ids. A
                    // definition without a readable catalog fails the binding
                    // closed rather than admitting an uncheckable pin.
                    let Some(operations) = app
                        .definition
                        .get("catalog")
                        .and_then(|catalog| catalog.get("operations"))
                        .and_then(serde_json::Value::as_object)
                    else {
                        return Err(format!(
                            "connected app {} ({}) has no readable operation catalog, \
                             so its operations cannot be bound",
                            app.id, app.name
                        ));
                    };
                    for operation_id in &binding.operation_ids {
                        if !operations.contains_key(operation_id) {
                            return Err(format!(
                                "operation {operation_id:?} is not declared by connected \
                                 app {} ({})",
                                app.id, app.name
                            ));
                        }
                    }
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
            ResultEntryKind::App,
            record.name.clone(),
        )
        .with_meta(format!("revision {ordinal}"))
        // The row's navigation target: the renderer opens the app in the
        // library panel by this id and never prints it.
        .with_target_id(record.id.to_string())])
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
            match self.store.get_app_for_chat(ctx.chat_id, app_id).await {
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
        match self
            .store
            .get_app_revision_for_chat(ctx.chat_id, revision_id)
            .await
        {
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
                return Ok(
                    match self.store.get_app_for_chat(ctx.chat_id, app_id).await {
                        Ok(Some(record)) => {
                            Self::success(&record, recorded.ordinal, recorded.ordinal == 1)
                        }
                        _ => ToolOutput::error("could not read back the app record"),
                    },
                );
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
            self.store
                .append_app_revision_for_chat(ctx.chat_id, app_id, &revision)
                .await
        } else {
            self.store
                .create_app_for_chat(
                    ctx.chat_id,
                    &CreateApp {
                        id: app_id,
                        revision,
                    },
                )
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
        issues: ConnectedAppId,
    }

    async fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect_test_sqlite_fixture(&format!(
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
        // And one rest_api record, so operation bindings resolve too. The
        // door reads the catalog leniently, so a minimal definition works.
        let issues = ConnectedAppId::new();
        store
            .replace_connected_apps(
                ConnectedAppKind::RestApi,
                &[crate::connected_app::ConnectedApp {
                    id: issues,
                    name: "issues".into(),
                    kind: ConnectedAppKind::RestApi,
                    definition: json!({
                        "base_url": "https://api.example.com",
                        "catalog": {
                            "document_sha256": "abc",
                            "operations": { "listIssues": {} },
                        },
                    }),
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
            memory_incognito: false,
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
            issues,
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

                    provider_replay: None,
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
                "name": "Issue triage",
                "bindings": [
                    { "app": connected, "operation_ids": ["listIssues"] }
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
            .execute(&ctx, arguments_bound_to(fixture.issues, None))
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
            .execute(&ctx, arguments_bound_to(fixture.issues, None))
            .await
            .unwrap();
        assert!(!retried.is_error, "{}", retried.content);
        assert!(retried.content.contains(&app_id.to_string()));
        assert_eq!(fixture.store.list_apps(10).await.unwrap().len(), 1);
        let record = fixture.store.get_app(app_id).await.unwrap().unwrap();
        assert_eq!(record.revision_count, 1);

        // The same call id must not be able to publish different content.
        let mut different = arguments_bound_to(fixture.issues, None);
        different["bundle_html"] = json!("<!doctype html><h1>Changed</h1>");
        let refused = fixture.tool.execute(&ctx, different).await.unwrap();
        assert!(refused.is_error);

        // A later call appends to the named app rather than creating one.
        let (_, append_ctx) = fixture.recorded_call().await;
        let appended = fixture
            .tool
            .execute(
                &append_ctx,
                arguments_bound_to(fixture.issues, Some(app_id.0)),
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
                arguments_bound_to(fixture.issues, Some(missing_app)),
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

        // A manifest pinning an operation id outside the ingest grammar.
        let (_, ctx) = fixture.recorded_call().await;
        let mut bad_manifest = arguments_bound_to(fixture.issues, None);
        bad_manifest["manifest"]["bindings"][0]["operation_ids"] = json!(["not an operation id"]);
        let refused = fixture.tool.execute(&ctx, bad_manifest).await.unwrap();
        assert!(refused.is_error);
        assert!(refused.content.contains("invalid app manifest"));

        // An oversized bundle is refused before anything is written.
        let (_, ctx) = fixture.recorded_call().await;
        let mut oversized = arguments_bound_to(fixture.issues, None);
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
            .execute(&foreign_ctx, arguments_bound_to(fixture.issues, None))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(refused.content.contains("not recorded"));
        assert_eq!(fixture.store.list_apps(10).await.unwrap().len(), 0);
    }

    /// The authoring door resolves each binding's vocabulary against the
    /// record's kind and, for operations, against the record's declared
    /// catalog — so a wrong kind or an undeclared operation fails here with a
    /// teachable error rather than at the user's first open.
    #[tokio::test]
    async fn operation_bindings_resolve_kind_and_catalog_at_the_door() {
        let fixture = fixture().await;
        let operations_manifest = |app: ConnectedAppId, operation_ids: &[&str]| {
            json!({
                "bundle_html": "<!doctype html><h1>Issues</h1>",
                "manifest": {
                    "name": "Issue browser",
                    "bindings": [{ "app": app, "operation_ids": operation_ids }],
                },
            })
        };

        // A declared operation on a rest_api record publishes.
        let (_, ctx) = fixture.recorded_call().await;
        let created = fixture
            .tool
            .execute(&ctx, operations_manifest(fixture.issues, &["listIssues"]))
            .await
            .unwrap();
        assert!(!created.is_error, "{}", created.content);

        // An operation binding against an mcp_server record is refused.
        let (_, ctx) = fixture.recorded_call().await;
        let refused = fixture
            .tool
            .execute(&ctx, operations_manifest(fixture.sentry, &["listIssues"]))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(
            refused.content.contains("only rest_api apps"),
            "{}",
            refused.content
        );

        // An operation the catalog does not declare is refused.
        let (_, ctx) = fixture.recorded_call().await;
        let refused = fixture
            .tool
            .execute(&ctx, operations_manifest(fixture.issues, &["ghostOp"]))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(
            refused.content.contains("not declared"),
            "{}",
            refused.content
        );

        // The tools vocabulary is removed (#1332, #1589): a manifest pinning
        // `tools` no longer parses as any binding shape, and the refusal
        // carries the live schema so the model sees what is bindable.
        for connected in [fixture.sentry, fixture.issues] {
            let (_, ctx) = fixture.recorded_call().await;
            let tools_manifest = json!({
                "bundle_html": "<!doctype html><h1>Triage</h1>",
                "manifest": {
                    "name": "Issue triage",
                    "bindings": [{ "app": connected, "tools": ["mcp__sentry__list_issues"] }],
                },
            });
            let refused = fixture.tool.execute(&ctx, tools_manifest).await.unwrap();
            assert!(refused.is_error);
            assert!(
                refused.content.contains("invalid arguments"),
                "{}",
                refused.content
            );
        }
    }

    /// The door resolves folder bindings against the host's approved
    /// folders: no source (headless embeddings) refuses honestly, an
    /// unapproved id refuses with the available folders spelled out, and an
    /// approved one publishes (docs/folder-bindings.md).
    #[tokio::test]
    async fn folder_bindings_resolve_against_approved_folders_at_the_door() {
        use crate::id::HostRootId;
        use crate::local_app::ApprovedFolderSource;

        struct StaticFolders(Vec<(HostRootId, String)>);

        #[async_trait]
        impl ApprovedFolderSource for StaticFolders {
            async fn approved_folders(&self) -> Vec<(HostRootId, String)> {
                self.0.clone()
            }
        }

        let folder_manifest = |folder: Uuid| {
            json!({
                "bundle_html": "<!doctype html><h1>Files</h1>",
                "manifest": {
                    "name": "File browser",
                    "bindings": [{ "folder": folder, "access": "read_write" }],
                },
            })
        };

        // Without a source, the door refuses honestly.
        let sourceless = fixture().await;
        let (_, ctx) = sourceless.recorded_call().await;
        let refused = sourceless
            .tool
            .execute(&ctx, folder_manifest(Uuid::new_v4()))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(
            refused.content.contains("not available here"),
            "{}",
            refused.content
        );

        // With a source, an unapproved id refuses with the alternatives, and
        // the approved id publishes.
        let sourced = fixture().await;
        let approved = HostRootId::from_uuid(Uuid::new_v4()).unwrap();
        let tool = CreateAppTool::new(sourced.store.clone(), sourced.profile_dir.clone())
            .with_approved_folders(Arc::new(StaticFolders(vec![(
                approved,
                "Tax documents".into(),
            )])));
        let (_, ctx) = sourced.recorded_call().await;
        let refused = tool
            .execute(&ctx, folder_manifest(Uuid::new_v4()))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(
            refused.content.contains("Tax documents"),
            "{}",
            refused.content
        );
        let (_, ctx) = sourced.recorded_call().await;
        let created = tool
            .execute(&ctx, folder_manifest(*approved.as_uuid()))
            .await
            .unwrap();
        assert!(!created.is_error, "{}", created.content);
    }

    /// The door resolves gateway bindings against the profile's gateway
    /// session: no session refuses with the one fix the user has (sign in),
    /// an unentitled id refuses with what is bindable spelled out, an
    /// undeclared operation names the app it is not declared by, and a
    /// declared pin publishes (docs/decisions/0007).
    #[tokio::test]
    async fn gateway_bindings_resolve_against_the_gateway_session_at_the_door() {
        use crate::local_app::{GatewayAppSource, GatewayAuthoringApp};

        struct StaticGateway(Option<Vec<GatewayAuthoringApp>>);

        #[async_trait]
        impl GatewayAppSource for StaticGateway {
            async fn entitled_apps(&self) -> Option<Vec<GatewayAuthoringApp>> {
                self.0.as_ref().map(|apps| {
                    apps.iter()
                        .map(|app| GatewayAuthoringApp {
                            id: app.id.clone(),
                            name: app.name.clone(),
                            operation_ids: app.operation_ids.clone(),
                        })
                        .collect()
                })
            }
        }

        let gateway_manifest = |gateway_app: &str, operation_ids: &[&str]| {
            json!({
                "bundle_html": "<!doctype html><h1>Incidents</h1>",
                "manifest": {
                    "name": "Incident board",
                    "bindings": [{
                        "gateway_app": gateway_app,
                        "operation_ids": operation_ids,
                    }],
                },
            })
        };
        let entitled = || {
            vec![GatewayAuthoringApp {
                id: "app-incident".into(),
                name: "Incident API".into(),
                operation_ids: vec!["listIncidents".into()],
            }]
        };

        // No source at all, and a source reporting no session, are the same
        // refusal: the fix for both is a gateway session.
        for source in [None, Some(Arc::new(StaticGateway(None)))] {
            let fixture = fixture().await;
            let tool = match source {
                Some(source) => {
                    CreateAppTool::new(fixture.store.clone(), fixture.profile_dir.clone())
                        .with_gateway_apps(source)
                }
                None => CreateAppTool::new(fixture.store.clone(), fixture.profile_dir.clone()),
            };
            let (_, ctx) = fixture.recorded_call().await;
            let refused = tool
                .execute(&ctx, gateway_manifest("app-incident", &["listIncidents"]))
                .await
                .unwrap();
            assert!(refused.is_error);
            assert!(
                refused.content.contains("no gateway session"),
                "{}",
                refused.content
            );
        }

        let fixture = fixture().await;
        let tool = CreateAppTool::new(fixture.store.clone(), fixture.profile_dir.clone())
            .with_gateway_apps(Arc::new(StaticGateway(Some(entitled()))));

        // An id the session does not answer to refuses with the alternatives.
        let (_, ctx) = fixture.recorded_call().await;
        let refused = tool
            .execute(&ctx, gateway_manifest("app-ghost", &["listIncidents"]))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(
            refused.content.contains("Incident API"),
            "{}",
            refused.content
        );

        // An operation the gateway's catalog does not declare refuses naming
        // the app it is not declared by.
        let (_, ctx) = fixture.recorded_call().await;
        let refused = tool
            .execute(&ctx, gateway_manifest("app-incident", &["ghostOp"]))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(
            refused.content.contains("not declared by gateway app"),
            "{}",
            refused.content
        );

        // A declared pin on an entitled app publishes.
        let (_, ctx) = fixture.recorded_call().await;
        let created = tool
            .execute(&ctx, gateway_manifest("app-incident", &["listIncidents"]))
            .await
            .unwrap();
        assert!(!created.is_error, "{}", created.content);
    }

    #[tokio::test]
    async fn the_projection_carries_the_name_ordinal_and_app_id_and_never_the_bundle() {
        let fixture = fixture().await;
        let (_, ctx) = fixture.recorded_call().await;
        let output = fixture
            .tool
            .execute(&ctx, arguments_bound_to(fixture.issues, None))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);

        let preview = ToolResultPreview::build(crate::local_app::CREATE_APP_TOOL, &output)
            .expect("create_app projects an entries card");
        let ToolResultPreview::Entries { entries, .. } = &preview else {
            panic!("create_app projects entries, not {preview:?}");
        };
        let [row] = entries.as_slice() else {
            panic!("one app row per call");
        };
        assert_eq!(row.kind, ResultEntryKind::App);
        assert_eq!(row.label, "Issue triage");
        assert_eq!(row.meta.as_deref(), Some("revision 1"));
        // The app id is the row's navigation target, so the card can open the
        // app it just made.
        assert_eq!(
            row.target_id.as_deref(),
            Some(AppId::for_call(ctx.call_id.unwrap()).to_string().as_str())
        );
        // Renderer-safe: an id and a name, never the bundle or the bindings.
        let json = serde_json::to_string(&preview).unwrap();
        assert!(!json.contains("doctype"));
        assert!(!json.contains("listIssues"));
    }
}
