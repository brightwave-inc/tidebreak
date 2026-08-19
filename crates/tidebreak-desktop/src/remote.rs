//! Remote connection mode: attaching this client to a machine it does not host.
//!
//! A machine is a running `tidebreak-server`. Normally the desktop app is both
//! machine and client: the shell boots the embedded server on loopback and the
//! webview attaches to it. Decision record 47 adds the other shape — the same
//! webview attached to a server somewhere else, reached with a base URL and a
//! token the user supplies.
//!
//! Two rules govern the address:
//!
//! - **TLS unless loopback.** The token is a long-lived bearer credential for
//!   somebody's whole account, so cleartext to anywhere but this machine hands
//!   it to whoever is on the path. This is the same line
//!   [`crate::deep_link`] holds for a provision link, and both call
//!   [`url_host_is_loopback`] so there is one rule, not two.
//! - **The address is proved before it is stored.** Connecting probes the
//!   machine with the token and stores nothing unless the machine answers. A
//!   stored address that never worked is a support case that looks like a bug.
//!
//! The embedded server keeps running while a remote attachment is live. It is
//! still this machine, and host authority still lives there; what changes is
//! which machine the renderer talks to. Host-authority surfaces answer for that
//! difference themselves — see the refusal reasons they carry.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use tidebreak_core::keychain::KeychainSecretProvider;
use tidebreak_core::storage::SecretProvider;

/// Keychain key holding the bearer token for the attached remote machine.
const TOKEN_KEY: &str = "desktop.remote-machine.token";

/// File under the profile data dir holding the attached machine's address.
/// The address is not a secret; the token that reaches it is, and lives in the
/// OS credential store instead.
const ADDRESS_FILE: &str = "remote-machine.json";

/// Route the connect probe calls. Cheap, authenticated, and present on every
/// profile, so a 2xx proves both reachability and a token the machine accepts.
const PROBE_PATH: &str = "/policy";

/// How long the connect probe waits before calling a machine unreachable.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Which machine the renderer is attached to.
///
/// The renderer branches on this rather than on the shape of the base URL: a
/// developer machine on loopback is still remote, and "is this URL loopback"
/// is not the question any caller actually has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attachment {
    /// The server this app booted, in this process, on loopback.
    Local,
    /// A server elsewhere, reached with a user-supplied URL and token.
    Remote,
}

/// Why a connect attempt was refused.
///
/// The reason is the contract: one stable string per distinct cause, following
/// the precedent `output_writeback_authority_unavailable` set. Prose does not
/// cross this boundary — the renderer owns the copy and switches on the reason,
/// the same way it does for an export failure. `detail` carries the underlying
/// transport or credential-store text for diagnostics, never for display.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConnectError {
    /// Stable reason code.
    pub reason: &'static str,
    /// Underlying cause, for logs and support. Not shown as-is.
    pub detail: Option<String>,
}

/// The address is not a URL this client can use at all.
pub const REASON_URL_INVALID: &str = "remote_machine_url_invalid";
/// The address is cleartext `http` and its host is not this machine.
pub const REASON_REQUIRES_TLS: &str = "remote_machine_requires_tls";
/// The machine did not answer the probe.
pub const REASON_UNREACHABLE: &str = "remote_machine_unreachable";
/// The machine answered, and refused the token.
pub const REASON_TOKEN_REFUSED: &str = "remote_machine_token_refused";
/// The machine answered the probe with something other than success or a
/// credential refusal — a fronting proxy, or a URL that names something that
/// is not a Tidebreak machine.
pub const REASON_NOT_A_MACHINE: &str = "remote_machine_not_a_machine";
/// The token could not be stored in, or read from, the OS credential store.
pub const REASON_TOKEN_STORAGE_FAILED: &str = "remote_machine_token_storage_failed";

impl RemoteConnectError {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    fn detailed(reason: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            reason,
            detail: Some(detail.to_string()),
        }
    }
}

/// What the renderer knows about the current attachment.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMachineState {
    /// Which machine the renderer reaches on the next boot.
    pub attachment: Attachment,
    /// The attached machine's base URL, absent when attached locally.
    pub base_url: Option<String>,
}

/// The persisted address, exactly as it sits on disk.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredAddress {
    base_url: String,
}

/// The shell's remote-attachment state: the address on disk, the token in the
/// OS credential store, and a cached read of both.
pub struct RemoteAttachment {
    address_path: PathBuf,
    secrets: Arc<dyn SecretProvider>,
    /// `None` until first read; `Some(None)` means "read, and not attached".
    cached: RwLock<Option<Option<Attached>>>,
}

/// A live remote attachment: address plus the token that reaches it.
#[derive(Clone, Debug)]
pub struct Attached {
    pub base_url: String,
    pub token: String,
}

impl RemoteAttachment {
    /// Build the attachment over a profile data dir and a keychain service.
    ///
    /// The service is the channel-scoped one the embedded server uses, so a
    /// staging build never reads the release build's remote token.
    pub fn new(data_dir: &Path, keychain_service: Option<&str>) -> Self {
        let secrets: Arc<dyn SecretProvider> = Arc::new(match keychain_service {
            Some(service) => KeychainSecretProvider::with_service(service),
            None => KeychainSecretProvider::new(),
        });
        Self::with_secrets(data_dir, secrets)
    }

    fn with_secrets(data_dir: &Path, secrets: Arc<dyn SecretProvider>) -> Self {
        Self {
            address_path: data_dir.join(ADDRESS_FILE),
            secrets,
            cached: RwLock::new(None),
        }
    }

    /// The current attachment, reading disk and keychain at most once.
    ///
    /// An address with no token behind it reads as *not attached*: the token is
    /// what makes the address usable, and half an attachment would boot the
    /// renderer against a machine it cannot authenticate to.
    pub async fn current(&self) -> Option<Attached> {
        if let Some(cached) = self.cached.read().await.as_ref() {
            return cached.clone();
        }
        let resolved = self.read_through().await;
        *self.cached.write().await = Some(resolved.clone());
        resolved
    }

    async fn read_through(&self) -> Option<Attached> {
        let bytes = std::fs::read(&self.address_path).ok()?;
        let stored: StoredAddress = serde_json::from_slice(&bytes).ok()?;
        let token = self.secrets.get_secret(TOKEN_KEY).await.ok()??;
        Some(Attached {
            base_url: stored.base_url,
            token,
        })
    }

    /// What the renderer should show.
    pub async fn state(&self) -> RemoteMachineState {
        match self.current().await {
            Some(attached) => RemoteMachineState {
                attachment: Attachment::Remote,
                base_url: Some(attached.base_url),
            },
            None => RemoteMachineState {
                attachment: Attachment::Local,
                base_url: None,
            },
        }
    }

    /// Validate, probe, and store an attachment to `base_url`.
    ///
    /// Nothing is written until the machine answers the probe, so a refused
    /// connect leaves the previous attachment exactly as it was.
    pub async fn connect(
        &self,
        base_url: &str,
        token: &str,
    ) -> Result<RemoteMachineState, RemoteConnectError> {
        let base_url = validated_base_url(base_url)?;
        let token = token.trim();
        if token.is_empty() {
            return Err(RemoteConnectError::new(REASON_TOKEN_REFUSED));
        }
        probe(&base_url, token).await?;
        self.store(&base_url, token).await?;
        Ok(RemoteMachineState {
            attachment: Attachment::Remote,
            base_url: Some(base_url),
        })
    }

    async fn store(&self, base_url: &str, token: &str) -> Result<(), RemoteConnectError> {
        self.secrets
            .set_secret(TOKEN_KEY, token)
            .await
            .map_err(|error| RemoteConnectError::detailed(REASON_TOKEN_STORAGE_FAILED, error))?;
        let body = serde_json::to_vec_pretty(&StoredAddress {
            base_url: base_url.to_owned(),
        })
        .expect("a struct of owned strings serializes");
        // Token first, address second: a crash between the two leaves an
        // orphan token, which reads as "not attached". The reverse order
        // would leave an address with no token, which reads the same way but
        // after a failed boot instead of before one.
        std::fs::write(&self.address_path, body)
            .map_err(|error| RemoteConnectError::detailed(REASON_TOKEN_STORAGE_FAILED, error))?;
        *self.cached.write().await = Some(Some(Attached {
            base_url: base_url.to_owned(),
            token: token.to_owned(),
        }));
        Ok(())
    }

    /// Drop the attachment and forget the token. Returns the local state.
    pub async fn disconnect(&self) -> RemoteMachineState {
        // Best-effort on both sides: a removal that fails must still leave the
        // app detached rather than stuck attached to a machine the user asked
        // to leave. The address file is what `current` reads first, so
        // removing it is what actually detaches.
        let _ = std::fs::remove_file(&self.address_path);
        let _ = self.secrets.delete_secret(TOKEN_KEY).await;
        *self.cached.write().await = Some(None);
        RemoteMachineState {
            attachment: Attachment::Local,
            base_url: None,
        }
    }
}

/// Normalize a user-entered base URL, or say why it cannot be used.
///
/// Mirrors the provision-link rule in [`crate::deep_link`]: `https` everywhere
/// except loopback, no credentials in the URL, no query or fragment. A base URL
/// may carry a path prefix, because a machine can sit behind a fronting proxy
/// that mounts it under one.
pub fn validated_base_url(raw: &str) -> Result<String, RemoteConnectError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(RemoteConnectError::new(REASON_URL_INVALID));
    }
    let url = tauri::Url::parse(raw)
        .map_err(|error| RemoteConnectError::detailed(REASON_URL_INVALID, error))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(RemoteConnectError::new(REASON_URL_INVALID));
    }
    if url.host_str().is_none() {
        return Err(RemoteConnectError::new(REASON_URL_INVALID));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(RemoteConnectError::new(REASON_URL_INVALID));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(RemoteConnectError::new(REASON_URL_INVALID));
    }
    if url.scheme() == "http" && !url_host_is_loopback(&url) {
        return Err(RemoteConnectError::new(REASON_REQUIRES_TLS));
    }
    Ok(normalized(&url))
}

/// The URL as this client will use it: no trailing slash, so route paths
/// concatenate onto it directly.
fn normalized(url: &tauri::Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

/// Whether a URL's host is this machine.
///
/// `localhost` counts because the resolver is required to map it to a loopback
/// address; other names do not, since what they resolve to is not knowable
/// here. Shared with the provision-link check so the cleartext exception has
/// one definition.
pub fn url_host_is_loopback(url: &tauri::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

/// Ask the machine whether it is there and whether it accepts this token.
async fn probe(base_url: &str, token: &str) -> Result<(), RemoteConnectError> {
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .expect("fixed HTTP client configuration is valid");
    let response = client
        .get(format!("{base_url}{PROBE_PATH}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| RemoteConnectError::detailed(REASON_UNREACHABLE, error))?;
    classify_probe(response.status().as_u16())
}

/// Turn the probe's status into a stable reason.
fn classify_probe(status: u16) -> Result<(), RemoteConnectError> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(RemoteConnectError::new(REASON_TOKEN_REFUSED)),
        other => Err(RemoteConnectError::detailed(
            REASON_NOT_A_MACHINE,
            format!("status {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(raw: &str) -> &'static str {
        validated_base_url(raw)
            .expect_err("expected a refusal")
            .reason
    }

    #[test]
    fn https_is_accepted_and_normalized() {
        assert_eq!(
            validated_base_url("  https://machine.example.com/  ").unwrap(),
            "https://machine.example.com"
        );
        assert_eq!(
            validated_base_url("https://proxy.example.com/tidebreak/").unwrap(),
            "https://proxy.example.com/tidebreak"
        );
    }

    /// The rule this mode exists to hold: cleartext reaches this machine only.
    #[test]
    fn cleartext_is_refused_off_loopback() {
        assert_eq!(reason("http://machine.example.com"), REASON_REQUIRES_TLS);
        assert_eq!(reason("http://10.0.0.4:8080"), REASON_REQUIRES_TLS);
        // A name that merely looks local is not local; what it resolves to is
        // not knowable here.
        assert_eq!(reason("http://localhost.example.com"), REASON_REQUIRES_TLS);
    }

    #[test]
    fn cleartext_loopback_is_the_developer_exception() {
        for raw in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(
                validated_base_url(raw).is_ok(),
                "{raw} should be accepted on loopback"
            );
        }
    }

    #[test]
    fn unusable_addresses_are_refused_before_any_probe() {
        assert_eq!(reason(""), REASON_URL_INVALID);
        assert_eq!(reason("machine.example.com"), REASON_URL_INVALID);
        assert_eq!(reason("ftp://machine.example.com"), REASON_URL_INVALID);
        assert_eq!(
            reason("https://user:pw@machine.example.com"),
            REASON_URL_INVALID
        );
        assert_eq!(
            reason("https://machine.example.com?token=abc"),
            REASON_URL_INVALID
        );
        assert_eq!(
            reason("https://machine.example.com#fragment"),
            REASON_URL_INVALID
        );
    }

    #[test]
    fn probe_statuses_map_to_stable_reasons() {
        assert!(classify_probe(200).is_ok());
        assert_eq!(
            classify_probe(401).unwrap_err().reason,
            REASON_TOKEN_REFUSED
        );
        assert_eq!(
            classify_probe(403).unwrap_err().reason,
            REASON_TOKEN_REFUSED
        );
        assert_eq!(
            classify_probe(404).unwrap_err().reason,
            REASON_NOT_A_MACHINE
        );
        assert_eq!(
            classify_probe(502).unwrap_err().reason,
            REASON_NOT_A_MACHINE
        );
    }

    #[tokio::test]
    async fn an_address_without_a_token_reads_as_not_attached() {
        let dir = tempfile::tempdir().unwrap();
        let attachment =
            RemoteAttachment::with_secrets(dir.path(), Arc::new(TestSecrets::default()));
        std::fs::write(
            dir.path().join(ADDRESS_FILE),
            br#"{"baseUrl":"https://machine.example.com"}"#,
        )
        .unwrap();
        assert!(attachment.current().await.is_none());
        assert_eq!(attachment.state().await.attachment, Attachment::Local);
    }

    #[tokio::test]
    async fn disconnect_forgets_both_halves() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(TestSecrets::default());
        let attachment = RemoteAttachment::with_secrets(dir.path(), secrets.clone());
        attachment
            .store("https://machine.example.com", "token-value")
            .await
            .unwrap();
        assert_eq!(
            attachment.current().await.unwrap().base_url,
            "https://machine.example.com"
        );

        attachment.disconnect().await;
        assert!(attachment.current().await.is_none());
        assert!(!dir.path().join(ADDRESS_FILE).exists());
        assert!(secrets.get_secret(TOKEN_KEY).await.unwrap().is_none());
    }

    #[derive(Default)]
    struct TestSecrets {
        entries: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    #[async_trait::async_trait]
    impl SecretProvider for TestSecrets {
        async fn get_secret(&self, key: &str) -> tidebreak_core::Result<Option<String>> {
            Ok(self.entries.lock().unwrap().get(key).cloned())
        }

        async fn set_secret(&self, key: &str, value: &str) -> tidebreak_core::Result<()> {
            self.entries
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
            Ok(())
        }

        async fn delete_secret(&self, key: &str) -> tidebreak_core::Result<()> {
            self.entries.lock().unwrap().remove(key);
            Ok(())
        }
    }
}
