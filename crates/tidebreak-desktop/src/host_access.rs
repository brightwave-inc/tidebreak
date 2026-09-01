//! Narrow native consent surface for connected host folders.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tidebreak_core::{ChatId, Store};
use tidebreak_host_broker::{
    AppFolderPathRequest, AppFolderWriteRequest, Capability, ControlRequest, ControlResult,
    ExecutionContext, GrantSubject, RelativePath, ResolveExecRootsRequest, RootId, RootSummary,
    WriteFileMode,
};
use tokio::sync::{oneshot, Mutex, OnceCell};
use unicode_general_category::{get_general_category, GeneralCategory};
use uuid::Uuid;

use crate::broker::BrokerClient;
use crate::client_execution::{ControlPlaneClient, ReceiptStore};

pub(crate) struct HostAccess {
    pub(super) broker: BrokerClient,
    pub(super) trusted_folders: crate::trusted_folders::TrustedFolderStore,
    pub(super) picker: Mutex<()>,
    pub(super) output_exports: Mutex<()>,
    pub(super) debug_exports: Mutex<()>,
    pub(super) output_writebacks: Mutex<()>,
    pub(super) root_changes: Mutex<()>,
    pub(super) computer_use: crate::client_execution::computer_use::ComputerUseState,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) foreground_browser: crate::client_execution::browser::ForegroundBrowserExecutorState,
    store: OnceCell<std::sync::Arc<dyn Store>>,
    pub(super) control_plane: OnceCell<ControlPlaneClient>,
    pub(super) receipts: ReceiptStore,
    staged_folders: OnceCell<std::sync::Arc<dyn tidebreak_server::code_execution::StagedFolders>>,
    /// The embedded server's update-quiesce handle, installed at server boot.
    /// [`Self::quiesce_for_update`] runs it before the broker drain so a
    /// restart-to-update parks sessions at a safe point first.
    server_quiesce: OnceCell<tidebreak_server::UpdateQuiesce>,
    /// Which machine this client is attached to. Host authority applies to the
    /// local one only, so every native command consults this first.
    remote: std::sync::Arc<crate::remote::RemoteAttachment>,
}

impl HostAccess {
    pub(crate) fn new(
        app: AppHandle,
        data_dir: PathBuf,
        home_dir: PathBuf,
        remote: std::sync::Arc<crate::remote::RemoteAttachment>,
    ) -> Result<Self, String> {
        let receipts = ReceiptStore::open(&data_dir)
            .map_err(|_| "could not open private client-execution receipts".to_owned())?;
        let trusted_folders = crate::trusted_folders::TrustedFolderStore::open(&data_dir)
            .map_err(|_| "could not open trusted folder defaults".to_owned())?;
        Ok(Self {
            broker: BrokerClient::new(app, data_dir, home_dir),
            trusted_folders,
            picker: Mutex::const_new(()),
            output_exports: Mutex::const_new(()),
            debug_exports: Mutex::const_new(()),
            output_writebacks: Mutex::const_new(()),
            root_changes: Mutex::const_new(()),
            computer_use: crate::client_execution::computer_use::ComputerUseState::default(),
            foreground_browser:
                crate::client_execution::browser::ForegroundBrowserExecutorState::default(),
            store: OnceCell::new(),
            control_plane: OnceCell::new(),
            receipts,
            staged_folders: OnceCell::new(),
            server_quiesce: OnceCell::new(),
            remote,
        })
    }

    /// Refuse `authority` unless this client is attached to the local machine.
    ///
    /// The error is the bare stable reason code — see
    /// [`crate::host_authority`] for why, and for the four codes.
    pub(crate) async fn require_local(
        &self,
        authority: crate::host_authority::Authority,
    ) -> Result<(), String> {
        crate::host_authority::require_local_authority(
            self.remote.current().await.as_ref(),
            authority,
        )
    }

    /// Install the server's per-turn exec staging registry.
    ///
    /// The folder tools run here, in the desktop process, while the overlay
    /// they have to agree with is owned by the server's execution provider.
    /// Handing the lookup across is what lets a folder listing answer from the
    /// tree exec is writing into without the broker learning about turns.
    pub(crate) fn initialize_staged_folders(
        &self,
        staged: std::sync::Arc<dyn tidebreak_server::code_execution::StagedFolders>,
    ) -> Result<(), String> {
        self.staged_folders
            .set(staged)
            .map_err(|_| "exec staging lookup was initialized more than once".to_owned())
    }

    pub(super) fn staged_folders(
        &self,
    ) -> Option<&std::sync::Arc<dyn tidebreak_server::code_execution::StagedFolders>> {
        self.staged_folders.get()
    }

    pub(crate) fn initialize_store(&self, store: std::sync::Arc<dyn Store>) -> Result<(), String> {
        self.store
            .set(store)
            .map_err(|_| "host access store was initialized more than once".to_owned())
    }

    /// Install the embedded server's update-quiesce handle at server boot.
    pub(crate) fn initialize_update_quiesce(
        &self,
        quiesce: tidebreak_server::UpdateQuiesce,
    ) -> Result<(), String> {
        self.server_quiesce
            .set(quiesce)
            .map_err(|_| "server update quiesce was initialized more than once".to_owned())
    }

    /// Drop conversation-scoped broker rows whose chats no longer exist.
    ///
    /// Epoch resets wipe broker state with SQLite. Normal chat delete purges
    /// the subject after detach. This catch-up covers interrupted deletes and
    /// older profiles that retained orphan grants across product history.
    pub(crate) async fn reconcile_orphaned_conversation_authority(&self) -> Result<(), String> {
        let Some(store) = self.store() else {
            return Ok(());
        };
        let live: std::collections::HashSet<Uuid> = store
            .list_chats()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|chat| chat.id.0)
            .collect();
        let result = self
            .broker
            .control(ControlRequest::ListGrantStatements)
            .await
            .map_err(|error| error.to_string())?;
        let ControlResult::ListGrantStatements { grants } = result else {
            return Err("host broker returned an unexpected response".to_owned());
        };
        let mut orphan_conversations = std::collections::HashSet::new();
        for grant in grants {
            if grant.subject.kind() != tidebreak_host_broker::SubjectKind::Conversation {
                continue;
            }
            let conversation_id = grant.subject.id();
            if !live.contains(&conversation_id) {
                orphan_conversations.insert(conversation_id);
            }
        }
        // Attachments can linger without a grant statement if only detach was
        // skipped; list unavailable roots' attached conversations too.
        let result = self
            .broker
            .control(ControlRequest::ListUnavailableRoots)
            .await
            .map_err(|error| error.to_string())?;
        if let ControlResult::ListUnavailableRoots { roots } = result {
            for root in roots {
                for conversation_id in root.attached_conversations {
                    if !live.contains(&conversation_id) {
                        orphan_conversations.insert(conversation_id);
                    }
                }
            }
        }
        for conversation_id in orphan_conversations {
            let result = self
                .broker
                .control(ControlRequest::PurgeConversationSubject(
                    tidebreak_host_broker::PurgeConversationSubjectRequest { conversation_id },
                ))
                .await
                .map_err(|error| error.to_string())?;
            let ControlResult::PurgeConversationSubject(_) = result else {
                return Err("host broker returned an unexpected response".to_owned());
            };
        }
        Ok(())
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
            .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
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

    /// Bring the process to a restart-safe point for an update.
    ///
    /// Session work first — code sessions park at a turn boundary, chat turn
    /// leases are handed back — then the broker's admission barrier drains.
    /// The error is a sentence the Updates panel shows as-is; a partial
    /// quiesce is unwound before returning it. Before the server boots there
    /// is no session work, so only the broker drains.
    pub(crate) async fn quiesce_for_update(&self) -> Result<(), String> {
        if let Some(server) = self.server_quiesce.get() {
            server.quiesce_for_update().await?;
        }
        if let Err(error) = self.broker.quiesce_for_update().await {
            eprintln!("tidebreak-desktop: could not quiesce host broker for update: {error}");
            if let Some(server) = self.server_quiesce.get() {
                server.resume_after_failed_update();
            }
            return Err(crate::updater::UPDATE_PREPARE_ERROR.to_owned());
        }
        Ok(())
    }

    pub(crate) async fn resume_after_failed_update(&self) -> Result<(), String> {
        if let Some(server) = self.server_quiesce.get() {
            server.resume_after_failed_update();
        }
        self.broker
            .resume_after_failed_update()
            .await
            .map_err(|error| error.to_string())
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
impl tidebreak_server::code_execution::ExecFolderGrantResolver for DesktopExecFolderGrantResolver {
    async fn resolve(
        &self,
        query: tidebreak_server::code_execution::ExecFolderGrantQuery,
    ) -> Result<Vec<tidebreak_server::code_execution::ResolvedExecFolderGrant>, String> {
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
                let root_id = tidebreak_core::HostRootId::from_uuid(root.root_id.as_uuid())
                    .map_err(|_| "host broker returned an invalid folder identity".to_owned())?;
                Ok(tidebreak_server::code_execution::ResolvedExecFolderGrant {
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

/// The desktop's host-folder surface for local-app folder bindings: the
/// server's optional seam (docs/folder-bindings.md), implemented over the
/// broker sidecar's app-folder control surface.
///
/// The server has already enforced the app grant — pin, coverage, access
/// level, fingerprint — before any call lands here; this bridge carries
/// opaque root ids and folder-relative paths to the broker, which owns the
/// host-level half (live registration, pinned descriptors, byte bounds).
pub(crate) struct DesktopHostFolders {
    app: AppHandle,
}

impl DesktopHostFolders {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn broker(&self) -> State<'_, HostAccess> {
        self.app.state::<HostAccess>()
    }
}

/// The broker's safe error vocabulary folded into the seam's closed one —
/// codes only, never broker message text, so nothing host-shaped can reach
/// an app frame.
fn folder_op_error(
    error: crate::broker::BrokerClientError,
) -> tidebreak_server::host_folders::FolderOpError {
    use tidebreak_server::host_folders::FolderOpError;

    match error {
        crate::broker::BrokerClientError::Broker { code, .. } => match code {
            tidebreak_host_broker::ErrorCode::Denied
            | tidebreak_host_broker::ErrorCode::InvalidRoot => FolderOpError::NotConnected,
            tidebreak_host_broker::ErrorCode::NotFound => FolderOpError::NotFound,
            tidebreak_host_broker::ErrorCode::InvalidRequest => FolderOpError::InvalidPath,
            tidebreak_host_broker::ErrorCode::TooLarge => FolderOpError::TooLarge,
            tidebreak_host_broker::ErrorCode::AlreadyExists => FolderOpError::WrongMode,
            _ => FolderOpError::Failed,
        },
        _ => FolderOpError::Failed,
    }
}

fn folder_relative_path(
    path: &str,
) -> Result<RelativePath, tidebreak_server::host_folders::FolderOpError> {
    RelativePath::parse(path)
        .map_err(|_| tidebreak_server::host_folders::FolderOpError::InvalidPath)
}

fn folder_app_id(
    app: tidebreak_core::id::AppId,
) -> Result<tidebreak_host_broker::AppId, tidebreak_server::host_folders::FolderOpError> {
    tidebreak_host_broker::AppId::from_uuid(*app.as_uuid())
        .map_err(|_| tidebreak_server::host_folders::FolderOpError::Failed)
}

#[async_trait::async_trait]
impl tidebreak_server::host_folders::HostFolders for DesktopHostFolders {
    async fn approved_roots(
        &self,
    ) -> tidebreak_core::Result<Vec<tidebreak_server::host_folders::ApprovedFolder>> {
        let result = self
            .broker()
            .broker
            .control(ControlRequest::ListApprovedRoots)
            .await
            .map_err(|error| tidebreak_core::AgentError::config(error.to_string()))?;
        let ControlResult::ListApprovedRoots { roots } = result else {
            return Err(tidebreak_core::AgentError::config(
                "host broker returned an invalid folder listing",
            ));
        };
        Ok(roots
            .into_iter()
            .filter_map(|root| {
                let root_id = tidebreak_core::HostRootId::from_uuid(root.root_id.as_uuid()).ok()?;
                Some(tidebreak_server::host_folders::ApprovedFolder {
                    root_id,
                    display_name: root.display_name,
                })
            })
            .collect())
    }

    async fn list_folder(
        &self,
        app: tidebreak_core::id::AppId,
        root: tidebreak_core::HostRootId,
        path: &str,
    ) -> Result<
        Vec<tidebreak_server::host_folders::FolderEntry>,
        tidebreak_server::host_folders::FolderOpError,
    > {
        use tidebreak_server::host_folders::FolderOpError;

        let app_id = folder_app_id(app)?;
        let root_id =
            RootId::from_uuid(*root.as_uuid()).map_err(|_| FolderOpError::NotConnected)?;
        let path = folder_relative_path(path)?;
        let result = self
            .broker()
            .broker
            .control(ControlRequest::ListAppFolder(AppFolderPathRequest {
                app_id,
                root_id,
                path,
            }))
            .await
            .map_err(folder_op_error)?;
        let ControlResult::ListAppFolder { entries } = result else {
            return Err(FolderOpError::Failed);
        };
        Ok(entries
            .into_iter()
            .map(|entry| tidebreak_server::host_folders::FolderEntry {
                name: entry.name,
                directory: matches!(entry.kind, tidebreak_host_broker::EntryKind::Directory),
            })
            .collect())
    }

    async fn read_file(
        &self,
        app: tidebreak_core::id::AppId,
        root: tidebreak_core::HostRootId,
        path: &str,
    ) -> Result<Vec<u8>, tidebreak_server::host_folders::FolderOpError> {
        use base64::Engine as _;

        use tidebreak_server::host_folders::FolderOpError;

        let app_id = folder_app_id(app)?;
        let root_id =
            RootId::from_uuid(*root.as_uuid()).map_err(|_| FolderOpError::NotConnected)?;
        let path = folder_relative_path(path)?;
        let result = self
            .broker()
            .broker
            .control(ControlRequest::ReadAppFolderFile(AppFolderPathRequest {
                app_id,
                root_id,
                path,
            }))
            .await
            .map_err(folder_op_error)?;
        let ControlResult::ReadAppFolderFile(result) = result else {
            return Err(FolderOpError::Failed);
        };
        base64::engine::general_purpose::STANDARD
            .decode(result.content_base64)
            .map_err(|_| FolderOpError::Failed)
    }

    async fn write_file(
        &self,
        app: tidebreak_core::id::AppId,
        root: tidebreak_core::HostRootId,
        path: &str,
        content: &[u8],
        replace: bool,
    ) -> Result<
        tidebreak_server::host_folders::FolderWriteReceipt,
        tidebreak_server::host_folders::FolderOpError,
    > {
        use base64::Engine as _;
        use sha2::Digest as _;

        use tidebreak_server::host_folders::FolderOpError;

        let app_id = folder_app_id(app)?;
        let root_id =
            RootId::from_uuid(*root.as_uuid()).map_err(|_| FolderOpError::NotConnected)?;
        let path = folder_relative_path(path)?;
        let result = self
            .broker()
            .broker
            .control(ControlRequest::WriteAppFolderFile(AppFolderWriteRequest {
                app_id,
                root_id,
                path,
                mode: if replace {
                    WriteFileMode::Replace
                } else {
                    WriteFileMode::Create
                },
                content_base64: base64::engine::general_purpose::STANDARD.encode(content),
                bytes: content.len(),
                sha256: sha2::Sha256::digest(content).into(),
            }))
            .await
            .map_err(folder_op_error)?;
        let ControlResult::WriteAppFolderFile { bytes, replaced } = result else {
            return Err(FolderOpError::Failed);
        };
        Ok(tidebreak_server::host_folders::FolderWriteReceipt { bytes, replaced })
    }
}

#[derive(Clone, Copy)]
pub(super) struct AuthoritativeContext {
    pub(super) chat_id: Uuid,
    pub(super) execution: ExecutionContext,
    pub(super) subject: GrantSubject,
}

impl AuthoritativeContext {
    /// Browser workspace identity derived only after the persisted chat has
    /// been loaded. This namespace cannot collide with code `WorkspaceId`
    /// values, and no renderer or model field participates in its value.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn foreground_browser_scope(&self) -> String {
        format!("foreground-chat:{}", self.chat_id)
    }
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SetTrustedFolderRequest {
    root_id: RootId,
    trusted: bool,
}

/// Whether the broker can currently reach a listed folder.
///
/// `Unavailable` is the set-aside state: the approval and attachment stand,
/// but the directory could not be reopened — an unplugged drive, a moved
/// folder. The distinction is the product surface for it; without this a
/// set-aside folder is indistinguishable from one the user detached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FolderStatus {
    Connected,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectedFolder {
    pub(crate) root_id: RootId,
    pub(crate) display_name: String,
    pub(crate) status: FolderStatus,
    pub(crate) available_in_future_chats: bool,
}

#[tauri::command]
pub(crate) async fn connect_folder(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: ConnectFolderRequest,
) -> Result<Option<ConnectedFolder>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
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
    let connected =
        crate::client_execution::root_attachment_reconciliation::connect_selected_folder(
            &state, context, path,
        )
        .await?;
    state
        .trusted_folders
        .set(connected.root_id, true)
        .map_err(trusted_folder_error)?;
    Ok(Some(ConnectedFolder {
        available_in_future_chats: true,
        ..connected
    }))
}

/// The folders attached to one conversation, by their safe identities.
///
/// Identity comes from the trusted management listing of approved roots, not
/// from the agent's `ListRoots` operation, which answers only for folders the
/// conversation may currently read. Sourcing it from there made a folder whose
/// read consent had been revoked vanish from the panel that owns disconnecting
/// it and granting access back — the one state in which the reader most needs
/// both. What the folder *allows* is still not part of this answer: the
/// renderer reads that from the consent statements, so a listed folder can
/// legitimately allow nothing.
#[tauri::command]
pub(crate) async fn list_connected_folders(
    state: State<'_, HostAccess>,
    chat_id: Uuid,
) -> Result<Vec<ConnectedFolder>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    connected_folders(&state, chat_id).await
}

async fn connected_folders(
    state: &HostAccess,
    chat_id: Uuid,
) -> Result<Vec<ConnectedFolder>, String> {
    let store = state
        .store()
        .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
    let chat = store
        .get_chat(ChatId::from(chat_id))
        .await
        .map_err(|_| "could not load connected folders".to_owned())?
        .ok_or_else(|| "conversation not found".to_owned())?;
    if chat.root_attachments.is_empty() {
        return Ok(Vec::new());
    }
    let trusted = state.trusted_folders.list().map_err(trusted_folder_error)?;
    let roots = approved_roots(state).await?;
    let product_roots = chat
        .root_attachments
        .iter()
        .map(|attachment| *attachment.root_id.as_uuid())
        .collect::<std::collections::HashSet<_>>();
    let mut folders = roots
        .into_iter()
        .filter(|root| product_roots.contains(&root.root_id.as_uuid()))
        .map(|root| ConnectedFolder {
            root_id: root.root_id,
            display_name: root.display_name,
            status: FolderStatus::Connected,
            available_in_future_chats: trusted.contains(&root.root_id),
        })
        .collect::<Vec<_>>();
    // A set-aside root — one the broker could not reopen — used to vanish
    // from this listing entirely, leaving an unplugged drive looking exactly
    // like a deliberate detach. It stays visible instead, marked unavailable,
    // so the panel can say what happened and offer to forget it.
    let result = state
        .broker
        .control(ControlRequest::ListUnavailableRoots)
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::ListUnavailableRoots { roots } = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    folders.extend(
        roots
            .into_iter()
            .filter(|root| product_roots.contains(&root.root_id.as_uuid()))
            .map(|root| ConnectedFolder {
                root_id: root.root_id,
                display_name: root.display_name,
                status: FolderStatus::Unavailable,
                available_in_future_chats: trusted.contains(&root.root_id),
            }),
    );
    Ok(folders)
}

#[tauri::command]
pub(crate) async fn list_approved_folders(
    state: State<'_, HostAccess>,
) -> Result<Vec<ConnectedFolder>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
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
) -> Result<Vec<tidebreak_server::consent::ConsentStatementSnapshot>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
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
        .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
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
/// title until purge or revoke removes it. Conversation subjects are purged
/// on chat delete (and should not linger after an epoch wipe); this fallback
/// covers the brief window before cleanup and any project subject still live
/// without a title.
async fn capability_statement(
    store: &dyn Store,
    grant: tidebreak_host_broker::GrantStatementSummary,
) -> Option<tidebreak_server::consent::ConsentStatementSnapshot> {
    use tidebreak_host_broker::{ConsentMethod, Scope, SubjectKind};
    use tidebreak_server::consent::{
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
        ConsentMethod::TrustedFolder => ConsentMethodSnapshot::TrustedFolder,
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
            (tidebreak_core::GrantLevel::Chat { chat_id }, title)
        }
        SubjectKind::Project => {
            let project_id = tidebreak_core::ProjectId::from(grant.subject.id());
            let title = store
                .get_project(project_id)
                .await
                .ok()
                .flatten()
                .and_then(|project| project.title);
            (tidebreak_core::GrantLevel::Project { project_id }, title)
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
    grant_id: tidebreak_host_broker::GrantId,
    level: tidebreak_core::GrantLevel,
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
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let subject = match request.level {
        tidebreak_core::GrantLevel::Chat { chat_id } => GrantSubject::conversation(chat_id.0),
        tidebreak_core::GrantLevel::Project { project_id } => GrantSubject::project(project_id.0),
    }
    .map_err(|_| "invalid consent statement".to_owned())?;
    let result = state
        .broker
        .control(ControlRequest::RevokeGrant(
            tidebreak_host_broker::RevokeGrantRequest {
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
    state: State<'_, HostAccess>,
    request: ConnectApprovedFolderRequest,
) -> Result<Option<ConnectedFolder>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let root = approved_roots(&state)
        .await?
        .into_iter()
        .find(|root| root.root_id == request.root_id)
        .ok_or_else(|| "the approved folder is no longer available".to_owned())?;
    let _root_change = state.root_changes.lock().await;
    let context = state.context(request.chat_id).await?;
    let connected = crate::client_execution::root_attachment_reconciliation::connect_existing_root(
        &state, context, root,
    )
    .await?;
    state
        .trusted_folders
        .set(connected.root_id, true)
        .map_err(trusted_folder_error)?;
    Ok(Some(ConnectedFolder {
        available_in_future_chats: true,
        ..connected
    }))
}

/// Save or clear one folder's automatic attachment default.
#[tauri::command]
pub(crate) async fn set_trusted_folder(
    state: State<'_, HostAccess>,
    request: SetTrustedFolderRequest,
) -> Result<bool, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let _root_change = state.root_changes.lock().await;
    if request.trusted {
        let live = approved_roots(&state)
            .await?
            .iter()
            .any(|root| root.root_id == request.root_id);
        if !live {
            let result = state
                .broker
                .control(ControlRequest::ListUnavailableRoots)
                .await
                .map_err(|error| error.to_string())?;
            let ControlResult::ListUnavailableRoots { roots } = result else {
                return Err("host broker returned an unexpected response".to_owned());
            };
            if !roots.iter().any(|root| root.root_id == request.root_id) {
                return Err("the approved folder is no longer available".to_owned());
            }
        }
    }
    state
        .trusted_folders
        .set(request.root_id, request.trusted)
        .map_err(trusted_folder_error)
}

/// Attach every saved folder to one newly created chat without another host
/// prompt. The saved root IDs are intersected with the broker's current live
/// approvals before any product or broker attachment changes.
#[tauri::command]
pub(crate) async fn attach_trusted_folders(
    state: State<'_, HostAccess>,
    chat_id: Uuid,
) -> Result<Vec<ConnectedFolder>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let trusted = state.trusted_folders.list().map_err(trusted_folder_error)?;
    if trusted.is_empty() {
        return Ok(Vec::new());
    }
    let roots = approved_roots(&state)
        .await?
        .into_iter()
        .filter(|root| trusted.contains(&root.root_id))
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return Ok(Vec::new());
    }

    let _root_change = state.root_changes.lock().await;
    let attached = connected_folders(&state, chat_id)
        .await?
        .into_iter()
        .map(|folder| folder.root_id)
        .collect::<std::collections::HashSet<_>>();
    let mut connected = Vec::new();
    for root in roots {
        if attached.contains(&root.root_id) {
            continue;
        }
        let context = state.context(chat_id).await?;
        let folder =
            crate::client_execution::root_attachment_reconciliation::connect_existing_root(
                &state, context, root,
            )
            .await?;
        connected.push(ConnectedFolder {
            available_in_future_chats: true,
            ..folder
        });
    }
    Ok(connected)
}

/// The capabilities the folders and permissions surfaces may ask to add to an
/// attached folder.
///
/// Read is here because it can be taken away: revoking it leaves the folder
/// attached and allowing nothing, and without a way to ask for it back that is
/// a one-way door. Granting it is a widening like any other — an explicit
/// action answered in the native dialog — never something an attachment does
/// on the user's behalf.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WidenedFolderCapability {
    ReadFiles,
    WriteFiles,
    ExecuteCommands,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GrantFolderCapabilityRequest {
    chat_id: Uuid,
    root_id: RootId,
    capability: WidenedFolderCapability,
}

/// Grant one more capability to a folder this chat already has attached.
///
/// The same shape as attach-time consent: a native dialog names the chat, the
/// folder, and exactly what is being allowed, and the broker records the
/// approval as a fresh permission-dialog grant. The folder identity comes from
/// the broker's own approved-root listing intersected with this chat's
/// attachments, never from renderer strings, and the broker independently
/// re-checks that the root is live and attached before minting anything.
#[tauri::command]
pub(crate) async fn grant_folder_capability(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: GrantFolderCapabilityRequest,
) -> Result<Option<bool>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let chat_label = conversation_label(&state, request.chat_id).await?;
    let root = connected_folders(&state, request.chat_id)
        .await?
        .into_iter()
        .find(|root| {
            root.root_id == request.root_id && matches!(root.status, FolderStatus::Connected)
        })
        .ok_or_else(|| "the folder is no longer connected".to_owned())?;

    let _consent = state
        .picker
        .try_lock()
        .map_err(|_| "a folder permission prompt is already open".to_owned())?;
    if !confirm_folder_widening(&app, &chat_label, &root.display_name, request.capability).await? {
        return Ok(None);
    }

    // Resolve authority again after the user responds so a deleted or changed
    // conversation cannot reuse the earlier context.
    let _root_change = state.root_changes.lock().await;
    let context = state.context(request.chat_id).await?;
    let capability = match request.capability {
        WidenedFolderCapability::ReadFiles => Capability::ReadFiles,
        WidenedFolderCapability::WriteFiles => Capability::WriteFiles,
        WidenedFolderCapability::ExecuteCommands => Capability::ExecuteCommands,
    };
    let result = state
        .broker
        .control(ControlRequest::GrantRootCapability(
            tidebreak_host_broker::GrantRootCapabilityRequest {
                subject: context.subject,
                conversation_id: context.chat_id,
                root_id: request.root_id,
                capability,
                consent_method: tidebreak_host_broker::ConsentMethod::PermissionDialog,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::GrantRootCapability(result) = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    Ok(Some(result.granted))
}

#[tauri::command]
pub(crate) async fn disconnect_folder(
    state: State<'_, HostAccess>,
    request: DisconnectFolderRequest,
) -> Result<bool, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let context = state.context(request.chat_id).await?;
    let _root_change = state.root_changes.lock().await;
    crate::client_execution::root_attachment_reconciliation::disconnect_root(
        &state,
        context,
        request.root_id,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ForgetFolderRequest {
    root_id: RootId,
}

/// Withdraw one folder's approval, grants, and attachments everywhere.
#[tauri::command]
pub(crate) async fn forget_folder(
    state: State<'_, HostAccess>,
    request: ForgetFolderRequest,
) -> Result<bool, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let _root_change = state.root_changes.lock().await;
    let live = approved_roots(&state)
        .await?
        .into_iter()
        .find(|root| root.root_id == request.root_id)
        .and_then(|root| root.owner);
    let result = state
        .broker
        .control(ControlRequest::ListUnavailableRoots)
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::ListUnavailableRoots { roots } = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    let unavailable = roots
        .into_iter()
        .find(|root| root.root_id == request.root_id)
        .map(|root| root.owner);
    let owner = live
        .or(unavailable)
        .ok_or_else(|| "the approved folder is no longer available".to_owned())?;
    disconnect_root_everywhere(&state, request.root_id).await?;
    let result = state
        .broker
        .control(ControlRequest::RevokeRoot(
            tidebreak_host_broker::RevokeRootRequest {
                operation_id: tidebreak_host_broker::OperationId::new(),
                subject: owner,
                root_id: request.root_id,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::RevokeRoot(result) = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    state
        .trusted_folders
        .set(request.root_id, false)
        .map_err(trusted_folder_error)?;
    Ok(result.revoked)
}

async fn disconnect_root_everywhere(state: &HostAccess, root_id: RootId) -> Result<(), String> {
    let store = state
        .store()
        .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
    let chat_ids = store
        .list_chats()
        .await
        .map_err(|_| "could not load chats using this folder".to_owned())?
        .into_iter()
        .filter(|chat| {
            chat.root_attachments
                .iter()
                .any(|attachment| *attachment.root_id.as_uuid() == root_id.as_uuid())
        })
        .map(|chat| chat.id.0)
        .collect::<Vec<_>>();
    for chat_id in chat_ids {
        let context = state.context(chat_id).await?;
        crate::client_execution::root_attachment_reconciliation::disconnect_root(
            state, context, root_id,
        )
        .await?;
    }
    Ok(())
}

/// Forget host-broker authority held by a conversation that no longer exists.
///
/// Chat ids are never reused. After the product deletes a chat, grants and
/// attachments for that subject are leftover authority with no product surface
/// left to exercise them. Detach still happens first while the chat exists;
/// this is the terminal cleanup for any residual conversation-scoped rows.
#[tauri::command]
pub(crate) async fn purge_deleted_conversation_subject(
    state: State<'_, HostAccess>,
    request: PurgeDeletedConversationSubjectRequest,
) -> Result<bool, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    if request.chat_id.is_nil() {
        return Err("invalid conversation identity".to_owned());
    }
    // Refuse while the chat still exists: purge is for deleted subjects, not a
    // shortcut around disconnect.
    if let Some(store) = state.store() {
        if store
            .get_chat(ChatId::from(request.chat_id))
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err(
                "disconnect folders and delete the chat before purging its host authority"
                    .to_owned(),
            );
        }
    }
    let result = state
        .broker
        .control(ControlRequest::PurgeConversationSubject(
            tidebreak_host_broker::PurgeConversationSubjectRequest {
                conversation_id: request.chat_id,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::PurgeConversationSubject(result) = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    Ok(result.changed)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PurgeDeletedConversationSubjectRequest {
    chat_id: Uuid,
}

async fn approved_folders(state: &HostAccess) -> Result<Vec<ConnectedFolder>, String> {
    let trusted = state.trusted_folders.list().map_err(trusted_folder_error)?;
    Ok(approved_roots(state)
        .await?
        .into_iter()
        .map(|root| ConnectedFolder {
            root_id: root.root_id,
            display_name: root.display_name,
            status: FolderStatus::Connected,
            available_in_future_chats: trusted.contains(&root.root_id),
        })
        .collect())
}

fn trusted_folder_error(_error: std::io::Error) -> String {
    "could not update trusted folder defaults".to_owned()
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
        .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
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

async fn confirm_folder_widening(
    app: &AppHandle,
    chat_label: &str,
    display_name: &str,
    capability: WidenedFolderCapability,
) -> Result<bool, String> {
    let folder_label = safe_dialog_label(display_name);
    let allowed = match capability {
        WidenedFolderCapability::ReadFiles => "read files in",
        WidenedFolderCapability::WriteFiles => "write files in",
        WidenedFolderCapability::ExecuteCommands => "run commands in",
    };
    let (tx, rx) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(format!(
            "Allow the chat “{chat_label}” to {allowed} the connected folder “{folder_label}”?"
        ))
        .title("Grant folder access")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Allow".to_owned(),
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

/// Grant-free directory picker for code-mode repo registration and clone
/// destinations. The path is returned as a string; the server validates it.
#[tauri::command]
pub(crate) async fn pick_code_directory(
    app: AppHandle,
    state: State<'_, HostAccess>,
) -> Result<Option<String>, String> {
    state
        .require_local(crate::host_authority::Authority::FolderBroker)
        .await?;
    let _picker = state
        .picker
        .try_lock()
        .map_err(|_| "a folder picker is already open".to_owned())?;
    let path = pick_folder_titled(&app, None, "Choose a folder").await?;
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err("the folder picker returned an invalid path".to_owned());
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

pub(super) async fn pick_folder(
    app: &AppHandle,
    starting_directory: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    pick_folder_titled(
        app,
        starting_directory,
        "Choose a folder Tidebreak can read",
    )
    .await
}

async fn pick_folder_titled(
    app: &AppHandle,
    starting_directory: Option<PathBuf>,
    title: &str,
) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = oneshot::channel();
    let mut picker = app.dialog().file().set_title(title);
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
        assert_eq!(
            standalone.foreground_browser_scope(),
            format!("foreground-chat:{chat_id}")
        );
        assert_ne!(
            standalone.foreground_browser_scope(),
            tidebreak_core::WorkspaceId::new().to_string()
        );

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
