//! Durable broker registry and mutation receipts.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Serialize};

use super::{
    has_physical_root_alias, root_display_name, BrokerError, MutationRecord, RegisteredRoot, State,
};
use crate::{
    path_policy::RootIdentity, Grant, GrantSubject, OperationId, RootAttachment, RootId,
    RootPolicy, Scope,
};

const STATE_VERSION: u32 = 2;
const STATE_FILE_NAME: &str = "host-broker-state.json";
pub(super) const MAX_STATE_FILE_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct StateFile {
    directory: PathBuf,
    path: PathBuf,
    _lock: File,
    #[cfg(test)]
    saves_until_failure: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    fail_after_publish: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedState {
    version: u32,
    roots: Vec<PersistedRoot>,
    grants: Vec<Grant>,
    attachments: Vec<RootAttachment>,
    mutations: Vec<PersistedMutation>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRoot {
    id: RootId,
    owner: GrantSubject,
    path: PathBuf,
    identity: RootIdentity,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedMutation {
    operation_id: OperationId,
    record: MutationRecord,
}

impl StateFile {
    pub(super) fn open(data_dir: &Path) -> Result<Self, BrokerError> {
        fs::create_dir_all(data_dir)?;
        let directory = fs::canonicalize(data_dir)?;
        #[cfg(unix)]
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let mut lock_options = OpenOptions::new();
        lock_options.read(true).write(true).create(true);
        #[cfg(unix)]
        lock_options.mode(0o600);
        let lock = lock_options.open(directory.join("host-broker.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another broker already owns this state directory",
                )
                .into())
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        Ok(Self {
            path: directory.join(STATE_FILE_NAME),
            directory,
            _lock: lock,
            #[cfg(test)]
            saves_until_failure: std::sync::atomic::AtomicUsize::new(usize::MAX),
            #[cfg(test)]
            fail_after_publish: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(super) fn load(&self, policy: &RootPolicy) -> Result<State, BrokerError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.len() > MAX_STATE_FILE_BYTES as u64 {
            return Err(BrokerError::StateTooLarge);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take((MAX_STATE_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STATE_FILE_BYTES {
            return Err(BrokerError::StateTooLarge);
        }
        let persisted: PersistedState = serde_json::from_slice(&bytes).map_err(invalid_data)?;
        if persisted.version != STATE_VERSION {
            return Err(invalid_data(format!(
                "unsupported broker state version {}",
                persisted.version
            ))
            .into());
        }

        let mut roots = HashMap::new();
        for item in persisted.roots {
            let validated = policy.open_root(&item.path)?;
            if validated.identity() != item.identity {
                return Err(invalid_data("persisted root identity changed").into());
            }
            let display_name = root_display_name(validated.canonical_path());
            if roots
                .insert(
                    item.id,
                    RegisteredRoot {
                        owner: item.owner,
                        display_name,
                        root: Arc::new(validated),
                    },
                )
                .is_some()
            {
                return Err(invalid_data("duplicate persisted root identity").into());
            }
        }

        let mut mutations = HashMap::new();
        for item in persisted.mutations {
            if mutations.insert(item.operation_id, item.record).is_some() {
                return Err(invalid_data("duplicate persisted operation identity").into());
            }
        }
        let state = State {
            roots,
            grants: persisted.grants,
            attachments: persisted.attachments,
            mutations,
            active_mutations: Default::default(),
        };
        validate_loaded_state(&state)?;
        Ok(state)
    }

    pub(super) fn save(&self, state: &State) -> Result<(), BrokerError> {
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering;

            let remaining = self.saves_until_failure.load(Ordering::SeqCst);
            if remaining != usize::MAX {
                if remaining == 0 {
                    self.saves_until_failure.store(usize::MAX, Ordering::SeqCst);
                    return Err(io::Error::other("injected broker state save failure").into());
                }
                self.saves_until_failure.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let mut roots = state
            .roots
            .iter()
            .map(|(id, root)| PersistedRoot {
                id: *id,
                owner: root.owner,
                path: root.root.canonical_path().to_path_buf(),
                identity: root.root.identity(),
            })
            .collect::<Vec<_>>();
        roots.sort_by_key(|root| root.id.to_string());
        let mut mutations = state
            .mutations
            .iter()
            .map(|(operation_id, record)| PersistedMutation {
                operation_id: *operation_id,
                record: record.clone(),
            })
            .collect::<Vec<_>>();
        mutations.sort_by_key(|item| item.operation_id.to_string());
        let persisted = PersistedState {
            version: STATE_VERSION,
            roots,
            grants: state.grants.clone(),
            attachments: state.attachments.clone(),
            mutations,
        };
        let bytes = serde_json::to_vec_pretty(&persisted).map_err(invalid_data)?;
        if bytes.len() > MAX_STATE_FILE_BYTES {
            return Err(BrokerError::StateTooLarge);
        }
        self.write_atomically(&bytes).map_err(|error| {
            if error.published {
                BrokerError::PersistenceAmbiguous
            } else {
                BrokerError::Io(error.source)
            }
        })?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn fail_after_saves(&self, successful_saves: usize) {
        use std::sync::atomic::Ordering;

        self.saves_until_failure
            .store(successful_saves, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn fail_once_after_publish(&self) {
        use std::sync::atomic::Ordering;

        self.fail_after_publish.store(true, Ordering::SeqCst);
    }

    fn write_atomically(&self, bytes: &[u8]) -> Result<(), AtomicWriteError> {
        let temporary = self
            .directory
            .join(format!(".{STATE_FILE_NAME}.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), AtomicWriteError> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary).map_err(AtomicWriteError::before)?;
            file.write_all(bytes).map_err(AtomicWriteError::before)?;
            file.sync_all().map_err(AtomicWriteError::before)?;
            drop(file);
            replace_file(&temporary, &self.path).map_err(AtomicWriteError::before)?;
            #[cfg(test)]
            if self
                .fail_after_publish
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(AtomicWriteError::after(io::Error::other(
                    "injected post-publication failure",
                )));
            }
            sync_directory(&self.directory).map_err(AtomicWriteError::after)
        })();
        if result.as_ref().is_err_and(|error| !error.published) {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

struct AtomicWriteError {
    source: io::Error,
    published: bool,
}

impl AtomicWriteError {
    fn before(source: io::Error) -> Self {
        Self {
            source,
            published: false,
        }
    }

    fn after(source: io::Error) -> Self {
        Self {
            source,
            published: true,
        }
    }
}

pub(super) fn validate_loaded_state(state: &State) -> Result<(), BrokerError> {
    for grant in &state.grants {
        let root_id = match grant.scope() {
            Scope::Root { root_id } | Scope::PathSubtree { root_id, .. } => *root_id,
            Scope::Subject => continue,
        };
        if !state.roots.contains_key(&root_id) {
            return Err(invalid_data("grant references an unknown root").into());
        }
    }
    let mut attachment_identities = std::collections::HashSet::new();
    for attachment in &state.attachments {
        if !state.roots.contains_key(&attachment.root_id()) {
            return Err(invalid_data("attachment references an unknown root").into());
        }
        let has_matching_grant = state.grants.iter().any(|grant| {
            matches!(
                grant.scope(),
                Scope::Root { root_id } | Scope::PathSubtree { root_id, .. }
                    if *root_id == attachment.root_id()
            ) && match grant.subject().kind() {
                crate::SubjectKind::Project => true,
                crate::SubjectKind::Conversation => {
                    grant.subject().id() == attachment.conversation_id()
                }
            }
        });
        if !has_matching_grant {
            return Err(invalid_data("attachment has no matching subject grant").into());
        }
        if !attachment_identities.insert((attachment.conversation_id(), attachment.root_id())) {
            return Err(invalid_data("duplicate persisted root attachment").into());
        }
    }
    for (root_id, root) in &state.roots {
        let has_grant = state.grants.iter().any(|grant| {
            grant.subject() == root.owner
                && matches!(
                    grant.scope(),
                    Scope::Root { root_id: granted }
                        | Scope::PathSubtree {
                            root_id: granted,
                            ..
                        } if granted == root_id
                )
        });
        if !has_grant {
            return Err(invalid_data("persisted root is missing its grant").into());
        }
    }
    for record in state.mutations.values() {
        match record {
            MutationRecord::Register {
                request,
                outcome: super::MutationOutcome::Complete(Ok(result)),
            } => {
                if let Some(root) = state.roots.get(&result.root.root_id) {
                    let subject_has_grant = state.grants.iter().any(|grant| {
                        grant.subject() == request.subject
                            && matches!(
                                grant.scope(),
                                Scope::Root { root_id }
                                    | Scope::PathSubtree { root_id, .. }
                                    if *root_id == result.root.root_id
                            )
                    });
                    if root.display_name != result.root.display_name || !subject_has_grant {
                        return Err(invalid_data(
                            "successful register receipt does not match authoritative state",
                        )
                        .into());
                    }
                } else {
                    let was_revoked = state.mutations.values().any(|record| {
                        matches!(
                            record,
                            MutationRecord::Revoke {
                                request: revoke,
                            outcome: super::MutationOutcome::Complete(Ok(revoke_result)),
                        } if revoke_result.revoked
                                && revoke.root_id == result.root.root_id
                        )
                    });
                    if !was_revoked {
                        return Err(invalid_data("successful register receipt has no root").into());
                    }
                }
            }
            MutationRecord::Revoke {
                request,
                outcome: super::MutationOutcome::Complete(Ok(result)),
            } => {
                let still_owned = state
                    .roots
                    .get(&request.root_id)
                    .is_some_and(|root| root.owner == request.subject);
                let root_still_exists = state.roots.contains_key(&request.root_id);
                let blocked_by_legacy_alias =
                    still_owned && has_physical_root_alias(state, request.root_id);
                let was_registered = state.mutations.values().any(|record| {
                    matches!(
                        record,
                        MutationRecord::Register {
                            request: register,
                            outcome: super::MutationOutcome::Complete(Ok(register_result)),
                        } if register_result.root.root_id == request.root_id
                            && register.subject == request.subject
                    )
                });
                if (result.revoked && (root_still_exists || !was_registered))
                    || (!result.revoked && still_owned && !blocked_by_legacy_alias)
                {
                    return Err(invalid_data(
                        "successful revoke receipt does not match authoritative state",
                    )
                    .into());
                }
            }
            MutationRecord::Attachment {
                outcome: super::MutationOutcome::Pending,
                ..
            } => {
                return Err(invalid_data("persisted attachment mutation is incomplete").into());
            }
            MutationRecord::Attachment {
                request,
                outcome: super::MutationOutcome::Complete(Ok(result)),
            } if result.root_id != request.root_id || result.mutation != request.mutation => {
                return Err(invalid_data(
                    "successful attachment receipt does not match its request",
                )
                .into());
            }
            _ => {}
        }
    }
    Ok(())
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(unix)]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file(_temporary: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic broker state replacement is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}
