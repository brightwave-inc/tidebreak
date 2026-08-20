//! Remote connection mode: attaching this client to a machine it does not host.
//!
//! A machine is a running `tidebreak-server`. Normally the desktop app is both
//! machine and client: the shell boots the embedded server on loopback and the
//! webview attaches to it. Decision record 47 adds the other shape — the same
//! webview attached to a server somewhere else. Gateway-backed machines reuse
//! the desktop's existing OAuth session; standalone machines still accept a
//! base URL and a token the user supplies.
//!
//! Two rules govern the address:
//!
//! - **TLS unless loopback.** Every attachment presents a bearer credential,
//!   so cleartext to anywhere but this machine hands it to whoever is on the
//!   path. This is the same line
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

use tidebreak_core::config::tidebreak_machine_resource;
use tidebreak_core::keychain::KeychainSecretProvider;
use tidebreak_core::storage::SecretProvider;

/// Keychain key holding the bearer token for a legacy static-token machine.
const TOKEN_KEY: &str = "desktop.remote-machine.token";

/// File under the profile data dir holding the attached machine's address.
/// The address is not a secret; the token that reaches it is, and lives in the
/// OS credential store instead.
const ADDRESS_FILE: &str = "remote-machine.json";

/// Route the connect probe calls. Cheap, authenticated, and present on every
/// profile, so a 2xx proves both reachability and a token the machine accepts.
const PROBE_PATH: &str = "/policy";
/// Public machine metadata used to select Gateway-backed authentication.
const DISCOVERY_PATH: &str = "/auth/discovery";

/// How long the connect probe waits before calling a machine unreachable.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Discovery is a tiny fixed-shape document. A hard cap prevents a machine
/// endpoint from turning that unauthenticated read into an unbounded buffer.
const MAX_DISCOVERY_RESPONSE_BYTES: usize = 16 * 1024;

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
/// The machine is not Gateway-backed, or it names a different Gateway than
/// the one managing this desktop profile.
pub const REASON_GATEWAY_AUTH_UNAVAILABLE: &str = "remote_machine_gateway_auth_unavailable";

impl RemoteConnectError {
    pub(crate) fn new(reason: &'static str) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    pub(crate) fn detailed(reason: &'static str, detail: impl std::fmt::Display) -> Self {
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
    #[serde(default)]
    auth: StoredAuth,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum StoredAuth {
    #[default]
    StaticToken,
    Gateway {
        gateway_url: String,
    },
}

/// The shell's remote-attachment state: public connection metadata on disk,
/// the optional legacy static token in the OS credential store, and a cached
/// read of both.
pub struct RemoteAttachment {
    address_path: PathBuf,
    secrets: Arc<dyn SecretProvider>,
    /// `None` until first read; `Some(None)` means "read, and not attached".
    cached: RwLock<Option<Option<Attached>>>,
}

/// A live remote attachment: address plus the selected authentication source.
#[derive(Clone, Debug)]
pub struct Attached {
    pub base_url: String,
    pub auth: AttachedAuth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachedAuth {
    StaticToken(String),
    Gateway { gateway_url: String },
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
    /// A legacy static-token address with no token behind it reads as *not
    /// attached*. Gateway attachments need only their public Gateway URL; the
    /// short-lived bearer is minted from the existing OAuth session at use.
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
        let auth = match stored.auth {
            StoredAuth::StaticToken => {
                AttachedAuth::StaticToken(self.secrets.get_secret(TOKEN_KEY).await.ok()??)
            }
            StoredAuth::Gateway { gateway_url } => AttachedAuth::Gateway { gateway_url },
        };
        Some(Attached {
            base_url: stored.base_url,
            auth,
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
        self.store_static(&base_url, token).await?;
        Ok(RemoteMachineState {
            attachment: Attachment::Remote,
            base_url: Some(base_url),
        })
    }

    async fn store_static(&self, base_url: &str, token: &str) -> Result<(), RemoteConnectError> {
        self.secrets
            .set_secret(TOKEN_KEY, token)
            .await
            .map_err(|error| RemoteConnectError::detailed(REASON_TOKEN_STORAGE_FAILED, error))?;
        let body = serde_json::to_vec_pretty(&StoredAddress {
            base_url: base_url.to_owned(),
            auth: StoredAuth::StaticToken,
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
            auth: AttachedAuth::StaticToken(token.to_owned()),
        }));
        Ok(())
    }

    /// Attach with a short-lived token minted from this desktop's existing
    /// Model Gateway session. Only the machine and Gateway URLs are persisted;
    /// the user token is refreshed from the OAuth session and never becomes a
    /// second long-lived credential in the keychain.
    pub async fn connect_gateway(
        &self,
        base_url: &str,
        gateway_url: &str,
        token: &str,
    ) -> Result<RemoteMachineState, RemoteConnectError> {
        let base_url = validated_base_url(base_url)?;
        let gateway_url = validated_gateway_url(gateway_url)?;
        probe(&base_url, token).await?;
        let body = serde_json::to_vec_pretty(&StoredAddress {
            base_url: base_url.clone(),
            auth: StoredAuth::Gateway {
                gateway_url: gateway_url.clone(),
            },
        })
        .expect("a struct of owned strings serializes");
        std::fs::write(&self.address_path, body)
            .map_err(|error| RemoteConnectError::detailed(REASON_TOKEN_STORAGE_FAILED, error))?;
        let _ = self.secrets.delete_secret(TOKEN_KEY).await;
        *self.cached.write().await = Some(Some(Attached {
            base_url: base_url.clone(),
            auth: AttachedAuth::Gateway { gateway_url },
        }));
        Ok(RemoteMachineState {
            attachment: Attachment::Remote,
            base_url: Some(base_url),
        })
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

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum AuthDiscovery {
    Gateway {
        gateway_url: String,
        resource: String,
    },
    StaticToken,
    Local,
}

/// Discover the Gateway identity authority a hosted machine is configured to
/// use. This endpoint carries no credential and returns no user data.
///
/// The machine audience is derived locally from the canonical validated base
/// URL. Discovery may echo it for diagnostics, but cannot choose it.
pub async fn discover_gateway(
    base_url: &str,
) -> Result<(String, String, String), RemoteConnectError> {
    let base_url = validated_base_url(base_url)?;
    let resource = tidebreak_machine_resource(&base_url);
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("fixed HTTP client configuration is valid");
    let response = client
        .get(format!("{base_url}{DISCOVERY_PATH}"))
        .send()
        .await
        .map_err(|error| RemoteConnectError::detailed(REASON_UNREACHABLE, error))?;
    if !response.status().is_success() {
        return Err(RemoteConnectError::detailed(
            REASON_NOT_A_MACHINE,
            format!("status {}", response.status()),
        ));
    }
    let body = read_discovery_body(response).await?;
    let discovery: AuthDiscovery = serde_json::from_slice(&body)
        .map_err(|error| RemoteConnectError::detailed(REASON_NOT_A_MACHINE, error))?;
    match discovery {
        AuthDiscovery::Gateway {
            gateway_url,
            resource: advertised,
        } if advertised == resource => {
            Ok((base_url, validated_gateway_url(&gateway_url)?, resource))
        }
        AuthDiscovery::Gateway { .. } | AuthDiscovery::StaticToken | AuthDiscovery::Local => {
            Err(RemoteConnectError::new(REASON_GATEWAY_AUTH_UNAVAILABLE))
        }
    }
}

async fn read_discovery_body(response: reqwest::Response) -> Result<Vec<u8>, RemoteConnectError> {
    use futures::StreamExt as _;

    let too_large = || {
        RemoteConnectError::detailed(
            REASON_NOT_A_MACHINE,
            "discovery response exceeded its byte budget",
        )
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DISCOVERY_RESPONSE_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| RemoteConnectError::detailed(REASON_NOT_A_MACHINE, error))?;
        if body.len().saturating_add(chunk.len()) > MAX_DISCOVERY_RESPONSE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
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

fn validated_gateway_url(raw: &str) -> Result<String, RemoteConnectError> {
    let url = tauri::Url::parse(raw.trim())
        .map_err(|error| RemoteConnectError::detailed(REASON_GATEWAY_AUTH_UNAVAILABLE, error))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() == "http" && !url_host_is_loopback(&url))
    {
        return Err(RemoteConnectError::new(REASON_GATEWAY_AUTH_UNAVAILABLE));
    }
    Ok(normalized(&url))
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
        .redirect(reqwest::redirect::Policy::none())
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
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    async fn serve_once(
        response_for_base_url: impl FnOnce(&str) -> Vec<u8>,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let response = response_for_base_url(&base_url);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(&response).await.unwrap();
            request
        });
        (base_url, task)
    }

    fn ok_json(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

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

    #[test]
    fn machine_resource_uses_the_canonical_validated_base_url() {
        let canonical = validated_base_url("https://machine.example.com/").unwrap();
        assert_eq!(canonical, "https://machine.example.com");
        assert_eq!(
            tidebreak_machine_resource(&canonical),
            "tidebreak:46e3f66ef15f895d5561433564bfd02ab4b0dee3d3ef535d714a1ea03c7729a3"
        );
        assert_eq!(
            tidebreak_machine_resource(&canonical),
            tidebreak_machine_resource(
                &validated_base_url(" https://machine.example.com ").unwrap()
            )
        );
    }

    #[tokio::test]
    async fn discovery_must_echo_the_independently_derived_machine_resource() {
        let (base_url, served) = serve_once(|base_url| {
            let body = serde_json::json!({
                "mode": "gateway",
                "gateway_url": "https://gateway.example.com",
                "resource": tidebreak_machine_resource(base_url),
            })
            .to_string();
            ok_json(&body)
        })
        .await;
        let expected_resource = tidebreak_machine_resource(&base_url);
        assert_eq!(
            discover_gateway(&base_url).await.unwrap(),
            (
                base_url.clone(),
                "https://gateway.example.com".to_owned(),
                expected_resource,
            )
        );
        served.await.unwrap();

        let (base_url, served) = serve_once(|_| {
            ok_json(
                &serde_json::json!({
                    "mode": "gateway",
                    "gateway_url": "https://gateway.example.com",
                    "resource": format!("tidebreak:{}", "0".repeat(64)),
                })
                .to_string(),
            )
        })
        .await;
        assert_eq!(
            discover_gateway(&base_url).await.unwrap_err().reason,
            REASON_GATEWAY_AUTH_UNAVAILABLE
        );
        served.await.unwrap();
    }

    #[tokio::test]
    async fn discovery_body_is_bounded_with_or_without_content_length() {
        let declared = MAX_DISCOVERY_RESPONSE_BYTES + 1;
        let (base_url, served) = serve_once(move |_| {
            format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n")
                .into_bytes()
        })
        .await;
        assert_eq!(
            discover_gateway(&base_url).await.unwrap_err().reason,
            REASON_NOT_A_MACHINE
        );
        served.await.unwrap();

        let oversized = vec![b'x'; MAX_DISCOVERY_RESPONSE_BYTES + 1];
        let (base_url, served) = serve_once(move |_| {
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                oversized.len()
            )
            .into_bytes();
            response.extend_from_slice(&oversized);
            response.extend_from_slice(b"\r\n0\r\n\r\n");
            response
        })
        .await;
        assert_eq!(
            discover_gateway(&base_url).await.unwrap_err().reason,
            REASON_NOT_A_MACHINE
        );
        served.await.unwrap();
    }

    #[tokio::test]
    async fn discovery_and_bearer_probe_refuse_redirects() {
        let discovery_target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let discovery_target_url = format!("http://{}", discovery_target.local_addr().unwrap());
        let (base_url, served) = serve_once(move |_| {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {discovery_target_url}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes()
        })
        .await;
        assert_eq!(
            discover_gateway(&base_url).await.unwrap_err().reason,
            REASON_NOT_A_MACHINE
        );
        served.await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                discovery_target.accept()
            )
            .await
            .is_err(),
            "discovery followed a redirect"
        );

        let probe_target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let probe_target_url = format!("http://{}", probe_target.local_addr().unwrap());
        let (base_url, served) = serve_once(move |_| {
            format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {probe_target_url}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes()
        })
        .await;
        assert_eq!(
            probe(&base_url, "machine-bearer").await.unwrap_err().reason,
            REASON_NOT_A_MACHINE
        );
        let request = String::from_utf8(served.await.unwrap()).unwrap();
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer machine-bearer"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), probe_target.accept())
                .await
                .is_err(),
            "bearer-bearing policy probe followed a redirect"
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
    async fn legacy_address_json_still_loads_as_static_token_auth() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(TestSecrets::default());
        secrets.set_secret(TOKEN_KEY, "token-value").await.unwrap();
        let attachment = RemoteAttachment::with_secrets(dir.path(), secrets);
        std::fs::write(
            dir.path().join(ADDRESS_FILE),
            br#"{"baseUrl":"https://machine.example.com"}"#,
        )
        .unwrap();

        let attached = attachment.current().await.unwrap();
        assert_eq!(attached.base_url, "https://machine.example.com");
        assert_eq!(
            attached.auth,
            AttachedAuth::StaticToken("token-value".to_owned())
        );
    }

    #[tokio::test]
    async fn gateway_attachment_loads_without_a_persisted_access_token() {
        let dir = tempfile::tempdir().unwrap();
        let attachment =
            RemoteAttachment::with_secrets(dir.path(), Arc::new(TestSecrets::default()));
        std::fs::write(
            dir.path().join(ADDRESS_FILE),
            br#"{
  "baseUrl": "https://machine.example.com",
  "auth": {
    "mode": "gateway",
    "gateway_url": "https://gateway.example.com"
  }
}"#,
        )
        .unwrap();

        let attached = attachment.current().await.unwrap();
        assert_eq!(attached.base_url, "https://machine.example.com");
        assert_eq!(
            attached.auth,
            AttachedAuth::Gateway {
                gateway_url: "https://gateway.example.com".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn disconnect_forgets_both_halves() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(TestSecrets::default());
        let attachment = RemoteAttachment::with_secrets(dir.path(), secrets.clone());
        attachment
            .store_static("https://machine.example.com", "token-value")
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
