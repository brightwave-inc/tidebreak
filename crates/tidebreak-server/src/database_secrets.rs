//! Self-host secret custody in the deployment's own database (decision 102).
//!
//! Each stored secret is one `deployment_secrets` row, encrypted with
//! AES-256-GCM under a 32-byte key the operator keeps in the file
//! `TIDEBREAK_SECRET_KEY_FILE` names. A database dump or backup alone reveals
//! no secret. Whoever holds both the key file and the database reads every
//! one.
//!
//! - Every write draws a fresh random 96-bit nonce.
//! - The associated data is [`ASSOCIATED_DATA_LABEL`] followed by the secret's
//!   name, so a ciphertext copied to another name fails to decrypt, and so
//!   does one altered in place.
//! - Every row records the id of the key that wrote it: the first 8 bytes of
//!   the key's SHA-256, in hex. [`DatabaseSecretProvider::open`] refuses a key
//!   that did not write the stored rows, and no row written under another key
//!   is ever replaced or removed.
//! - [`DatabaseSecretProvider::open`] also decrypts every row once, so a row
//!   that no longer decrypts stops the boot instead of reading as a missing
//!   credential later. After boot, a row that fails to decrypt is an error
//!   that names the secret, never a missing secret. Errors name secrets, never
//!   values or the key.
//! - A key file that accounts other than its owner can change is refused, and
//!   one they can read draws a warning. The check follows symlinks, as a
//!   Kubernetes secret mount is one.
//!
//! The key protects dumps and backups, not a database someone can write to:
//! a writer can put back an older row under the same name and key, and it
//! still decrypts.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use chrono::Utc;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom as _, SystemRandom};
use tidebreak_core::{
    AgentError, DbStore, DeploymentSecret, DeploymentSecretWrite, Result, SecretProvider,
};

/// The variable that names the key file, as operators see it in messages.
const KEY_FILE_VARIABLE: &str = "TIDEBREAK_SECRET_KEY_FILE";
/// Put in front of a secret's name to form its associated data. A new
/// encryption format takes a new label, so a row written in one format never
/// decrypts as another.
pub const ASSOCIATED_DATA_LABEL: &str = "tidebreak-secret-v1:";
const KEY_LEN: usize = 32;
/// How many leading bytes of the key's SHA-256 form its id.
const KEY_ID_LEN: usize = 8;
/// Far above the 45 bytes a base64 key and its newline take. A larger file
/// is not a key file, and reading all of it would only cost memory.
const KEY_FILE_LIMIT: u64 = 4 * 1024;
/// The Vault custody's bound, so both accept the same credentials.
const SECRET_VALUE_LIMIT: usize = 1024 * 1024;
const SECRET_NAME_LIMIT: usize = 512;

/// The operator's key, read once at boot. Its debug output shows only the id.
pub struct SecretKey {
    key: LessSafeKey,
    id: String,
}

impl SecretKey {
    /// Read the key from `path`: 32 bytes as one line of standard base64,
    /// which `openssl rand -base64 32` writes. Trailing whitespace is
    /// ignored.
    ///
    /// A missing, unreadable, or malformed file is a configuration error that
    /// names the variable and the path, never the file's contents.
    pub fn from_file(path: &Path) -> Result<Self> {
        let shown = path.display();
        let unreadable = |error: std::io::Error| {
            AgentError::config(format!(
                "could not read {KEY_FILE_VARIABLE} at {shown}: {error}. Let the Tidebreak \
                 process read the file"
            ))
        };
        let file = std::fs::File::open(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AgentError::config(format!(
                    "{KEY_FILE_VARIABLE} names {shown}, and no file is there. Check that the \
                     key file is mounted at that path. Only a deployment that has never stored \
                     secrets should create a new key there, with `openssl rand -base64 32`"
                ))
            } else {
                unreadable(error)
            }
        })?;
        // The opened file's own mode: when the path is a symlink, as a
        // Kubernetes secret mount is, this is the target's.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = file.metadata().map_err(unreadable)?.permissions().mode();
            match key_file_access(mode) {
                KeyFileAccess::Private => {}
                KeyFileAccess::ReadableByOthers => tracing::warn!(
                    "{KEY_FILE_VARIABLE} at {shown} can be read by accounts other than its \
                     owner. Restrict it with `chmod 400 {shown}`"
                ),
                KeyFileAccess::WritableByOthers => {
                    return Err(AgentError::config(format!(
                        "{KEY_FILE_VARIABLE} at {shown} can be changed by accounts other than \
                         its owner, so one of them could replace the key with one they know. \
                         Restrict it with `chmod 400 {shown}` and start Tidebreak again"
                    )));
                }
            }
        }
        let mut encoded = Vec::new();
        file.take(KEY_FILE_LIMIT + 1)
            .read_to_end(&mut encoded)
            .map_err(unreadable)?;
        let key = if encoded.len() as u64 > KEY_FILE_LIMIT {
            Err("is too large to be a key file".to_owned())
        } else {
            Self::decode(&encoded)
        };
        key.map_err(|problem| {
            AgentError::config(format!(
                "{KEY_FILE_VARIABLE} at {shown} {problem}. It must hold a 32-byte key as one \
                 line of standard base64, which `openssl rand -base64 32` writes"
            ))
        })
    }

    /// The key a file's bytes encode, or what is wrong with them, phrased to
    /// follow the file's name.
    fn decode(encoded: &[u8]) -> std::result::Result<Self, String> {
        let encoded = encoded.trim_ascii_end();
        if encoded.is_empty() {
            return Err("is empty".to_owned());
        }
        // The decoder's own error names the offending byte, which is part of
        // the key; say only that the file is not base64.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "is not base64".to_owned())?;
        let bytes: [u8; KEY_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("decodes to {} bytes, not {KEY_LEN}", bytes.len()))?;
        Self::from_bytes(&bytes).map_err(|_| "could not be loaded as a key".to_owned())
    }

    fn from_bytes(bytes: &[u8; KEY_LEN]) -> Result<Self> {
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
        let id = digest.as_ref()[..KEY_ID_LEN]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let key = UnboundKey::new(&AES_256_GCM, bytes)
            .map_err(|_| AgentError::config("could not load the secret key"))?;
        Ok(Self {
            key: LessSafeKey::new(key),
            id,
        })
    }

    /// The key's id: the first 8 bytes of its SHA-256, in hex. Every row
    /// records the id of the key that wrote it, and the id reveals nothing
    /// that decrypts a row.
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Who besides its owner can reach a key file.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyFileAccess {
    /// Only the owner.
    Private,
    /// Its group or everyone can read it, but not change it.
    ReadableByOthers,
    /// Its group or everyone can change it.
    WritableByOthers,
}

#[cfg(unix)]
fn key_file_access(mode: u32) -> KeyFileAccess {
    if mode & 0o022 != 0 {
        KeyFileAccess::WritableByOthers
    } else if mode & 0o044 != 0 {
        KeyFileAccess::ReadableByOthers
    } else {
        KeyFileAccess::Private
    }
}

/// `value` as a SQL string literal, for a statement a refusal suggests. A
/// row's name or key id comes from the database, so a quote in it must not
/// end the literal early.
fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretKey")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// A [`SecretProvider`] that keeps each secret as an encrypted row in the
/// deployment's own database.
pub struct DatabaseSecretProvider {
    store: Arc<DbStore>,
    key: SecretKey,
    random: SystemRandom,
}

impl DatabaseSecretProvider {
    /// Open the custody over `store` with `key`.
    ///
    /// Refuses when any stored secret was written under another key: every
    /// read of it would fail, and nothing this key writes could replace it.
    /// Then decrypts every row once and refuses when one no longer decrypts,
    /// as after a damaged restore: a caller that checks whether a credential
    /// exists would read the failure as "none", and the deployment would look
    /// unconfigured. Either way the boot stops before anything reads a
    /// credential, and the rows stay exactly as they are.
    pub async fn open(store: Arc<DbStore>, key: SecretKey) -> Result<Self> {
        let stored = store.deployment_secrets().await?;
        let mut others: Vec<&str> = stored
            .iter()
            .map(|secret| secret.key_id.as_str())
            .filter(|id| *id != key.id)
            .collect();
        others.sort_unstable();
        others.dedup();
        if !others.is_empty() {
            let noun = if others.len() == 1 { "key" } else { "keys" };
            let listed = others
                .iter()
                .map(|id| sql_literal(id))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(AgentError::config(format!(
                "{KEY_FILE_VARIABLE} does not match the stored secrets: the database holds \
                 secrets written under {noun} {}, and this file holds key {}. Restore the \
                 original key file and start Tidebreak again. If the original key is lost, \
                 those secrets cannot be recovered: remove them with `DELETE FROM \
                 deployment_secrets WHERE key_id IN ({listed})` and enter the credentials \
                 again. No stored secret was changed",
                others.join(", "),
                key.id
            )));
        }
        let custody = Self {
            store,
            key,
            random: SystemRandom::new(),
        };
        for secret in &stored {
            // The value is dropped at once; only whether it decrypts matters.
            if custody.decrypt(&secret.name, secret).is_err() {
                let name = &secret.name;
                return Err(AgentError::config(format!(
                    "{KEY_FILE_VARIABLE} cannot decrypt the stored secret {name}, although \
                     its key id matches this key: its row was altered or damaged, for example \
                     by a bad restore. Restore the database from a backup taken before the \
                     damage, or remove the secret with `DELETE FROM deployment_secrets WHERE \
                     name = {}` and enter its credentials again. No stored secret was changed",
                    sql_literal(name)
                )));
            }
        }
        tracing::info!(
            key_id = %custody.key.id,
            "stored secrets are kept encrypted in the database"
        );
        Ok(custody)
    }

    fn associated_data(name: &str) -> Aad<Vec<u8>> {
        Aad::from(format!("{ASSOCIATED_DATA_LABEL}{name}").into_bytes())
    }

    fn encrypt(&self, name: &str, value: &str) -> Result<DeploymentSecret> {
        let mut nonce = [0u8; NONCE_LEN];
        self.random.fill(&mut nonce).map_err(|_| {
            AgentError::Secret(format!(
                "could not draw a nonce to encrypt the secret {name}"
            ))
        })?;
        let mut ciphertext = value.as_bytes().to_vec();
        self.key
            .key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Self::associated_data(name),
                &mut ciphertext,
            )
            .map_err(|_| AgentError::Secret(format!("could not encrypt the secret {name}")))?;
        Ok(DeploymentSecret {
            name: name.to_owned(),
            key_id: self.key.id.clone(),
            nonce: nonce.to_vec(),
            ciphertext,
            updated_at: Utc::now(),
        })
    }

    /// Decrypt the row read for `name`. The associated data is built from
    /// the name the caller asked for, not from anything the row says.
    fn decrypt(&self, name: &str, secret: &DeploymentSecret) -> Result<String> {
        if secret.key_id != self.key.id {
            return Err(AgentError::Secret(format!(
                "the stored secret {name} was written under key {}, not this deployment's key \
                 {}; restore the original {KEY_FILE_VARIABLE}",
                secret.key_id, self.key.id
            )));
        }
        let undecryptable = || {
            AgentError::Secret(format!(
                "the stored secret {name} could not be decrypted: its row was altered, or copied \
                 from another secret"
            ))
        };
        let nonce = Nonce::try_assume_unique_for_key(&secret.nonce).map_err(|_| undecryptable())?;
        let mut buffer = secret.ciphertext.clone();
        let plaintext = self
            .key
            .key
            .open_in_place(nonce, Self::associated_data(name), &mut buffer)
            .map_err(|_| undecryptable())?;
        String::from_utf8(plaintext.to_vec())
            .map_err(|_| AgentError::Secret(format!("the stored secret {name} is not valid UTF-8")))
    }
}

#[async_trait]
impl SecretProvider for DatabaseSecretProvider {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        check_name(key)?;
        match self.store.deployment_secret(key).await? {
            Some(secret) => self.decrypt(key, &secret).map(Some),
            None => Ok(None),
        }
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        check_name(key)?;
        if value.len() > SECRET_VALUE_LIMIT {
            return Err(AgentError::Secret(format!(
                "the secret {key} exceeds the {SECRET_VALUE_LIMIT}-byte storage limit"
            )));
        }
        let secret = self.encrypt(key, value)?;
        match self.store.put_deployment_secret(&secret).await? {
            DeploymentSecretWrite::Done => Ok(()),
            DeploymentSecretWrite::OtherKey => Err(written_under_another_key(key)),
        }
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        check_name(key)?;
        match self
            .store
            .delete_deployment_secret(key, &self.key.id)
            .await?
        {
            DeploymentSecretWrite::Done => Ok(()),
            DeploymentSecretWrite::OtherKey => Err(written_under_another_key(key)),
        }
    }
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > SECRET_NAME_LIMIT {
        return Err(AgentError::Secret(format!(
            "the credential key must contain 1 to {SECRET_NAME_LIMIT} bytes"
        )));
    }
    Ok(())
}

fn written_under_another_key(name: &str) -> AgentError {
    AgentError::Secret(format!(
        "the stored secret {name} was written under another key, so it was left unchanged; \
         restore the original {KEY_FILE_VARIABLE}"
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sha2::{Digest as _, Sha256};

    use super::*;

    async fn test_store() -> (tempfile::TempDir, Arc<DbStore>) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let store = DbStore::connect_test_sqlite_fixture(&url).await.unwrap();
        (dir, Arc::new(store))
    }

    fn random_bytes<const N: usize>() -> [u8; N] {
        let mut bytes = [0u8; N];
        SystemRandom::new().fill(&mut bytes).unwrap();
        bytes
    }

    fn random_key() -> SecretKey {
        SecretKey::from_bytes(&random_bytes::<KEY_LEN>()).unwrap()
    }

    fn encoded(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// A key file readable only by its owner, whatever the test's umask. A
    /// file an earlier call left read-only is replaced, not written through.
    fn key_file(dir: &Path, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = dir.join("secret.key");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        set_mode(&path, 0o600);
        path
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Load `path` while capturing what the load logs.
    fn load_logging(path: &Path) -> (Result<SecretKey>, String) {
        #[derive(Clone)]
        struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Captured {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Captured(Arc::default());
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let loaded = tracing::subscriber::with_default(subscriber, || SecretKey::from_file(path));
        let logged = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        (loaded, logged)
    }

    /// A value made at run time, so no credential-shaped literal sits in the
    /// source.
    fn fake_value(label: &str) -> String {
        format!("{label}-{}", uuid::Uuid::new_v4().simple())
    }

    #[tokio::test]
    async fn a_secret_round_trips_and_is_stored_encrypted() {
        let (_dir, store) = test_store().await;
        let key = random_key();
        let key_id = key.id().to_owned();
        let secrets = DatabaseSecretProvider::open(store.clone(), key)
            .await
            .unwrap();
        let value = fake_value("round-trip");

        assert_eq!(secrets.get_secret("provider.test").await.unwrap(), None);
        secrets.set_secret("provider.test", &value).await.unwrap();
        assert_eq!(
            secrets.get_secret("provider.test").await.unwrap(),
            Some(value.clone())
        );

        let row = store
            .deployment_secret("provider.test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.key_id, key_id);
        assert_eq!(row.nonce.len(), NONCE_LEN);
        // The value plus the 16-byte GCM tag, and none of it in the clear.
        assert_eq!(row.ciphertext.len(), value.len() + 16);
        assert!(!row
            .ciphertext
            .windows(value.len())
            .any(|window| window == value.as_bytes()));
    }

    #[tokio::test]
    async fn an_overwrite_replaces_the_value_under_a_fresh_nonce() {
        let (_dir, store) = test_store().await;
        let secrets = DatabaseSecretProvider::open(store.clone(), random_key())
            .await
            .unwrap();
        let first = fake_value("first");
        let second = fake_value("second");

        secrets.set_secret("provider.test", &first).await.unwrap();
        let before = store
            .deployment_secret("provider.test")
            .await
            .unwrap()
            .unwrap();
        secrets.set_secret("provider.test", &second).await.unwrap();
        let after = store
            .deployment_secret("provider.test")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            secrets.get_secret("provider.test").await.unwrap(),
            Some(second)
        );
        assert_ne!(before.nonce, after.nonce, "every write draws a new nonce");

        // Writing the same value again still draws a new nonce, so equal
        // values never produce equal rows.
        secrets.set_secret("provider.test", &first).await.unwrap();
        let again = store
            .deployment_secret("provider.test")
            .await
            .unwrap()
            .unwrap();
        assert_ne!(before.nonce, again.nonce);
        assert_ne!(before.ciphertext, again.ciphertext);
    }

    #[tokio::test]
    async fn a_delete_removes_the_secret() {
        let (_dir, store) = test_store().await;
        let secrets = DatabaseSecretProvider::open(store.clone(), random_key())
            .await
            .unwrap();

        secrets
            .set_secret("provider.test", &fake_value("deleted"))
            .await
            .unwrap();
        secrets.delete_secret("provider.test").await.unwrap();
        assert_eq!(secrets.get_secret("provider.test").await.unwrap(), None);
        assert_eq!(
            store.deployment_secret("provider.test").await.unwrap(),
            None
        );
        secrets.delete_secret("provider.test").await.unwrap();
    }

    /// The name is the associated data, so a row copied under another name
    /// fails to decrypt, and the error names the secret, not its value.
    #[tokio::test]
    async fn a_ciphertext_moved_to_another_name_fails_to_decrypt() {
        let (_dir, store) = test_store().await;
        let secrets = DatabaseSecretProvider::open(store.clone(), random_key())
            .await
            .unwrap();
        let value = fake_value("moved");
        secrets.set_secret("provider.alpha", &value).await.unwrap();

        let mut moved = store
            .deployment_secret("provider.alpha")
            .await
            .unwrap()
            .unwrap();
        moved.name = "provider.beta".to_owned();
        assert_eq!(
            store.put_deployment_secret(&moved).await.unwrap(),
            DeploymentSecretWrite::Done
        );

        let error = secrets
            .get_secret("provider.beta")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("provider.beta"), "{error}");
        assert!(error.contains("could not be decrypted"), "{error}");
        assert!(!error.contains(&value), "{error}");
        assert_eq!(
            secrets.get_secret("provider.alpha").await.unwrap(),
            Some(value)
        );
    }

    #[tokio::test]
    async fn a_tampered_row_fails_to_decrypt() {
        let (_dir, store) = test_store().await;
        let secrets = DatabaseSecretProvider::open(store.clone(), random_key())
            .await
            .unwrap();
        let value = fake_value("tampered");
        secrets.set_secret("provider.test", &value).await.unwrap();
        let original = store
            .deployment_secret("provider.test")
            .await
            .unwrap()
            .unwrap();

        let mut flipped_ciphertext = original.clone();
        flipped_ciphertext.ciphertext[0] ^= 0x01;
        let mut flipped_tag = original.clone();
        *flipped_tag.ciphertext.last_mut().unwrap() ^= 0x80;
        let mut flipped_nonce = original.clone();
        flipped_nonce.nonce[0] ^= 0x01;
        let mut short_nonce = original.clone();
        short_nonce.nonce.pop();
        for tampered in [flipped_ciphertext, flipped_tag, flipped_nonce, short_nonce] {
            store.put_deployment_secret(&tampered).await.unwrap();
            let error = secrets
                .get_secret("provider.test")
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("provider.test"), "{error}");
            assert!(!error.contains(&value), "{error}");
        }

        store.put_deployment_secret(&original).await.unwrap();
        assert_eq!(
            secrets.get_secret("provider.test").await.unwrap(),
            Some(value)
        );
    }

    /// A key that did not write the stored secrets refuses the boot, says to
    /// restore the original key file, and changes no row.
    #[tokio::test]
    async fn the_wrong_key_refuses_to_open_and_changes_nothing() {
        let (_dir, store) = test_store().await;
        let original = random_bytes::<KEY_LEN>();
        let secrets =
            DatabaseSecretProvider::open(store.clone(), SecretKey::from_bytes(&original).unwrap())
                .await
                .unwrap();
        let value = fake_value("kept");
        secrets.set_secret("provider.test", &value).await.unwrap();
        let before = store.deployment_secret("provider.test").await.unwrap();

        let wrong = random_key();
        let wrong_id = wrong.id().to_owned();
        let error = match DatabaseSecretProvider::open(store.clone(), wrong).await {
            Err(error) => error,
            Ok(_) => panic!("a key that wrote no stored secret opened the custody"),
        };
        assert_eq!(error.kind(), "config");
        let message = error.to_string();
        assert!(
            message.contains("does not match the stored secrets"),
            "{message}"
        );
        assert!(
            message.contains("Restore the original key file"),
            "{message}"
        );
        assert!(message.contains(&wrong_id), "{message}");
        assert!(message.contains(secrets.key.id()), "{message}");
        // When the original key is gone, the refusal says how to start over
        // without touching rows the current key wrote.
        assert!(message.contains("If the original key is lost"), "{message}");
        assert!(
            message.contains(&format!(
                "DELETE FROM deployment_secrets WHERE key_id IN ('{}')",
                secrets.key.id()
            )),
            "{message}"
        );
        assert!(!message.contains(&value), "{message}");
        assert_eq!(
            store.deployment_secret("provider.test").await.unwrap(),
            before
        );

        let reopened =
            DatabaseSecretProvider::open(store.clone(), SecretKey::from_bytes(&original).unwrap())
                .await
                .unwrap();
        assert_eq!(
            reopened.get_secret("provider.test").await.unwrap(),
            Some(value)
        );
    }

    /// A row this key wrote that no longer decrypts, as after a damaged
    /// restore, refuses the boot and names the secret. Without the check the
    /// custody opened, and a caller asking whether a credential exists read
    /// the failure as "none".
    #[tokio::test]
    async fn a_row_that_no_longer_decrypts_refuses_to_open() {
        let (_dir, store) = test_store().await;
        let bytes = random_bytes::<KEY_LEN>();
        let secrets =
            DatabaseSecretProvider::open(store.clone(), SecretKey::from_bytes(&bytes).unwrap())
                .await
                .unwrap();
        let value = fake_value("damaged");
        secrets.set_secret("provider.o'hare", &value).await.unwrap();
        let original = store
            .deployment_secret("provider.o'hare")
            .await
            .unwrap()
            .unwrap();
        let mut damaged = original.clone();
        let middle = damaged.ciphertext.len() / 2;
        damaged.ciphertext[middle] ^= 0x10;
        store.put_deployment_secret(&damaged).await.unwrap();

        let error = match DatabaseSecretProvider::open(
            store.clone(),
            SecretKey::from_bytes(&bytes).unwrap(),
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("the custody opened over a row its key cannot decrypt"),
        };
        assert_eq!(error.kind(), "config");
        let message = error.to_string();
        assert!(message.contains("provider.o'hare"), "{message}");
        assert!(message.contains("cannot decrypt"), "{message}");
        assert!(message.contains("Restore the database"), "{message}");
        // The suggested statement quotes the name safely.
        assert!(
            message.contains("WHERE name = 'provider.o''hare'"),
            "{message}"
        );
        assert!(!message.contains(&value), "{message}");
        assert_eq!(
            store.deployment_secret("provider.o'hare").await.unwrap(),
            Some(damaged),
            "the refusal changed no row"
        );

        store.put_deployment_secret(&original).await.unwrap();
        let repaired =
            DatabaseSecretProvider::open(store.clone(), SecretKey::from_bytes(&bytes).unwrap())
                .await
                .unwrap();
        assert_eq!(
            repaired.get_secret("provider.o'hare").await.unwrap(),
            Some(value)
        );
    }

    /// A row that lands under another key after boot is still never replaced
    /// or removed, and reading it names the mismatch instead of reading as
    /// unset.
    #[tokio::test]
    async fn a_row_written_under_another_key_is_never_overwritten_or_deleted() {
        let (_dir, store) = test_store().await;
        let secrets = DatabaseSecretProvider::open(store.clone(), random_key())
            .await
            .unwrap();
        let other = DatabaseSecretProvider::open(store.clone(), random_key())
            .await
            .unwrap();
        other
            .set_secret("provider.test", &fake_value("other"))
            .await
            .unwrap();
        let before = store.deployment_secret("provider.test").await.unwrap();

        for error in [
            secrets
                .set_secret("provider.test", &fake_value("mine"))
                .await
                .unwrap_err(),
            secrets.delete_secret("provider.test").await.unwrap_err(),
            secrets.get_secret("provider.test").await.unwrap_err(),
        ] {
            let message = error.to_string();
            assert!(message.contains("provider.test"), "{message}");
            assert!(message.contains("another key") || message.contains("written under key"));
        }
        assert_eq!(
            store.deployment_secret("provider.test").await.unwrap(),
            before
        );
    }

    #[test]
    fn a_key_file_holds_32_bytes_of_base64_on_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = random_bytes::<KEY_LEN>();
        let expected_id = Sha256::digest(bytes)[..KEY_ID_LEN]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        // What `openssl rand -base64 32` writes, and the same with other
        // trailing whitespace.
        for contents in [
            format!("{}\n", encoded(&bytes)),
            encoded(&bytes),
            format!("{} \r\n\n", encoded(&bytes)),
        ] {
            let key = SecretKey::from_file(&key_file(dir.path(), contents)).unwrap();
            assert_eq!(key.id(), expected_id);
            assert_eq!(key.id().len(), 16);
        }
        let shown = format!("{:?}", SecretKey::from_bytes(&bytes).unwrap());
        assert!(shown.contains(&expected_id), "{shown}");

        let encoded_key = encoded(&bytes);
        for (contents, problem) in [
            (String::new(), "is empty"),
            ("\n \n".to_owned(), "is empty"),
            (format!("{encoded_key}\n{encoded_key}\n"), "is not base64"),
            (format!("  {encoded_key}"), "is not base64"),
            // A key cut short by a copy and paste: missing padding is not
            // base64, and a shorter whole key decodes to too few bytes.
            (format!("{}\n", &encoded_key[..43]), "is not base64"),
            (format!("{}*\n", &encoded_key[..43]), "is not base64"),
            (format!("{}\n", &encoded_key[..40]), "decodes to 30 bytes"),
            (
                format!("{}\n", encoded(&random_bytes::<16>())),
                "decodes to 16 bytes",
            ),
            (
                format!("{}\n", encoded(&random_bytes::<33>())),
                "decodes to 33 bytes",
            ),
            ("A".repeat(5000), "too large"),
        ] {
            let error = SecretKey::from_file(&key_file(dir.path(), &contents)).unwrap_err();
            assert_eq!(error.kind(), "config");
            let message = error.to_string();
            assert!(message.contains(problem), "{problem}: {message}");
            assert!(message.contains(KEY_FILE_VARIABLE), "{message}");
            assert!(message.contains("openssl rand -base64 32"), "{message}");
            assert!(!message.contains(&encoded_key), "{message}");
        }

        let missing = SecretKey::from_file(&dir.path().join("absent.key"))
            .unwrap_err()
            .to_string();
        assert!(missing.contains(KEY_FILE_VARIABLE), "{missing}");
        assert!(missing.contains("absent.key"), "{missing}");
        assert!(missing.contains("no file is there"), "{missing}");
        // A failed mount must not read as advice to replace the key.
        assert!(
            missing.contains("Check that the key file is mounted"),
            "{missing}"
        );
        assert!(missing.contains("never stored secrets"), "{missing}");

        // A directory where the file should be cannot be read as one.
        let unreadable = SecretKey::from_file(dir.path()).unwrap_err().to_string();
        assert!(unreadable.contains("could not read"), "{unreadable}");
        assert!(unreadable.contains(KEY_FILE_VARIABLE), "{unreadable}");
    }

    /// A key file others can change could be swapped for a key they know
    /// before the first secret is stored, so it is refused, symlink or not.
    #[cfg(unix)]
    #[test]
    fn a_key_file_others_can_change_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let contents = format!("{}\n", encoded(&random_bytes::<KEY_LEN>()));
        for mode in [0o620, 0o602, 0o622, 0o660, 0o666, 0o646] {
            let path = key_file(dir.path(), &contents);
            set_mode(&path, mode);
            let error = SecretKey::from_file(&path).unwrap_err();
            assert_eq!(error.kind(), "config");
            let message = error.to_string();
            assert!(
                message.contains("can be changed by accounts"),
                "{mode:o}: {message}"
            );
            assert!(message.contains("chmod 400"), "{mode:o}: {message}");
            assert!(message.contains(KEY_FILE_VARIABLE), "{mode:o}: {message}");
        }

        let target = key_file(dir.path(), &contents);
        set_mode(&target, 0o666);
        let link = dir.path().join("mounted.key");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let message = SecretKey::from_file(&link).unwrap_err().to_string();
        assert!(message.contains("can be changed by accounts"), "{message}");
    }

    /// A key file others can read still loads, with a warning that says how
    /// to restrict it. A private one loads quietly, including through a
    /// symlink, whose own mode says nothing about the file it names.
    #[cfg(unix)]
    #[test]
    fn a_key_file_others_can_read_loads_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = random_bytes::<KEY_LEN>();
        let contents = format!("{}\n", encoded(&bytes));
        let expected = SecretKey::from_bytes(&bytes).unwrap().id().to_owned();

        for mode in [0o640, 0o604, 0o644, 0o444] {
            let path = key_file(dir.path(), &contents);
            set_mode(&path, mode);
            let (loaded, logged) = load_logging(&path);
            assert_eq!(loaded.unwrap().id(), expected, "{mode:o}");
            assert!(logged.contains("WARN"), "{mode:o}: {logged}");
            assert!(
                logged.contains("can be read by accounts other than its owner"),
                "{mode:o}: {logged}"
            );
            assert!(logged.contains("chmod 400"), "{mode:o}: {logged}");
            assert!(!logged.contains(contents.trim()), "{mode:o}: {logged}");
        }

        for mode in [0o600, 0o400] {
            let path = key_file(dir.path(), &contents);
            set_mode(&path, mode);
            let (loaded, logged) = load_logging(&path);
            assert_eq!(loaded.unwrap().id(), expected, "{mode:o}");
            assert_eq!(logged, "", "{mode:o}");
        }

        let target = key_file(dir.path(), &contents);
        set_mode(&target, 0o400);
        let link = dir.path().join("mounted.key");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (loaded, logged) = load_logging(&link);
        assert_eq!(loaded.unwrap().id(), expected);
        assert_eq!(logged, "");
    }

    #[cfg(unix)]
    #[test]
    fn a_key_file_the_process_cannot_read_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = key_file(
            dir.path(),
            format!("{}\n", encoded(&random_bytes::<KEY_LEN>())),
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads any file, so the refusal only happens as another user.
        if std::fs::File::open(&path).is_ok() {
            return;
        }
        let error = SecretKey::from_file(&path).unwrap_err().to_string();
        assert!(error.contains("could not read"), "{error}");
        assert!(error.contains(KEY_FILE_VARIABLE), "{error}");
    }
}
