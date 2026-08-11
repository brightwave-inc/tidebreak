//! Durable receipts for the headless folder executor.
//!
//! The server hands a claimed client call to no other executor: only the exact
//! `(executor_id, lease_token)` pair that claimed it can recover or resolve it,
//! whether or not the lease has expired. So the executor must be able to
//! remember its own claim across a crash, or a killed process would leave a
//! parked call nothing can ever settle — the same hang the executor exists to
//! remove.
//!
//! One small JSON file per claim, in a private directory beside the profile's
//! other executor state. The file is written before the claim and again before
//! host I/O, and removed only once the server holds a terminal result. It
//! carries no folder path, no file content, and no credential: an identity, a
//! lease token, a phase, and — once known — the same model-facing outcome the
//! server will be given.

use std::path::{Path, PathBuf};

use openwave_core::{AgentError, CallId, ChatId, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::client::ClientExecutionOutcome;

/// What may be done with a claim whose host I/O had already begun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DispatchRecovery {
    /// Close the call out with a typed failure. A read has no durable host
    /// outcome to reconcile against, so repeating it would be a second
    /// disclosure this process cannot account for.
    Terminalize,
    /// Run it again. The operation's effect is identified by the request, so a
    /// repeat converges on the one effect rather than adding another.
    Retry,
}

/// One claim this executor owns, as much of it as must survive a crash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub(super) chat_id: ChatId,
    pub(super) call_id: CallId,
    pub(super) executor_id: Uuid,
    /// Chosen before the claim and never regenerated: it is the only key that
    /// can recover this claim.
    pub(super) lease_token: Uuid,
    pub(super) recovery: DispatchRecovery,
    /// Whether the host operation had been dispatched. Written durably before
    /// the first byte crosses into the broker.
    #[serde(default)]
    pub(super) dispatch_started: bool,
    /// The terminal answer, once computed. Present means the work is done and
    /// only publication remains, so recovery republishes it rather than running
    /// the operation a second time.
    #[serde(default)]
    pub(super) outcome: Option<ClientExecutionOutcome>,
}

impl Receipt {
    pub(super) fn new(
        chat_id: ChatId,
        call_id: CallId,
        executor_id: Uuid,
        recovery: DispatchRecovery,
    ) -> Self {
        Self {
            chat_id,
            call_id,
            executor_id,
            lease_token: Uuid::new_v4(),
            recovery,
            dispatch_started: false,
            outcome: None,
        }
    }
}

/// The receipt directory for one profile.
pub(super) struct ReceiptStore {
    root: PathBuf,
}

impl ReceiptStore {
    pub(super) fn open(data_dir: &Path) -> Result<Self> {
        let root = data_dir.join("folder-executions");
        create_private_dir(&root).map_err(|error| {
            AgentError::config(format!(
                "could not open the connected-folder executor state: {error}"
            ))
        })?;
        Ok(Self { root })
    }

    /// Every receipt on disk. An unreadable or unparsable file is reported
    /// rather than skipped: silently ignoring one would strand its call, and the
    /// operator can see and remove it.
    pub(super) fn load(&self) -> Result<Vec<Receipt>> {
        let entries = std::fs::read_dir(&self.root).map_err(private_state_error)?;
        let mut receipts = Vec::new();
        for entry in entries {
            let path = entry.map_err(private_state_error)?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(private_state_error)?;
            let receipt: Receipt = serde_json::from_str(&text).map_err(|_| {
                AgentError::msg(
                    "a connected-folder executor receipt is unreadable; \
                     remove it to let the conversation continue",
                )
            })?;
            // The name is derived from the identity, so a file that does not
            // match its own contents is not this store's.
            if path.file_name() != Some(self.path(receipt.call_id).file_name().unwrap_or_default())
            {
                return Err(AgentError::msg(
                    "a connected-folder executor receipt does not match its own identity",
                ));
            }
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    /// Write one receipt so it is durable before the step it fences.
    pub(super) fn save(&self, receipt: &Receipt) -> Result<()> {
        let text = serde_json::to_vec(receipt)
            .map_err(|_| AgentError::msg("could not encode a connected-folder executor receipt"))?;
        write_private_atomic(&self.path(receipt.call_id), &text).map_err(private_state_error)
    }

    pub(super) fn remove(&self, call_id: CallId) -> Result<()> {
        match std::fs::remove_file(self.path(call_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(private_state_error(error)),
        }
    }

    fn path(&self, call_id: CallId) -> PathBuf {
        self.root.join(format!("{call_id}.json"))
    }
}

/// Private state, not a credential — but it names this profile's pending host
/// work, so it inherits the data directory's own permissions rather than the
/// process umask.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Replace a receipt atomically and durably.
///
/// The rename is what makes a torn write impossible: a reader either sees the
/// previous phase or the new one, never half of either. Both the file and its
/// directory are synced, because a phase that only reached the page cache is a
/// phase a power loss can lose — and losing the `dispatch_started` fence is
/// exactly what would let a read be repeated.
fn write_private_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    // Best effort: not every platform lets a directory be opened for sync.
    if let Ok(directory) = std::fs::File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn private_state_error(_error: std::io::Error) -> AgentError {
    AgentError::msg("could not update the connected-folder executor's recovery state")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store's whole job: a phase written before a step is still there
    /// afterwards, so a process that died mid-operation is recognizable to the
    /// next one — and a published receipt leaves nothing behind to re-run.
    #[test]
    fn a_receipt_survives_the_process_that_wrote_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(dir.path()).unwrap();
        assert!(store.load().unwrap().is_empty());

        let mut receipt = Receipt::new(
            ChatId::new(),
            CallId::new(),
            Uuid::new_v4(),
            DispatchRecovery::Terminalize,
        );
        store.save(&receipt).unwrap();
        // A fresh store over the same directory is what the next run has.
        let recovered = ReceiptStore::open(dir.path()).unwrap().load().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].call_id, receipt.call_id);
        // The lease token is the only key that can recover the claim, so it must
        // come back byte for byte rather than being regenerated.
        assert_eq!(recovered[0].lease_token, receipt.lease_token);
        assert!(!recovered[0].dispatch_started);

        receipt.dispatch_started = true;
        store.save(&receipt).unwrap();
        assert!(store.load().unwrap()[0].dispatch_started);

        store.remove(receipt.call_id).unwrap();
        assert!(store.load().unwrap().is_empty());
        // Removing what is already gone is how a republished result settles.
        store.remove(receipt.call_id).unwrap();
    }
}
