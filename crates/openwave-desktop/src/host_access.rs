//! Narrow native consent surface for connected host folders.

use std::path::PathBuf;

use openwave_core::{ChatId, Store};
use openwave_host_broker::{
    Capability, ControlRequest, ControlResult, ExecutionContext, GrantSubject, OperationEnvelope,
    OperationRequest, OperationResult, ResolveExecRootsRequest, RootId, RootSummary,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tokio::sync::{oneshot, Mutex, OnceCell};
use unicode_general_category::{get_general_category, GeneralCategory};
use uuid::Uuid;

use crate::broker::BrokerClient;
use crate::client_execution::{ControlPlaneClient, ReceiptStore};

pub(crate) struct HostAccess {
    pub(super) broker: BrokerClient,
    pub(super) picker: Mutex<()>,
    pub(super) output_exports: Mutex<()>,
    pub(super) output_writebacks: Mutex<()>,
    pub(super) root_changes: Mutex<()>,
    store: OnceCell<std::sync::Arc<dyn Store>>,
    pub(super) control_plane: OnceCell<ControlPlaneClient>,
    pub(super) receipts: ReceiptStore,
    staged_folders: OnceCell<std::sync::Arc<dyn openwave_server::code_execution::StagedFolders>>,
}

impl HostAccess {
    pub(crate) fn new(
        app: AppHandle,
        data_dir: PathBuf,
        home_dir: PathBuf,
    ) -> Result<Self, String> {
        let receipts = ReceiptStore::open(&data_dir)
            .map_err(|_| "could not open private client-execution receipts".to_owned())?;
        Ok(Self {
            broker: BrokerClient::new(app, data_dir, home_dir),
            picker: Mutex::const_new(()),
            output_exports: Mutex::const_new(()),
            output_writebacks: Mutex::const_new(()),
            root_changes: Mutex::const_new(()),
            store: OnceCell::new(),
            control_plane: OnceCell::new(),
            receipts,
            staged_folders: OnceCell::new(),
        })
    }

    /// Install the server's per-turn exec staging registry.
    ///
    /// The folder tools run here, in the desktop process, while the overlay
    /// they have to agree with is owned by the server's execution provider.
    /// Handing the lookup across is what lets a folder listing answer from the
    /// tree exec is writing into without the broker learning about turns.
    pub(crate) fn initialize_staged_folders(
        &self,
        staged: std::sync::Arc<dyn openwave_server::code_execution::StagedFolders>,
    ) -> Result<(), String> {
        self.staged_folders
            .set(staged)
            .map_err(|_| "exec staging lookup was initialized more than once".to_owned())
    }

    pub(super) fn staged_folders(
        &self,
    ) -> Option<&std::sync::Arc<dyn openwave_server::code_execution::StagedFolders>> {
        self.staged_folders.get()
    }

    pub(crate) fn initialize_store(&self, store: std::sync::Arc<dyn Store>) -> Result<(), String> {
        self.store
            .set(store)
            .map_err(|_| "host access store was initialized more than once".to_owned())
    }

    pub(crate) fn store(&self) -> Option<&std::sync::Arc<dyn Store>> {
        self.store.get()
    }

    /// Stable private identity shared by native receipts and server recovery.
    pub(crate) const fn client_executor_id(&self) -> Uuid {
        self.receipts.executor_id()
    }

    pub(crate) fn initialize_control_plane(
        &self,
        base_url: String,
        token: String,
        executor_token: String,
    ) -> Result<(), String> {
        let client = ControlPlaneClient::new(base_url, token, executor_token)
            .map_err(|_| "could not initialize the local control plane".to_owned())?;
        self.control_plane
            .set(client)
            .map_err(|_| "local control plane was initialized more than once".to_owned())
    }

    pub(super) async fn context(&self, chat_id: Uuid) -> Result<AuthoritativeContext, String> {
        if chat_id.is_nil() {
            return Err("invalid conversation id".to_owned());
        }
        let store = self
            .store
            .get()
            .ok_or_else(|| "OpenWave is still starting".to_owned())?;
        let chat = store
            .get_chat(ChatId::from(chat_id))
            .await
            .map_err(|_| "could not load the conversation".to_owned())?
            .ok_or_else(|| "conversation not found".to_owned())?;
        let project_id = chat.project_id.map(|project_id| project_id.0);
        authoritative_context(chat_id, project_id)
    }

    pub(crate) async fn shutdown(&self) {
        self.broker.shutdown().await;
    }
}

/// Native bridge for server-owned exec requests.
///
/// The query is derived from the chat's product attachment projection by the
/// server. This implementation only intersects those opaque IDs with the
/// broker's current grants; model arguments never supply a path or widen the
/// returned set.
pub(crate) struct DesktopExecFolderGrantResolver {
    app: AppHandle,
}

impl DesktopExecFolderGrantResolver {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl openwave_server::code_execution::ExecFolderGrantResolver for DesktopExecFolderGrantResolver {
    async fn resolve(
        &self,
        query: openwave_server::code_execution::ExecFolderGrantQuery,
    ) -> Result<Vec<openwave_server::code_execution::ResolvedExecFolderGrant>, String> {
        let context = match query.project_id {
            Some(project_id) => ExecutionContext::project_chat(query.chat_id.0, project_id.0),
            None => ExecutionContext::standalone(query.chat_id.0),
        }
        .map_err(|_| "invalid conversation context".to_owned())?;
        let root_ids = query
            .root_ids
            .into_iter()
            .map(|root_id| {
                RootId::from_uuid(*root_id.as_uuid())
                    .map_err(|_| "invalid connected folder identity".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = self
            .app
            .state::<HostAccess>()
            .broker
            .control(ControlRequest::ResolveExecRoots(ResolveExecRootsRequest {
                context,
                root_ids,
            }))
            .await
            .map_err(|error| error.to_string())?;
        let ControlResult::ResolveExecRoots { roots } = result else {
            return Err("host broker returned an invalid folder resolution".to_owned());
        };
        roots
            .into_iter()
            .map(|root| {
                let root_id = openwave_core::HostRootId::from_uuid(root.root_id.as_uuid())
                    .map_err(|_| "host broker returned an invalid folder identity".to_owned())?;
                Ok(openwave_server::code_execution::ResolvedExecFolderGrant {
                    root_id,
                    path: root.path,
                    writable: root.writable,
                    // Staging is a property of the turn, decided by the server
                    // once it knows which folders it will stage. The broker
                    // answers about authority and nothing else.
                    overlay: None,
                    staging_unavailable: false,
                })
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
pub(super) struct AuthoritativeContext {
    pub(super) chat_id: Uuid,
    pub(super) execution: ExecutionContext,
    pub(super) subject: GrantSubject,
}

fn authoritative_context(
    chat_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<AuthoritativeContext, String> {
    let execution = match project_id {
        Some(project_id) => ExecutionContext::project_chat(chat_id, project_id),
        None => ExecutionContext::standalone(chat_id),
    }
    .map_err(|_| "invalid conversation context".to_owned())?;
    let subject = match project_id {
        Some(project_id) => GrantSubject::project(project_id),
        None => GrantSubject::conversation(chat_id),
    }
    .map_err(|_| "invalid conversation context".to_owned())?;
    Ok(AuthoritativeContext {
        chat_id,
        execution,
        subject,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ConnectFolderRequest {
    chat_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DisconnectFolderRequest {
    chat_id: Uuid,
    root_id: RootId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ConnectApprovedFolderRequest {
    chat_id: Uuid,
    root_id: RootId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectedFolder {
    pub(crate) root_id: RootId,
    pub(crate) display_name: String,
}

/// What the agent may do inside one connected folder.
///
/// The broker's own capability set also covers subject-wide discovery, which is
/// not a property of a folder and so has no member here.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FolderCapability {
    Read,
    Write,
}

impl FolderCapability {
    fn from_broker(capability: Capability) -> Option<Self> {
        match capability {
            Capability::ReadFiles => Some(Self::Read),
            Capability::WriteFiles => Some(Self::Write),
            _ => None,
        }
    }
}

/// A connected folder together with what this conversation may currently do in
/// it, as the broker reports it rather than as the app assumes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectedFolderAccess {
    pub(crate) root_id: RootId,
    pub(crate) display_name: String,
    pub(crate) capabilities: Vec<FolderCapability>,
}

#[tauri::command]
pub(crate) async fn connect_folder(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: ConnectFolderRequest,
) -> Result<Option<ConnectedFolder>, String> {
    let context = state.context(request.chat_id).await?;
    let _picker = state
        .picker
        .try_lock()
        .map_err(|_| "a folder picker is already open".to_owned())?;
    let path = pick_folder(&app, None).await?;
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err("the folder picker returned an invalid path".to_owned());
    }

    let _root_change = state.root_changes.lock().await;
    crate::client_execution::root_attachment_reconciliation::connect_selected_folder(
        &state, context, path,
    )
    .await
    .map(Some)
}

#[tauri::command]
pub(crate) async fn list_connected_folders(
    state: State<'_, HostAccess>,
    chat_id: Uuid,
) -> Result<Vec<ConnectedFolderAccess>, String> {
    let context = state.context(chat_id).await?;
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(ChatId::from(chat_id))
        .await
        .map_err(|_| "could not load connected folders".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    if chat.root_attachments.is_empty() {
        return Ok(Vec::new());
    }
    let request_id = openwave_host_broker::RequestId::new();
    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: openwave_host_broker::PROTOCOL_VERSION,
            request_id,
            context: context.execution,
            request: OperationRequest::ListRoots,
        })
        .await
        .map_err(|error| error.to_string())?;
    let OperationResult::ListRoots { roots } = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    let product_roots = chat
        .root_attachments
        .iter()
        .map(|attachment| *attachment.root_id.as_uuid())
        .collect::<std::collections::HashSet<_>>();
    Ok(roots
        .into_iter()
        .filter(|root| product_roots.contains(&root.root_id.as_uuid()))
        .map(|root| ConnectedFolderAccess {
            root_id: root.root_id,
            display_name: root.display_name,
            capabilities: root
                .capabilities
                .into_iter()
                .filter_map(FolderCapability::from_broker)
                .collect(),
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn list_approved_folders(
    state: State<'_, HostAccess>,
) -> Result<Vec<ConnectedFolder>, String> {
    approved_folders(&state).await
}

/// The capability half of the unified consent read model.
///
/// Every host-broker grant, mapped into the same statement shape the server
/// serves for standing tool grants, so the Permissions surface renders both
/// stores as one list. Read-only: enforcement stays in the broker's
/// `authorize()`, and these rows are a projection of the records it consults.
#[tauri::command]
pub(crate) async fn list_capability_consents(
    state: State<'_, HostAccess>,
) -> Result<Vec<openwave_server::consent::ConsentStatementSnapshot>, String> {
    let result = state
        .broker
        .control(ControlRequest::ListGrantStatements)
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::ListGrantStatements { grants } = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };

    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let mut statements = Vec::with_capacity(grants.len());
    for grant in grants {
        let Some(statement) = capability_statement(store.as_ref(), grant).await else {
            continue;
        };
        statements.push(statement);
    }
    Ok(statements)
}

/// One broker grant as a consent statement, or `None` for vocabulary this
/// build does not know how to render.
///
/// A subject whose chat or project no longer exists keeps its row with no
/// title: the grant still conveys authority until something revokes it, and
/// the read model exists precisely so such consent stays visible.
async fn capability_statement(
    store: &dyn Store,
    grant: openwave_host_broker::GrantStatementSummary,
) -> Option<openwave_server::consent::ConsentStatementSnapshot> {
    use openwave_host_broker::{ConsentMethod, Scope, SubjectKind};
    use openwave_server::consent::{
        ConsentHandle, ConsentMethodSnapshot, ConsentResource, ConsentStatementSnapshot,
        ConsentVerb, HostCapability,
    };

    let capability = match grant.capability {
        Capability::ListRoots => HostCapability::ListRoots,
        Capability::ReadFiles => HostCapability::ReadFiles,
        Capability::WriteFiles => HostCapability::WriteFiles,
        Capability::ExecuteCommands => HostCapability::ExecuteCommands,
        _ => return None,
    };
    let method = match grant.consent_method {
        ConsentMethod::FolderPicker => ConsentMethodSnapshot::FolderPicker,
        ConsentMethod::PermissionDialog => ConsentMethodSnapshot::PermissionDialog,
        ConsentMethod::OperatorConfig => ConsentMethodSnapshot::OperatorConfig,
        ConsentMethod::CarriedForward => ConsentMethodSnapshot::CarriedForward,
        _ => return None,
    };
    let (level, level_title) = match grant.subject.kind() {
        SubjectKind::Conversation => {
            let chat_id = ChatId::from(grant.subject.id());
            let title = store
                .get_chat(chat_id)
                .await
                .ok()
                .flatten()
                .and_then(|chat| chat.title);
            (openwave_core::GrantLevel::Chat { chat_id }, title)
        }
        SubjectKind::Project => {
            let project_id = openwave_core::ProjectId::from(grant.subject.id());
            let title = store
                .get_project(project_id)
                .await
                .ok()
                .flatten()
                .and_then(|project| project.title);
            (openwave_core::GrantLevel::Project { project_id }, title)
        }
    };
    let resource = match &grant.scope {
        Scope::Subject => ConsentResource::HostSubject,
        Scope::Root { root_id } => ConsentResource::HostRoot {
            root_id: root_id.to_string(),
            display_name: grant.root_display_name.clone(),
        },
        Scope::PathSubtree { root_id, relative } => ConsentResource::HostPathSubtree {
            root_id: root_id.to_string(),
            display_name: grant.root_display_name.clone(),
            relative: relative.as_str().to_owned(),
        },
        _ => return None,
    };
    Some(ConsentStatementSnapshot {
        handle: ConsentHandle::CapabilityGrant {
            grant_id: grant.grant_id.to_string(),
        },
        level,
        level_title,
        verb: ConsentVerb::Capability { capability },
        resource,
        method,
        granted_at: grant.granted_at,
    })
}

/// One statement-level revocation, named exactly as the consent surface
/// listed it: the broker's stable grant identity plus the level the statement
/// reaches, from which the owning subject is rebuilt.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RevokeCapabilityConsentRequest {
    grant_id: openwave_host_broker::GrantId,
    level: openwave_core::GrantLevel,
}

/// Withdraw one host-broker capability grant from the Permissions surface.
///
/// The subject is derived from the statement's level rather than looked up:
/// a grant whose chat or project was deleted still conveys authority, and the
/// consent surface exists precisely so such a statement can be withdrawn. A
/// mismatched level revokes nothing and reports `false`.
#[tauri::command]
pub(crate) async fn revoke_capability_consent(
    state: State<'_, HostAccess>,
    request: RevokeCapabilityConsentRequest,
) -> Result<bool, String> {
    let subject = match request.level {
        openwave_core::GrantLevel::Chat { chat_id } => GrantSubject::conversation(chat_id.0),
        openwave_core::GrantLevel::Project { project_id } => GrantSubject::project(project_id.0),
    }
    .map_err(|_| "invalid consent statement".to_owned())?;
    let result = state
        .broker
        .control(ControlRequest::RevokeGrant(
            openwave_host_broker::RevokeGrantRequest {
                subject,
                grant_id: request.grant_id,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::RevokeGrant(result) = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    Ok(result.revoked)
}

#[tauri::command]
pub(crate) async fn connect_approved_folder(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: ConnectApprovedFolderRequest,
) -> Result<Option<ConnectedFolder>, String> {
    state.context(request.chat_id).await?;
    let chat_label = conversation_label(&state, request.chat_id).await?;
    let root = approved_roots(&state)
        .await?
        .into_iter()
        .find(|root| root.root_id == request.root_id)
        .ok_or_else(|| "the approved folder is no longer available".to_owned())?;
    let _consent = state
        .picker
        .try_lock()
        .map_err(|_| "a folder permission prompt is already open".to_owned())?;
    if !confirm_folder_attachment(&app, &chat_label, &root.display_name).await? {
        return Ok(None);
    }

    // Resolve authority again after the user responds so a deleted or changed
    // conversation cannot reuse the earlier context.
    let _root_change = state.root_changes.lock().await;
    let context = state.context(request.chat_id).await?;
    crate::client_execution::root_attachment_reconciliation::connect_existing_root(
        &state, context, root,
    )
    .await
    .map(Some)
}

#[tauri::command]
pub(crate) async fn disconnect_folder(
    state: State<'_, HostAccess>,
    request: DisconnectFolderRequest,
) -> Result<bool, String> {
    let context = state.context(request.chat_id).await?;
    let _root_change = state.root_changes.lock().await;
    crate::client_execution::root_attachment_reconciliation::disconnect_root(
        &state,
        context,
        request.root_id,
    )
    .await
}

async fn approved_folders(state: &HostAccess) -> Result<Vec<ConnectedFolder>, String> {
    Ok(approved_roots(state)
        .await?
        .into_iter()
        .map(|root| ConnectedFolder {
            root_id: root.root_id,
            display_name: root.display_name,
        })
        .collect())
}

async fn approved_roots(state: &HostAccess) -> Result<Vec<RootSummary>, String> {
    let result = state
        .broker
        .control(ControlRequest::ListApprovedRoots)
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::ListApprovedRoots { roots } = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    Ok(roots)
}

async fn conversation_label(state: &HostAccess, chat_id: Uuid) -> Result<String, String> {
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(ChatId::from(chat_id))
        .await
        .map_err(|_| "could not load the conversation".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    Ok(safe_dialog_label(
        chat.title.as_deref().unwrap_or("Untitled chat"),
    ))
}

fn safe_dialog_label(value: &str) -> String {
    value
        .chars()
        .take(80)
        .map(|character| {
            if matches!(
                get_general_category(character),
                GeneralCategory::Control
                    | GeneralCategory::Format
                    | GeneralCategory::LineSeparator
                    | GeneralCategory::ParagraphSeparator
            ) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

async fn confirm_folder_attachment(
    app: &AppHandle,
    chat_label: &str,
    display_name: &str,
) -> Result<bool, String> {
    let folder_label = safe_dialog_label(display_name);
    let (tx, rx) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(format!(
            "Allow the chat “{chat_label}” to read the previously approved folder “{folder_label}”?"
        ))
        .title("Connect folder")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Connect".to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |approved| {
        let _ = tx.send(approved);
    });
    rx.await
        .map_err(|_| "folder permission prompt closed unexpectedly".to_owned())
}

pub(super) async fn pick_folder(
    app: &AppHandle,
    starting_directory: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title("Choose a folder OpenWave can read");
    if let Some(starting_directory) = starting_directory {
        picker = picker.set_directory(starting_directory);
    }
    if let Some(window) = app.get_webview_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.pick_folder(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "folder picker closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "the folder picker returned an invalid path".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoritative_chat_record_derives_exact_broker_authority() {
        let chat_id = Uuid::new_v4();
        let standalone = authoritative_context(chat_id, None).unwrap();
        assert_eq!(standalone.execution.conversation_id(), chat_id);
        assert_eq!(standalone.execution.project_id(), None);
        assert_eq!(standalone.subject.id(), chat_id);

        let project_id = Uuid::new_v4();
        let project = authoritative_context(chat_id, Some(project_id)).unwrap();
        assert_eq!(project.execution.conversation_id(), chat_id);
        assert_eq!(project.execution.project_id(), Some(project_id));
        assert_eq!(project.subject.id(), project_id);
    }

    #[test]
    fn renderer_request_cannot_supply_project_authority() {
        let chat_id = Uuid::new_v4();
        let valid = serde_json::json!({ "chatId": chat_id });
        let request = serde_json::from_value::<ConnectFolderRequest>(valid).unwrap();
        assert_eq!(request.chat_id, chat_id);

        let injected = serde_json::json!({
            "chatId": chat_id,
            "projectId": Uuid::new_v4(),
        });
        assert!(serde_json::from_value::<ConnectFolderRequest>(injected).is_err());
        assert!(authoritative_context(Uuid::nil(), None).is_err());
    }

    #[test]
    fn native_confirmation_labels_are_bounded_and_strip_controls() {
        let label = safe_dialog_label(&format!("Research\n\u{202e}{}", "x".repeat(100)));
        assert!(!label.contains('\n'));
        assert!(!label.contains('\u{202e}'));
        assert_eq!(label.chars().count(), 80);
        assert!(label.starts_with("Research\u{fffd}\u{fffd}"));
    }
}
