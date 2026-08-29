//! App-owned staging and publication for in-app browser downloads.
//!
//! A page receives no filesystem path. The webview writes to a random file in
//! this module's private directory, and the host publishes completed, bounded
//! bytes as a conversation output. Durable receipts recover a completed
//! download after a desktop restart and discard an interrupted one.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions as StdOpenOptions, TryLockError},
    io::{self, Cursor, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
#[cfg(unix)]
use cap_std::fs::PermissionsExt as _;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tauri::{AppHandle, Manager as _};
use tidebreak_core::{
    accept_workspace_artifact, binary_media_type_for_extension, deliverable_media_type,
    validate_binary_deliverable, validate_portable_filename, AgentError, BrowserControllerKind,
    ChatId, OutputId, OutputRevisionId, RevisionProducer, WorkspaceArtifactProposal,
    MAX_BINARY_DELIVERABLE_BYTES, MAX_DELIVERABLE_NAME_CHARS,
};
use url::Url;
use uuid::Uuid;

use crate::{browser_control::BrowserRegistry, host_access::HostAccess};

const DOWNLOAD_DIRECTORY: &str = "browser-downloads";
const DOWNLOAD_LOCK_FILE: &str = "downloads.lock";
const RECEIPT_PREFIX: &str = "receipt-";
const STAGE_PREFIX: &str = "stage-";
const RECEIPT_VERSION: u32 = 1;
const MAX_RECEIPT_BYTES: usize = 8 * 1024;
const MAX_DOWNLOAD_RECEIPTS: usize = 32;
const RECOVERY_INTERVAL: Duration = Duration::from_secs(2);
const FOREGROUND_SCOPE_PREFIX: &str = "foreground-chat:";

#[derive(Clone)]
pub(crate) struct BrowserDownloadStore {
    inner: Arc<BrowserDownloadStoreInner>,
}

struct BrowserDownloadStoreInner {
    root: PathBuf,
    launch_id: Uuid,
    io: Mutex<()>,
    publication: tokio::sync::Mutex<()>,
    _lock: File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BrowserDownloadPhase {
    Requested,
    Downloaded,
    Ready,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct BrowserDownloadReceipt {
    version: u32,
    operation_id: Uuid,
    launch_id: Uuid,
    chat_id: ChatId,
    browser_id: String,
    request_url_sha256: [u8; 32],
    requested_filename: String,
    final_filename: Option<String>,
    media_type: String,
    output_id: OutputId,
    revision_id: OutputRevisionId,
    phase: BrowserDownloadPhase,
    created_at: DateTime<Utc>,
}

impl std::fmt::Debug for BrowserDownloadReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserDownloadReceipt")
            .field("version", &self.version)
            .field("operation_id", &self.operation_id)
            .field("launch_id", &self.launch_id)
            .field("chat_id", &self.chat_id)
            .field("browser_id", &self.browser_id)
            .field("request_url_sha256", &"[redacted]")
            .field("requested_filename", &self.requested_filename)
            .field("final_filename", &self.final_filename)
            .field("media_type", &self.media_type)
            .field("output_id", &self.output_id)
            .field("revision_id", &self.revision_id)
            .field("phase", &self.phase)
            .field("created_at", &self.created_at)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct BrowserDownloadStarted {
    pub(crate) destination: PathBuf,
    pub(crate) filename: String,
}

pub(crate) enum BrowserDownloadFinished {
    Publish(BrowserDownloadReceipt),
    Rejected { filename: String, message: String },
    Ignored,
}

enum PublishOutcome {
    Published(String),
    Rejected(String),
    Deferred(String),
    Gone,
}

impl BrowserDownloadReceipt {
    fn new(
        launch_id: Uuid,
        chat_id: ChatId,
        browser_id: &str,
        url: &Url,
        filename: String,
        media_type: String,
    ) -> Self {
        Self {
            version: RECEIPT_VERSION,
            operation_id: Uuid::new_v4(),
            launch_id,
            chat_id,
            browser_id: browser_id.to_owned(),
            request_url_sha256: Sha256::digest(url.as_str().as_bytes()).into(),
            requested_filename: filename,
            final_filename: None,
            media_type,
            output_id: OutputId::new(),
            revision_id: OutputRevisionId::new(),
            phase: BrowserDownloadPhase::Requested,
            created_at: Utc::now(),
        }
    }

    fn validate(&self) -> io::Result<()> {
        let valid_browser_id = !self.browser_id.is_empty()
            && self.browser_id.chars().count() <= 80
            && self.browser_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            });
        if self.version != RECEIPT_VERSION
            || self.operation_id.is_nil()
            || self.launch_id.is_nil()
            || self.chat_id.0.is_nil()
            || !valid_browser_id
            || validate_binary_deliverable(&self.requested_filename, &self.media_type).is_err()
            || self.final_filename.as_deref().is_some_and(|filename| {
                validate_binary_deliverable(filename, &self.media_type).is_err()
            })
            || matches!(self.phase, BrowserDownloadPhase::Ready) != self.final_filename.is_some()
            || matches!(self.phase, BrowserDownloadPhase::Requested)
                && self.final_filename.is_some()
        {
            return Err(invalid_data("invalid browser-download receipt"));
        }
        Ok(())
    }
}

impl BrowserDownloadStore {
    pub(crate) fn open(data_dir: &Path) -> Result<Self, String> {
        let root = data_dir.join(DOWNLOAD_DIRECTORY);
        ensure_private_directory(&root)
            .map_err(|_| "could not open private browser download storage".to_owned())?;
        let root = fs::canonicalize(root)
            .map_err(|_| "could not open private browser download storage".to_owned())?;
        let mut options = StdOpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let lock = options
            .open(root.join(DOWNLOAD_LOCK_FILE))
            .map_err(|_| "could not lock private browser download storage".to_owned())?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err("another desktop process owns browser download storage".to_owned());
            }
            Err(TryLockError::Error(_)) => {
                return Err("could not lock private browser download storage".to_owned());
            }
        }
        let store = Self {
            inner: Arc::new(BrowserDownloadStoreInner {
                root,
                launch_id: Uuid::new_v4(),
                io: Mutex::new(()),
                publication: tokio::sync::Mutex::new(()),
                _lock: lock,
            }),
        };
        store
            .load_receipts()
            .map_err(|_| "could not recover private browser downloads".to_owned())?;
        Ok(store)
    }

    pub(crate) fn begin(
        &self,
        registry: &BrowserRegistry,
        browser_id: &str,
        workspace_id: &str,
        url: &Url,
        suggested_destination: &Path,
    ) -> Result<BrowserDownloadStarted, String> {
        let chat_id = authorized_chat_download(registry, browser_id, workspace_id, url)?;
        let filename = suggested_download_filename(suggested_destination)?;
        let media_type = download_media_type(&filename)?;
        let receipt = BrowserDownloadReceipt::new(
            self.inner.launch_id,
            chat_id,
            browser_id,
            url,
            filename.clone(),
            media_type,
        );

        let _guard = lock(&self.inner.io);
        let receipts = load_receipts_locked(&self.inner.root).map_err(private_storage_error)?;
        if receipts.len() >= MAX_DOWNLOAD_RECEIPTS {
            return Err("Too many browser downloads are still pending".to_owned());
        }
        if receipts.iter().any(|existing| {
            existing.launch_id == self.inner.launch_id
                && existing.chat_id == chat_id
                && existing.browser_id == browser_id
                && existing.phase == BrowserDownloadPhase::Requested
                && existing.request_url_sha256 == receipt.request_url_sha256
        }) {
            return Err("This download is already in progress".to_owned());
        }
        let destination = stage_path(&self.inner.root, receipt.operation_id);
        if fs::symlink_metadata(&destination).is_ok() {
            return Err("Could not allocate private browser download storage".to_owned());
        }
        save_receipt_locked(&self.inner.root, &receipt).map_err(private_storage_error)?;
        Ok(BrowserDownloadStarted {
            destination,
            filename,
        })
    }

    pub(crate) fn finish(
        &self,
        browser_id: &str,
        url: &Url,
        reported_path: Option<&Path>,
        success: bool,
    ) -> Result<BrowserDownloadFinished, String> {
        let _guard = lock(&self.inner.io);
        let digest: [u8; 32] = Sha256::digest(url.as_str().as_bytes()).into();
        let mut receipts = load_receipts_locked(&self.inner.root).map_err(private_storage_error)?;
        receipts.sort_by_key(|receipt| (receipt.created_at, receipt.operation_id));
        let Some(mut receipt) = receipts.into_iter().find(|receipt| {
            receipt.launch_id == self.inner.launch_id
                && receipt.browser_id == browser_id
                && receipt.phase == BrowserDownloadPhase::Requested
                && receipt.request_url_sha256 == digest
        }) else {
            return Ok(BrowserDownloadFinished::Ignored);
        };
        let expected = stage_path(&self.inner.root, receipt.operation_id);
        if reported_path.is_some_and(|path| path != expected) {
            remove_stage_locked(&expected).map_err(private_storage_error)?;
            remove_receipt_locked(&self.inner.root, receipt.operation_id)
                .map_err(private_storage_error)?;
            return Ok(BrowserDownloadFinished::Rejected {
                filename: receipt.requested_filename,
                message: "The browser reported an unexpected download destination".to_owned(),
            });
        }
        if !success {
            remove_stage_locked(&expected).map_err(private_storage_error)?;
            remove_receipt_locked(&self.inner.root, receipt.operation_id)
                .map_err(private_storage_error)?;
            return Ok(BrowserDownloadFinished::Rejected {
                filename: receipt.requested_filename,
                message: "The download was cancelled".to_owned(),
            });
        }
        receipt.phase = BrowserDownloadPhase::Downloaded;
        save_receipt_locked(&self.inner.root, &receipt).map_err(private_storage_error)?;
        Ok(BrowserDownloadFinished::Publish(receipt))
    }

    pub(crate) fn cancel_browser(&self, browser_id: &str) -> Result<(), String> {
        let _guard = lock(&self.inner.io);
        for receipt in load_receipts_locked(&self.inner.root).map_err(private_storage_error)? {
            if receipt.launch_id == self.inner.launch_id
                && receipt.browser_id == browser_id
                && receipt.phase == BrowserDownloadPhase::Requested
            {
                remove_stage_locked(&stage_path(&self.inner.root, receipt.operation_id))
                    .map_err(private_storage_error)?;
                remove_receipt_locked(&self.inner.root, receipt.operation_id)
                    .map_err(private_storage_error)?;
            }
        }
        Ok(())
    }

    fn load_receipts(&self) -> io::Result<Vec<BrowserDownloadReceipt>> {
        let _guard = lock(&self.inner.io);
        load_receipts_locked(&self.inner.root)
    }

    fn recovery_batch(&self) -> io::Result<Vec<BrowserDownloadReceipt>> {
        let _guard = lock(&self.inner.io);
        let receipts = load_receipts_locked(&self.inner.root)?;
        let mut retained_stages = HashSet::new();
        let mut recoverable = Vec::new();
        for receipt in receipts {
            if receipt.phase == BrowserDownloadPhase::Requested
                && receipt.launch_id != self.inner.launch_id
            {
                remove_stage_locked(&stage_path(&self.inner.root, receipt.operation_id))?;
                remove_receipt_locked(&self.inner.root, receipt.operation_id)?;
                continue;
            }
            retained_stages.insert(receipt.operation_id);
            if receipt.phase != BrowserDownloadPhase::Requested {
                recoverable.push(receipt);
            }
        }
        cleanup_orphan_stages_locked(&self.inner.root, &retained_stages)?;
        Ok(recoverable)
    }

    fn load_receipt(&self, operation_id: Uuid) -> io::Result<Option<BrowserDownloadReceipt>> {
        let _guard = lock(&self.inner.io);
        load_receipt_locked(&self.inner.root, operation_id)
    }

    fn save_receipt(&self, receipt: &BrowserDownloadReceipt) -> io::Result<()> {
        let _guard = lock(&self.inner.io);
        save_receipt_locked(&self.inner.root, receipt)
    }

    fn reject(&self, receipt: &BrowserDownloadReceipt) -> io::Result<()> {
        let _guard = lock(&self.inner.io);
        remove_stage_locked(&stage_path(&self.inner.root, receipt.operation_id))?;
        remove_receipt_locked(&self.inner.root, receipt.operation_id)
    }

    fn finalize(&self, receipt: &BrowserDownloadReceipt) -> io::Result<()> {
        self.reject(receipt)
    }

    fn read_staged(&self, receipt: &BrowserDownloadReceipt) -> Result<Vec<u8>, String> {
        let _guard = lock(&self.inner.io);
        let directory = open_private_directory(&self.inner.root)
            .map_err(|_| "The staged download is unavailable".to_owned())?;
        let name = stage_name(receipt.operation_id);
        let metadata = directory
            .symlink_metadata(&name)
            .map_err(|_| "The staged download is unavailable".to_owned())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("The staged download is not a regular file".to_owned());
        }
        if metadata.len() == 0 {
            return Err("The downloaded file is empty".to_owned());
        }
        if metadata.len() > MAX_BINARY_DELIVERABLE_BYTES as u64 {
            return Err(format!(
                "The downloaded file is larger than {} MiB",
                MAX_BINARY_DELIVERABLE_BYTES / (1024 * 1024)
            ));
        }
        #[cfg(unix)]
        directory
            .set_permissions(&name, cap_std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "The staged download could not be secured".to_owned())?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = directory
            .open_with(&name, &options)
            .map_err(|_| "The staged download is unavailable".to_owned())?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take((MAX_BINARY_DELIVERABLE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "The staged download could not be read".to_owned())?;
        if bytes.len() != metadata.len() as usize || bytes.len() > MAX_BINARY_DELIVERABLE_BYTES {
            return Err("The staged download changed while it was being read".to_owned());
        }
        Ok(bytes)
    }
}

pub(crate) async fn recover_browser_downloads(app: AppHandle) {
    loop {
        let downloads = app.state::<BrowserDownloadStore>().inner().clone();
        let receipts = match downloads.recovery_batch() {
            Ok(receipts) => receipts,
            Err(error) => {
                eprintln!("tidebreak-desktop: browser download recovery failed: {error}");
                tokio::time::sleep(RECOVERY_INTERVAL).await;
                continue;
            }
        };
        for receipt in receipts {
            publish_and_report(&app, &downloads, receipt).await;
        }
        tokio::time::sleep(RECOVERY_INTERVAL).await;
    }
}

pub(crate) fn publish_completed_download(app: AppHandle, receipt: BrowserDownloadReceipt) {
    let downloads = app.state::<BrowserDownloadStore>().inner().clone();
    tauri::async_runtime::spawn(async move {
        publish_and_report(&app, &downloads, receipt).await;
    });
}

async fn publish_and_report(
    app: &AppHandle,
    downloads: &BrowserDownloadStore,
    receipt: BrowserDownloadReceipt,
) {
    let browser_id = receipt.browser_id.clone();
    let workspace_id = format!("{FOREGROUND_SCOPE_PREFIX}{}", receipt.chat_id);
    match publish_browser_download(app, downloads, receipt).await {
        PublishOutcome::Published(filename) => crate::code_browser::emit_download_event(
            app,
            &workspace_id,
            &browser_id,
            "download_finished",
            None,
            format!("Saved {filename} to Outputs"),
        ),
        PublishOutcome::Rejected(message) => crate::code_browser::emit_download_event(
            app,
            &workspace_id,
            &browser_id,
            "download_failed",
            None,
            message,
        ),
        PublishOutcome::Deferred(error) => {
            eprintln!("tidebreak-desktop: browser download publication deferred: {error}");
        }
        PublishOutcome::Gone => {}
    }
}

async fn publish_browser_download(
    app: &AppHandle,
    downloads: &BrowserDownloadStore,
    receipt: BrowserDownloadReceipt,
) -> PublishOutcome {
    let _publication = downloads.inner.publication.lock().await;
    let mut receipt = match downloads.load_receipt(receipt.operation_id) {
        Ok(Some(receipt)) if receipt.phase != BrowserDownloadPhase::Requested => receipt,
        Ok(_) => return PublishOutcome::Gone,
        Err(error) => return PublishOutcome::Deferred(error.to_string()),
    };
    let host = app.state::<HostAccess>();
    let Some(store) = host.store().cloned() else {
        return PublishOutcome::Deferred("Tidebreak is still starting".to_owned());
    };
    let chat_exists = match store.get_chat(receipt.chat_id).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => return PublishOutcome::Deferred(error.to_string()),
    };
    if !chat_exists {
        let _ = downloads.reject(&receipt);
        return PublishOutcome::Rejected(
            "The conversation for this download no longer exists".to_owned(),
        );
    }

    let existing_output = match store.get_output(receipt.output_id).await {
        Ok(output) => output,
        Err(error) => return PublishOutcome::Deferred(error.to_string()),
    };
    if let Some(output) = existing_output {
        let revision = match store.get_output_revision(receipt.revision_id).await {
            Ok(revision) => revision,
            Err(error) => return PublishOutcome::Deferred(error.to_string()),
        };
        let recorded = revision.is_some_and(|revision| revision.output_id == output.id);
        let expected_filename = receipt.final_filename.as_deref();
        if recorded
            && output.chat_id == receipt.chat_id
            && output.deleted_at.is_none()
            && expected_filename == Some(output.filename.as_str())
            && output.media_type == receipt.media_type
        {
            let filename = output.filename;
            if let Err(error) = downloads.finalize(&receipt) {
                return PublishOutcome::Deferred(error.to_string());
            }
            return PublishOutcome::Published(filename);
        }
        let _ = downloads.reject(&receipt);
        return PublishOutcome::Rejected(
            "The download output identity is no longer valid".to_owned(),
        );
    }

    let bytes = match downloads.read_staged(&receipt) {
        Ok(bytes) => bytes,
        Err(message) => {
            let _ = downloads.reject(&receipt);
            return PublishOutcome::Rejected(message);
        }
    };
    if let Err(message) =
        validate_download_content(&receipt.requested_filename, &receipt.media_type, &bytes)
    {
        let _ = downloads.reject(&receipt);
        return PublishOutcome::Rejected(message);
    }

    if receipt.phase == BrowserDownloadPhase::Downloaded {
        let filename = match available_download_filename(
            store.as_ref(),
            receipt.chat_id,
            &receipt.requested_filename,
        )
        .await
        {
            Ok(filename) => filename,
            Err(error) => return PublishOutcome::Deferred(error),
        };
        receipt.final_filename = Some(filename);
        receipt.phase = BrowserDownloadPhase::Ready;
        if let Err(error) = downloads.save_receipt(&receipt) {
            return PublishOutcome::Deferred(error.to_string());
        }
    }

    let scratch_root = match crate::data_dir(app) {
        Ok(data_dir) => data_dir.join("scratch"),
        Err(error) => return PublishOutcome::Deferred(error),
    };
    let chat_id = receipt.chat_id;
    let scratch = match tauri::async_runtime::spawn_blocking(move || {
        tidebreak_server::output_files::open_or_create_chat_scratch(&scratch_root, chat_id)
    })
    .await
    {
        Ok(Ok(scratch)) => scratch,
        Ok(Err(error)) => return PublishOutcome::Deferred(error),
        Err(_) => {
            return PublishOutcome::Deferred("private output storage task stopped".to_owned())
        }
    };

    loop {
        let filename = receipt
            .final_filename
            .clone()
            .expect("ready receipts carry a filename");
        let proposal = WorkspaceArtifactProposal {
            output_id: receipt.output_id,
            chat_id: receipt.chat_id,
            filename: filename.clone(),
            media_type: receipt.media_type.clone(),
            revision_id: receipt.revision_id,
            producer: RevisionProducer::User,
            revise: false,
            content: bytes.clone(),
            created_at: receipt.created_at,
        };
        match accept_workspace_artifact(store.as_ref(), &scratch, &proposal).await {
            Ok(output) => {
                if let Err(error) = downloads.finalize(&receipt) {
                    return PublishOutcome::Deferred(error.to_string());
                }
                return PublishOutcome::Published(output.filename);
            }
            Err(AgentError::OutputFilenameTaken { .. }) => {
                let next = match available_download_filename(
                    store.as_ref(),
                    receipt.chat_id,
                    &receipt.requested_filename,
                )
                .await
                {
                    Ok(filename) => filename,
                    Err(error) => return PublishOutcome::Deferred(error),
                };
                if next == filename {
                    return PublishOutcome::Deferred(
                        "another output claimed the download filename".to_owned(),
                    );
                }
                receipt.final_filename = Some(next);
                if let Err(error) = downloads.save_receipt(&receipt) {
                    return PublishOutcome::Deferred(error.to_string());
                }
            }
            Err(error) => return PublishOutcome::Deferred(error.to_string()),
        }
    }
}

async fn available_download_filename(
    store: &dyn tidebreak_core::Store,
    chat_id: ChatId,
    requested: &str,
) -> Result<String, String> {
    for copy in 1..=999 {
        let candidate = if copy == 1 {
            requested.to_owned()
        } else {
            filename_with_copy_suffix(requested, copy)?
        };
        if store
            .find_outputs_by_filename(chat_id, &candidate)
            .await
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Ok(candidate);
        }
    }
    Err("This conversation has too many downloads with the same filename".to_owned())
}

fn filename_with_copy_suffix(filename: &str, copy: u16) -> Result<String, String> {
    let (stem, extension) = filename
        .rsplit_once('.')
        .map_or((filename, None), |(stem, extension)| {
            (stem, Some(extension))
        });
    let suffix = format!(" ({copy})");
    let extension_len = extension.map_or(0, |extension| extension.len() + 1);
    let max_stem = MAX_DELIVERABLE_NAME_CHARS
        .checked_sub(suffix.len() + extension_len)
        .ok_or_else(|| "The download filename is too long".to_owned())?;
    let stem = &stem[..stem.len().min(max_stem)];
    let candidate = match extension {
        Some(extension) => format!("{stem}{suffix}.{extension}"),
        None => format!("{stem}{suffix}"),
    };
    validate_portable_filename(&candidate).map_err(str::to_owned)?;
    Ok(candidate)
}

fn authorized_chat_download(
    registry: &BrowserRegistry,
    browser_id: &str,
    workspace_id: &str,
    url: &Url,
) -> Result<ChatId, String> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Only HTTP and HTTPS downloads are supported".to_owned());
    }
    let chat_id = workspace_id
        .strip_prefix(FOREGROUND_SCOPE_PREFIX)
        .ok_or_else(|| "Downloads can only be saved from a conversation browser".to_owned())?
        .parse::<ChatId>()
        .map_err(|_| "The browser conversation is not valid".to_owned())?;
    if workspace_id != format!("{FOREGROUND_SCOPE_PREFIX}{chat_id}") {
        return Err("The browser conversation is not valid".to_owned());
    }
    let snapshot = registry.snapshot(browser_id, workspace_id)?;
    if snapshot.visible != Some(true) {
        return Err("Show the browser before downloading a file".to_owned());
    }
    if snapshot
        .controller
        .as_ref()
        .map(|controller| controller.kind)
        != Some(BrowserControllerKind::Human)
    {
        return Err("Take control of the browser before downloading a file".to_owned());
    }
    Ok(chat_id)
}

fn suggested_download_filename(destination: &Path) -> Result<String, String> {
    let filename = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The site did not provide a safe download filename".to_owned())?;
    validate_portable_filename(filename).map_err(|_| {
        "The site provided a download filename that is not safe on this computer".to_owned()
    })?;
    Ok(filename.to_owned())
}

fn download_media_type(filename: &str) -> Result<String, String> {
    let media_type = if deliverable_media_type(filename).is_some() {
        "application/octet-stream"
    } else {
        binary_media_type_for_extension(filename)
    };
    if media_type == "application/octet-stream" && deliverable_media_type(filename).is_none() {
        return Err("This file type is not supported as a browser output".to_owned());
    }
    validate_binary_deliverable(filename, media_type).map_err(str::to_owned)?;
    Ok(media_type.to_owned())
}

fn validate_download_content(filename: &str, media_type: &str, bytes: &[u8]) -> Result<(), String> {
    if deliverable_media_type(filename).is_some() {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| "The downloaded text file is not valid UTF-8".to_owned())?;
        if text.contains('\0') {
            return Err("The downloaded text file contains invalid data".to_owned());
        }
        return Ok(());
    }
    let matches = match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/svg+xml" => false,
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "application/zip" => valid_zip(bytes, None),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            valid_zip(bytes, Some("word/"))
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            valid_zip(bytes, Some("xl/"))
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            valid_zip(bytes, Some("ppt/"))
        }
        "application/vnd.apache.parquet" => {
            bytes.len() >= 8 && bytes.starts_with(b"PAR1") && bytes.ends_with(b"PAR1")
        }
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err("The downloaded bytes do not match the file type".to_owned())
    }
}

fn valid_zip(bytes: &[u8], required_prefix: Option<&str>) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    match required_prefix {
        None => true,
        Some(prefix) => (0..archive.len()).any(|index| {
            archive
                .by_index(index)
                .ok()
                .is_some_and(|entry| entry.name().starts_with(prefix))
        }),
    }
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(invalid_data(
                "browser download storage is not a real directory",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    open_private_directory(path).map(|_| ())
}

fn open_private_directory(path: &Path) -> io::Result<Dir> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid_data(
            "browser download storage is not a real directory",
        ));
    }
    let directory = Dir::open_ambient_dir(path, ambient_authority())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = directory.dir_metadata()?;
        if metadata.dev() != cap_std::fs::MetadataExt::dev(&opened)
            || metadata.ino() != cap_std::fs::MetadataExt::ino(&opened)
        {
            return Err(invalid_data(
                "browser download storage changed while it was opened",
            ));
        }
    }
    Ok(directory)
}

fn load_receipts_locked(root: &Path) -> io::Result<Vec<BrowserDownloadReceipt>> {
    let mut receipts = Vec::new();
    let mut identities = HashSet::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_data("invalid browser download state name"))?;
        let Some(operation_id) = file_name
            .strip_prefix(RECEIPT_PREFIX)
            .and_then(|name| name.strip_suffix(".json"))
        else {
            if file_name.starts_with('.') && file_name.ends_with(".tmp") {
                fs::remove_file(entry.path())?;
            }
            continue;
        };
        if receipts.len() >= MAX_DOWNLOAD_RECEIPTS {
            return Err(invalid_data("too many browser download receipts"));
        }
        let operation_id = Uuid::parse_str(operation_id).map_err(invalid_data)?;
        validate_private_file(&entry.path(), MAX_RECEIPT_BYTES)?;
        let mut bytes = Vec::new();
        File::open(entry.path())?
            .take((MAX_RECEIPT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(invalid_data("browser download receipt is too large"));
        }
        let receipt: BrowserDownloadReceipt =
            serde_json::from_slice(&bytes).map_err(invalid_data)?;
        receipt.validate()?;
        if receipt.operation_id != operation_id || !identities.insert(operation_id) {
            return Err(invalid_data("browser download receipt identity mismatch"));
        }
        receipts.push(receipt);
    }
    Ok(receipts)
}

fn load_receipt_locked(
    root: &Path,
    operation_id: Uuid,
) -> io::Result<Option<BrowserDownloadReceipt>> {
    let path = receipt_path(root, operation_id);
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    validate_private_file(&path, MAX_RECEIPT_BYTES)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_RECEIPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let receipt: BrowserDownloadReceipt = serde_json::from_slice(&bytes).map_err(invalid_data)?;
    receipt.validate()?;
    if receipt.operation_id != operation_id {
        return Err(invalid_data("browser download receipt identity mismatch"));
    }
    Ok(Some(receipt))
}

fn save_receipt_locked(root: &Path, receipt: &BrowserDownloadReceipt) -> io::Result<()> {
    receipt.validate()?;
    let bytes = serde_json::to_vec(receipt).map_err(invalid_data)?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid_data("browser download receipt is too large"));
    }
    write_atomically(root, &receipt_path(root, receipt.operation_id), &bytes)
}

fn remove_receipt_locked(root: &Path, operation_id: Uuid) -> io::Result<()> {
    match fs::remove_file(receipt_path(root, operation_id)) {
        Ok(()) => sync_directory(root),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_stage_locked(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => path.parent().map_or(Ok(()), sync_directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup_orphan_stages_locked(root: &Path, retained: &HashSet<Uuid>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_data("invalid browser download state name"))?;
        let Some(operation_id) = file_name.strip_prefix(STAGE_PREFIX) else {
            continue;
        };
        let operation_id = Uuid::parse_str(operation_id).map_err(invalid_data)?;
        if retained.contains(&operation_id) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    sync_directory(root)
}

fn receipt_path(root: &Path, operation_id: Uuid) -> PathBuf {
    root.join(format!("{RECEIPT_PREFIX}{operation_id}.json"))
}

fn stage_name(operation_id: Uuid) -> String {
    format!("{STAGE_PREFIX}{operation_id}")
}

fn stage_path(root: &Path, operation_id: Uuid) -> PathBuf {
    root.join(stage_name(operation_id))
}

fn validate_private_file(path: &Path, max_bytes: usize) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes as u64
    {
        return Err(invalid_data("private browser download state is invalid"));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid_data(
            "private browser download state permissions are too broad",
        ));
    }
    Ok(())
}

fn write_atomically(directory: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = StdOpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, destination)?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn private_storage_error(_: io::Error) -> String {
    "Could not update private browser download storage".to_owned()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::BrowserOrigin;

    #[test]
    fn filenames_are_closed_portable_and_copy_safe() {
        assert_eq!(
            suggested_download_filename(Path::new("/tmp/report.pdf")).unwrap(),
            "report.pdf"
        );
        assert!(suggested_download_filename(Path::new("/tmp/../evil.pdf")).is_ok());
        assert!(suggested_download_filename(Path::new("/tmp/.hidden.pdf")).is_err());
        assert!(suggested_download_filename(Path::new("/tmp/CON.txt")).is_err());
        assert_eq!(
            filename_with_copy_suffix("report.pdf", 2).unwrap(),
            "report (2).pdf"
        );
        let long = format!("{}.pdf", "a".repeat(116));
        assert!(filename_with_copy_suffix(&long, 999).unwrap().len() <= MAX_DELIVERABLE_NAME_CHARS);
    }

    #[test]
    fn unsupported_extensions_and_mismatched_bytes_are_rejected() {
        assert!(download_media_type("installer.exe").is_err());
        assert_eq!(
            download_media_type("report.pdf").unwrap(),
            "application/pdf"
        );
        assert!(validate_download_content("report.pdf", "application/pdf", b"MZpayload").is_err());
        assert!(validate_download_content("report.pdf", "application/pdf", b"%PDF-1.7").is_ok());
        assert!(
            validate_download_content("notes.txt", "application/octet-stream", b"hello").is_ok()
        );
        assert!(
            validate_download_content("notes.txt", "application/octet-stream", b"bad\0text")
                .is_err()
        );
    }

    #[test]
    fn durable_receipts_recover_completed_downloads_and_drop_interrupted_ones() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrowserDownloadStore::open(temp.path()).unwrap();
        let chat_id = ChatId::new();
        let url = Url::parse("https://example.com/report.pdf").unwrap();
        let mut interrupted = BrowserDownloadReceipt::new(
            Uuid::new_v4(),
            chat_id,
            "browser-1",
            &url,
            "report.pdf".to_owned(),
            "application/pdf".to_owned(),
        );
        interrupted.launch_id = Uuid::new_v4();
        store.save_receipt(&interrupted).unwrap();
        fs::write(
            stage_path(&store.inner.root, interrupted.operation_id),
            b"partial",
        )
        .unwrap();

        let mut completed = BrowserDownloadReceipt::new(
            Uuid::new_v4(),
            chat_id,
            "browser-2",
            &url,
            "other.pdf".to_owned(),
            "application/pdf".to_owned(),
        );
        completed.phase = BrowserDownloadPhase::Downloaded;
        store.save_receipt(&completed).unwrap();
        fs::write(
            stage_path(&store.inner.root, completed.operation_id),
            b"%PDF-1.7",
        )
        .unwrap();

        let recovered = store.recovery_batch().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].operation_id, completed.operation_id);
        assert!(!stage_path(&store.inner.root, interrupted.operation_id).exists());
        assert!(store
            .load_receipt(interrupted.operation_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_browser_cannot_start_two_indistinguishable_downloads() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrowserDownloadStore::open(temp.path()).unwrap();
        let registry = BrowserRegistry::default();
        let chat_id = ChatId::new();
        let workspace_id = format!("{FOREGROUND_SCOPE_PREFIX}{chat_id}");
        registry
            .register(
                "browser-1",
                &workspace_id,
                "https://example.com".to_owned(),
                true,
            )
            .unwrap();
        let url = Url::parse("https://example.com/report.pdf").unwrap();

        store
            .begin(
                &registry,
                "browser-1",
                &workspace_id,
                &url,
                Path::new("/tmp/report.pdf"),
            )
            .unwrap();
        let error = store
            .begin(
                &registry,
                "browser-1",
                &workspace_id,
                &url,
                Path::new("/tmp/report.pdf"),
            )
            .unwrap_err();

        assert_eq!(error, "This download is already in progress");
    }

    #[test]
    fn cancellation_removes_the_exact_pending_download() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrowserDownloadStore::open(temp.path()).unwrap();
        let registry = BrowserRegistry::default();
        let chat_id = ChatId::new();
        let workspace_id = format!("{FOREGROUND_SCOPE_PREFIX}{chat_id}");
        registry
            .register(
                "browser-1",
                &workspace_id,
                "https://example.com".to_owned(),
                true,
            )
            .unwrap();
        let url = Url::parse("https://example.com/report.pdf").unwrap();
        let started = store
            .begin(
                &registry,
                "browser-1",
                &workspace_id,
                &url,
                Path::new("/tmp/report.pdf"),
            )
            .unwrap();
        fs::write(&started.destination, b"partial").unwrap();

        let finished = store.finish("browser-1", &url, None, false).unwrap();

        assert!(matches!(finished, BrowserDownloadFinished::Rejected { .. }));
        assert!(!started.destination.exists());
        assert!(store.load_receipts().unwrap().is_empty());
    }

    #[test]
    fn oversized_staged_downloads_are_rejected_before_they_are_read() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrowserDownloadStore::open(temp.path()).unwrap();
        let url = Url::parse("https://example.com/report.pdf").unwrap();
        let mut receipt = BrowserDownloadReceipt::new(
            store.inner.launch_id,
            ChatId::new(),
            "browser-1",
            &url,
            "report.pdf".to_owned(),
            "application/pdf".to_owned(),
        );
        receipt.phase = BrowserDownloadPhase::Downloaded;
        store.save_receipt(&receipt).unwrap();
        File::create(stage_path(&store.inner.root, receipt.operation_id))
            .unwrap()
            .set_len((MAX_BINARY_DELIVERABLE_BYTES + 1) as u64)
            .unwrap();

        assert_eq!(
            store.read_staged(&receipt).unwrap_err(),
            format!(
                "The downloaded file is larger than {} MiB",
                MAX_BINARY_DELIVERABLE_BYTES / (1024 * 1024)
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_stage_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = BrowserDownloadStore::open(temp.path()).unwrap();
        let url = Url::parse("https://example.com/report.pdf").unwrap();
        let mut receipt = BrowserDownloadReceipt::new(
            store.inner.launch_id,
            ChatId::new(),
            "browser-1",
            &url,
            "report.pdf".to_owned(),
            "application/pdf".to_owned(),
        );
        receipt.phase = BrowserDownloadPhase::Downloaded;
        store.save_receipt(&receipt).unwrap();
        let outside_file = outside.path().join("report.pdf");
        fs::write(&outside_file, b"%PDF-1.7").unwrap();
        symlink(
            outside_file,
            stage_path(&store.inner.root, receipt.operation_id),
        )
        .unwrap();

        assert_eq!(
            store.read_staged(&receipt).unwrap_err(),
            "The staged download is not a regular file"
        );
    }

    #[test]
    fn download_authority_requires_a_visible_human_controlled_foreground_browser() {
        let registry = BrowserRegistry::default();
        let chat_id = ChatId::new();
        let workspace_id = format!("{FOREGROUND_SCOPE_PREFIX}{chat_id}");
        registry
            .register(
                "browser-1",
                &workspace_id,
                "https://example.com".to_owned(),
                true,
            )
            .unwrap();
        let url = Url::parse("https://files.example.com/report.pdf").unwrap();
        assert_eq!(
            authorized_chat_download(&registry, "browser-1", &workspace_id, &url).unwrap(),
            chat_id
        );
        assert!(authorized_chat_download(&registry, "browser-1", "workspace-1", &url).is_err());
        assert!(BrowserOrigin::from_url(url.as_str()).is_some());
    }
}
