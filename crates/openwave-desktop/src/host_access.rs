//! Narrow native consent surface for connected host folders.

use std::path::PathBuf;

use openwave_core::{ChatId, Store};
use openwave_host_broker::{
    ConsentMethod, ControlRequest, ExecutionContext, GrantSubject, OperationEnvelope, OperationId,
    OperationRequest, OperationResult, RegisterRootRequest, RevokeRootRequest, RootId,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::{oneshot, Mutex, OnceCell};
use uuid::Uuid;

use crate::broker::BrokerClient;
use crate::client_execution::{ControlPlaneClient, ReceiptStore};

pub(crate) struct HostAccess {
    pub(super) broker: BrokerClient,
    pub(super) picker: Mutex<()>,
    store: OnceCell<std::sync::Arc<dyn Store>>,
    pub(super) control_plane: OnceCell<ControlPlaneClient>,
    pub(super) receipts: ReceiptStore,
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
            store: OnceCell::new(),
            control_plane: OnceCell::new(),
            receipts,
        })
    }

    pub(crate) fn initialize_store(&self, store: std::sync::Arc<dyn Store>) -> Result<(), String> {
        self.store
            .set(store)
            .map_err(|_| "host access store was initialized more than once".to_owned())
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectedFolder {
    root_id: RootId,
    display_name: String,
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

    let result = state
        .broker
        .control(ControlRequest::RegisterRoot(RegisterRootRequest {
            operation_id: OperationId::new(),
            subject: context.subject,
            conversation_id: context.chat_id,
            path,
            consent_method: ConsentMethod::FolderPicker,
        }))
        .await
        .map_err(|error| error.to_string())?;
    let openwave_host_broker::ControlResult::RegisterRoot(result) = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    Ok(Some(ConnectedFolder {
        root_id: result.root.root_id,
        display_name: result.root.display_name,
    }))
}

#[tauri::command]
pub(crate) async fn list_connected_folders(
    state: State<'_, HostAccess>,
    chat_id: Uuid,
) -> Result<Vec<ConnectedFolder>, String> {
    let context = state.context(chat_id).await?;
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
    Ok(roots
        .into_iter()
        .map(|root| ConnectedFolder {
            root_id: root.root_id,
            display_name: root.display_name,
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn disconnect_folder(
    state: State<'_, HostAccess>,
    request: DisconnectFolderRequest,
) -> Result<bool, String> {
    let context = state.context(request.chat_id).await?;
    let result = state
        .broker
        .control(ControlRequest::RevokeRoot(RevokeRootRequest {
            operation_id: OperationId::new(),
            subject: context.subject,
            root_id: request.root_id,
        }))
        .await
        .map_err(|error| error.to_string())?;
    let openwave_host_broker::ControlResult::RevokeRoot(result) = result else {
        return Err("host broker returned an unexpected response".to_owned());
    };
    Ok(result.revoked)
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
}
