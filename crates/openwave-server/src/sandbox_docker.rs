//! A [`SandboxBackend`] over the Docker CLI: provision a local container, resolve
//! it to a loopback address, and tear it down idempotently, plus a correlation-tag
//! orphan sweep that reclaims containers whose provisioning intent has no live run.
//!
//! # Placement
//!
//! This lives in `openwave-server` rather than in `openwave-sandbox-protocol`: the
//! protocol crate deliberately defines only the wire types and their semantics and
//! carries no transport or process dependency, while this adapter shells out to the
//! `docker` CLI. The server already owns the durable provisioning intents this sweep
//! reconciles against, and already depends on the protocol crate.
//!
//! # Scope
//!
//! This slice is the container *lifecycle* only. It stands a container up, publishes
//! its listener port to a loopback host port, and reports the reachable base URL.
//! The sandbox-agent wire transport that dials that URL — framing, the attach
//! handshake, the event stream — is a separate slice; nothing here speaks it.
//!
//! # Detected capability
//!
//! The container runtime is a detected capability, exactly as Seatbelt is for local
//! code execution: [`DockerSandboxBackend::is_available`] reports whether the runtime
//! binary is resolvable, and the host consults it before choosing this backend. When
//! the binary is absent the trait methods fail with a normalized [`BackendError`]
//! rather than panicking — the runtime is never a hard dependency. A Docker-CLI
//! compatible runtime such as `podman` works through the same code path by pointing
//! [`DockerConfig::binary`] at it.
//!
//! # The transport secret
//!
//! [`SandboxAddress`] bundles a per-run [`TransportSecret`]. The design has the host
//! mint that secret and deliver it through the provider's control plane, but the
//! provisioning path does not carry it yet, so this adapter mints it at
//! [`provision`](SandboxBackend::provision), injects it into the container's
//! environment (the local control plane is Docker itself), and recovers it in
//! [`address`](SandboxBackend::address). This keeps `address` a pure function of the
//! handle — it survives a host restart, because everything it needs lives in Docker's
//! own state — and the host-minted delivery wiring is a later slice's concern.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use openwave_sandbox_protocol::{
    BackendError, ProvisionRequest, RunId, SandboxAddress, SandboxBackend, SandboxHandle,
    SandboxTag, TransportSecret,
};
use tokio::process::Command;
use uuid::Uuid;

/// Label key stamping the host-minted correlation tag onto a provisioned container.
/// The orphan sweep filters on this label and reads the tag back from it.
pub const RUN_TAG_LABEL: &str = "openwave.run-tag";
/// Informational label recording which run a container serves.
pub const RUN_ID_LABEL: &str = "openwave.run-id";
/// Environment variable carrying the per-run transport secret into the container.
const TRANSPORT_SECRET_ENV: &str = "OPENWAVE_TRANSPORT_SECRET";
/// Environment variable carrying the delegated task into the container. The
/// agent image reads its task from here; see [`ProvisionRequest::task`] for why
/// this rides provisioning rather than a run-init frame today.
const TASK_ENV: &str = "OPENWAVE_SANDBOX_TASK";
/// Go-template that lists a tagged container's id and correlation tag, tab-separated.
/// Kept in sync with [`RUN_TAG_LABEL`] by a unit test.
const TAG_LIST_FORMAT: &str = "{{.ID}}\t{{.Label \"openwave.run-tag\"}}";

/// The default runtime binary, resolved on `PATH`.
const DEFAULT_BINARY: &str = "docker";
/// A documented placeholder image. The real agent image (loop plus supervisor) ships
/// in a later slice; a container from this ref only needs to start and hold its port.
const DEFAULT_IMAGE: &str = "ghcr.io/openwave/sandbox-agent:placeholder";
/// The in-container port the sandbox supervisor listens on by default.
const DEFAULT_LISTENER_PORT: u16 = 8080;

const RUNTIME_UNAVAILABLE: &str = "container runtime is unavailable";
const RUNTIME_SPAWN_FAILED: &str = "could not invoke the container runtime";
const RUN_FAILED: &str = "could not start the sandbox container";
const PORT_LOOKUP_FAILED: &str = "could not resolve the sandbox's published port";
const INSPECT_FAILED: &str = "could not inspect the sandbox container";
const REMOVE_FAILED: &str = "could not remove the sandbox container";
const LIST_FAILED: &str = "could not list sandbox containers";

/// How this backend drives the container runtime.
#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// The runtime binary. `docker` by default; point it at `podman` for the
    /// Docker-CLI-compatible path. Either a bare name resolved on `PATH` or an
    /// explicit path.
    pub binary: String,
    /// The image ref to run. Defaults to a documented placeholder.
    pub image: String,
    /// The in-container port to publish to a loopback host port.
    pub listener_port: u16,
    /// A command (and arguments) to run in the container, overriding the image's own
    /// entrypoint. Empty means the image's default is used — the real agent image
    /// needs no override; tests pass a trivial port-holding command.
    pub command: Vec<String>,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            binary: DEFAULT_BINARY.to_owned(),
            image: DEFAULT_IMAGE.to_owned(),
            listener_port: DEFAULT_LISTENER_PORT,
            command: Vec::new(),
        }
    }
}

/// A [`SandboxBackend`] backed by a local Docker (or podman) container.
pub struct DockerSandboxBackend {
    config: DockerConfig,
}

impl DockerSandboxBackend {
    /// Build a backend from explicit configuration.
    #[must_use]
    pub fn new(config: DockerConfig) -> Self {
        Self { config }
    }

    /// Build a backend with the default configuration (the `docker` binary and the
    /// placeholder image).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DockerConfig::default())
    }

    /// Whether the configured container runtime binary is resolvable on this host.
    ///
    /// This is the detected-capability seam: the host checks it before choosing this
    /// backend, exactly as it checks Seatbelt support before the local exec adapter.
    #[must_use]
    pub fn is_available(&self) -> bool {
        resolve_on_path(&self.config.binary).is_some()
    }

    /// Reclaim orphaned containers: destroy every tagged container whose correlation
    /// tag is *not* in `live_tags`.
    ///
    /// The reclaimable predicate is exactly "the container's stamped `openwave.run-tag`
    /// names no live run". The host builds `live_tags` from its durable provisioning
    /// intents and committed handles; a tagged container the host does not recognize
    /// belongs to a lapsed intent — a run that never committed a handle, whether the
    /// host crashed before or after the `docker run` returned — so it is reclaimed.
    /// This mirrors the durable op-log's tag sweep, and it is idempotent: a second
    /// sweep with the same live set finds the reclaimed containers already gone and
    /// reclaims nothing. A container whose removal fails stays listed and is re-driven
    /// by the next sweep rather than being abandoned.
    ///
    /// # Errors
    /// [`BackendError::Teardown`] if the runtime cannot be asked to list containers.
    pub async fn reclaim_orphans(
        &self,
        live_tags: &HashSet<SandboxTag>,
    ) -> Result<Vec<SandboxHandle>, BackendError> {
        if !self.is_available() {
            return Ok(Vec::new());
        }
        let mut reclaimed = Vec::new();
        for handle in self.list_tagged().await? {
            if live_tags.contains(&handle.tag) {
                continue;
            }
            if self.remove(&handle.reference).await.is_ok() {
                reclaimed.push(handle);
            }
        }
        Ok(reclaimed)
    }

    /// A fresh runtime command with stdio detached from this process.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.config.binary);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// `docker rm -f`, treating a missing container as success.
    async fn remove(&self, reference: &str) -> Result<(), BackendError> {
        let mut command = self.command();
        command.args(["rm", "-f", reference]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Teardown(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if output.status.success() || is_no_such_container(&output.stderr) {
            Ok(())
        } else {
            Err(BackendError::Teardown(REMOVE_FAILED.to_owned()))
        }
    }

    /// List every container carrying the correlation-tag label, with its tag parsed.
    async fn list_tagged(&self) -> Result<Vec<SandboxHandle>, BackendError> {
        let mut command = self.command();
        command.args([
            "ps",
            "-a",
            "--filter",
            &format!("label={RUN_TAG_LABEL}"),
            "--format",
            TAG_LIST_FORMAT,
        ]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Teardown(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return Err(BackendError::Teardown(LIST_FAILED.to_owned()));
        }
        Ok(parse_tag_listing(&output.stdout))
    }

    /// Recover the per-run transport secret from the container's environment.
    async fn read_transport_secret(&self, reference: &str) -> Result<String, BackendError> {
        let mut command = self.command();
        command.args(["inspect", "--format", "{{json .Config.Env}}", reference]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Unaddressable(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return if is_no_such_container(&output.stderr) {
                Err(BackendError::UnknownHandle)
            } else {
                Err(BackendError::Unaddressable(INSPECT_FAILED.to_owned()))
            };
        }
        let env: Vec<String> = serde_json::from_slice(&output.stdout)
            .map_err(|_| BackendError::Unaddressable(INSPECT_FAILED.to_owned()))?;
        let prefix = format!("{TRANSPORT_SECRET_ENV}=");
        env.iter()
            .find_map(|entry| entry.strip_prefix(&prefix))
            .map(str::to_owned)
            .ok_or_else(|| BackendError::Unaddressable(INSPECT_FAILED.to_owned()))
    }
}

#[async_trait]
impl SandboxBackend for DockerSandboxBackend {
    async fn provision(&self, request: ProvisionRequest) -> Result<SandboxHandle, BackendError> {
        if !self.is_available() {
            return Err(BackendError::Provision(RUNTIME_UNAVAILABLE.to_owned()));
        }
        // The design admits a local container attached-only precisely because Docker
        // has no lifetime cap it can enforce from outside the container, so
        // `lifetime_cap_secs` is not enforceable here and is intentionally ignored.
        let secret = Uuid::new_v4().to_string();
        let mut command = self.command();
        command.args(run_args(
            &self.config,
            request.tag,
            request.run_id,
            request.task.is_some(),
        ));
        // Set the secret on the runtime CLI's own environment and pass it through with
        // a valueless `--env`, so it never appears in this process's argv.
        command.env(TRANSPORT_SECRET_ENV, &secret);
        // The delegated task travels the same way: by name only, so task text
        // never lands in this process's argv or in `ps` output.
        if let Some(task) = &request.task {
            command.env(TASK_ENV, task);
        }
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Provision(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return Err(BackendError::Provision(RUN_FAILED.to_owned()));
        }
        let reference = parse_container_id(&output.stdout)
            .ok_or_else(|| BackendError::Provision(RUN_FAILED.to_owned()))?;
        Ok(SandboxHandle {
            reference,
            tag: request.tag,
        })
    }

    async fn address(&self, handle: &SandboxHandle) -> Result<SandboxAddress, BackendError> {
        if !self.is_available() {
            return Err(BackendError::Unaddressable(RUNTIME_UNAVAILABLE.to_owned()));
        }
        // Resolve the published host port from `docker inspect` rather than
        // `docker port`: inspect returns the whole port map as JSON (parseable and
        // unit-testable without a daemon), and it does not depend on `docker port`'s
        // line format. The publish is loopback-only, so the binding's host IP is
        // 127.0.0.1 and `base_url` names it directly.
        let mut command = self.command();
        command.args([
            "inspect",
            "--format",
            "{{json .NetworkSettings.Ports}}",
            &handle.reference,
        ]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Unaddressable(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return if is_no_such_container(&output.stderr) {
                Err(BackendError::UnknownHandle)
            } else {
                Err(BackendError::Unaddressable(PORT_LOOKUP_FAILED.to_owned()))
            };
        }
        let port = parse_inspect_ports(&output.stdout, self.config.listener_port)
            .ok_or_else(|| BackendError::Unaddressable(PORT_LOOKUP_FAILED.to_owned()))?;
        let secret = self.read_transport_secret(&handle.reference).await?;
        Ok(SandboxAddress {
            base_url: base_url(port),
            transport_secret: TransportSecret::new(secret),
        })
    }

    async fn destroy(&self, handle: &SandboxHandle) -> Result<(), BackendError> {
        if !self.is_available() {
            return Err(BackendError::Teardown(RUNTIME_UNAVAILABLE.to_owned()));
        }
        self.remove(&handle.reference).await
    }
}

/// The `docker run` argument vector for one provisioning. Factored out so the
/// label, publish, and env-passthrough composition is testable without a runtime.
fn run_args(config: &DockerConfig, tag: SandboxTag, run_id: RunId, with_task: bool) -> Vec<String> {
    let mut args = vec![
        "run".to_owned(),
        "-d".to_owned(),
        "--label".to_owned(),
        format!("{RUN_TAG_LABEL}={tag}"),
        "--label".to_owned(),
        format!("{RUN_ID_LABEL}={run_id}"),
        "--env".to_owned(),
        TRANSPORT_SECRET_ENV.to_owned(),
    ];
    if with_task {
        args.push("--env".to_owned());
        args.push(TASK_ENV.to_owned());
    }
    args.extend([
        "--publish".to_owned(),
        format!("127.0.0.1::{}", config.listener_port),
        config.image.clone(),
    ]);
    args.extend(config.command.iter().cloned());
    args
}

/// The loopback base URL for a published host port.
fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The container id printed by `docker run -d`: the first non-empty hex line.
fn parse_container_id(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let id = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    (id.len() >= 12 && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| id.to_owned())
}

/// One host-side binding of a container port, as `docker inspect` renders it.
#[derive(serde::Deserialize)]
struct PortBinding {
    #[serde(rename = "HostIp")]
    host_ip: Option<String>,
    #[serde(rename = "HostPort")]
    host_port: Option<String>,
}

/// The published host port for `container_port` from the JSON `docker inspect
/// {{json .NetworkSettings.Ports}}` renders, e.g.
/// `{"8080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49155"}]}`. A container that
/// publishes no such port renders the key as `null` (or omits it), which yields
/// `None`. A loopback binding is preferred so the resolved port matches the
/// loopback `base_url`.
fn parse_inspect_ports(stdout: &[u8], container_port: u16) -> Option<u16> {
    let ports: std::collections::HashMap<String, Option<Vec<PortBinding>>> =
        serde_json::from_slice(stdout).ok()?;
    let bindings = ports.get(&format!("{container_port}/tcp"))?.as_ref()?;
    let binding = bindings
        .iter()
        .find(|binding| is_loopback(binding.host_ip.as_deref()))
        .or_else(|| bindings.first())?;
    binding
        .host_port
        .as_deref()?
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
}

/// Whether a binding's host IP is a loopback address.
fn is_loopback(host_ip: Option<&str>) -> bool {
    matches!(host_ip, Some("127.0.0.1" | "::1"))
}

/// Parse the id/tag pairs from the tag-listing template's tab-separated output.
fn parse_tag_listing(stdout: &[u8]) -> Vec<SandboxHandle> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .filter_map(|line| {
            let (id, tag) = line.trim().split_once('\t')?;
            let (id, tag) = (id.trim(), tag.trim());
            if id.is_empty() {
                return None;
            }
            // A tag that does not parse as a valid identity is a label this backend
            // did not mint; leave it alone rather than reclaim something foreign.
            let tag = tag.parse::<SandboxTag>().ok()?;
            Some(SandboxHandle {
                reference: id.to_owned(),
                tag,
            })
        })
        .collect()
}

/// Whether a runtime error names a container that does not exist — the idempotent
/// case for destroy, and the unknown-handle case for address. Covers both Docker
/// (`No such container`) and podman (`no such container` / `no such object`).
fn is_no_such_container(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    text.contains("no such container") || text.contains("no such object")
}

/// Resolve a runtime binary the way a shell would: an explicit path is checked as
/// given, a bare name is searched on `PATH`. Returns the resolved path if it names
/// an executable file.
fn resolve_on_path(binary: &str) -> Option<PathBuf> {
    let candidate = Path::new(binary);
    if candidate.components().count() > 1 {
        return is_executable_file(candidate).then(|| candidate.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        let full = dir.join(binary);
        is_executable_file(&full).then_some(full)
    })
}

/// Whether `path` is a regular file with an executable bit (on unix) or simply a
/// file (elsewhere).
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend pointed at a runtime binary that cannot exist.
    fn unavailable_backend() -> DockerSandboxBackend {
        DockerSandboxBackend::new(DockerConfig {
            binary: "openwave-nonexistent-runtime-xyzzy".to_owned(),
            ..DockerConfig::default()
        })
    }

    #[test]
    fn detection_reports_unavailable_when_the_binary_is_absent() {
        assert!(!unavailable_backend().is_available());
        // A real shell builtin path that does exist resolves; `/bin/sh` stands in
        // for a present runtime binary so the positive branch is covered too.
        #[cfg(unix)]
        assert!(resolve_on_path("/bin/sh").is_some());
    }

    #[tokio::test]
    async fn trait_methods_fail_normalized_when_the_runtime_is_absent() {
        let backend = unavailable_backend();
        let request = ProvisionRequest {
            run_id: RunId::new(),
            tag: SandboxTag::new(),
            lifetime_cap_secs: None,
            task: None,
        };
        assert!(matches!(
            backend.provision(request).await,
            Err(BackendError::Provision(_))
        ));
        let handle = SandboxHandle {
            reference: "deadbeefcafe".to_owned(),
            tag: SandboxTag::new(),
        };
        assert!(matches!(
            backend.address(&handle).await,
            Err(BackendError::Unaddressable(_))
        ));
        assert!(matches!(
            backend.destroy(&handle).await,
            Err(BackendError::Teardown(_))
        ));
        // With no runtime there is nothing to sweep, and that is not an error.
        assert_eq!(
            backend
                .reclaim_orphans(&HashSet::new())
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn run_args_stamp_the_tag_publish_the_port_and_append_the_command() {
        let tag = SandboxTag::new();
        let run_id = RunId::new();
        let config = DockerConfig {
            listener_port: 9000,
            image: "example/image:tag".to_owned(),
            command: vec!["sleep".to_owned(), "infinity".to_owned()],
            ..DockerConfig::default()
        };
        let args = run_args(&config, tag, run_id, true);

        assert_eq!(args[0], "run");
        assert!(args.contains(&"-d".to_owned()));
        assert!(args.contains(&format!("{RUN_TAG_LABEL}={tag}")));
        assert!(args.contains(&format!("{RUN_ID_LABEL}={run_id}")));
        // The secret is passed by name only, never as an argv value.
        assert!(args.contains(&"--env".to_owned()));
        assert!(args.contains(&TRANSPORT_SECRET_ENV.to_owned()));
        assert!(args
            .iter()
            .all(|arg| !arg.contains(TRANSPORT_SECRET_ENV) || arg == TRANSPORT_SECRET_ENV));
        // The delegated task is passed by name only too, so task text never
        // reaches this process's argv.
        assert!(args.contains(&TASK_ENV.to_owned()));
        assert!(args
            .iter()
            .all(|arg| !arg.contains(TASK_ENV) || arg == TASK_ENV));
        assert!(args.contains(&"127.0.0.1::9000".to_owned()));
        // The image precedes the container command.
        let image_at = args
            .iter()
            .position(|arg| arg == "example/image:tag")
            .unwrap();
        let command_at = args.iter().position(|arg| arg == "sleep").unwrap();
        assert!(image_at < command_at);
        assert_eq!(args.last().unwrap(), "infinity");

        // With no task to deliver the passthrough is omitted entirely, leaving
        // the image on its own default.
        let taskless = run_args(&config, tag, run_id, false);
        assert!(!taskless.contains(&TASK_ENV.to_owned()));
    }

    #[test]
    fn tag_list_format_tracks_the_label_constant() {
        assert!(TAG_LIST_FORMAT.contains(RUN_TAG_LABEL));
    }

    #[test]
    fn inspect_ports_resolve_the_published_host_port() {
        // The published-port shape `docker inspect {{json .NetworkSettings.Ports}}`
        // renders, with the loopback binding chosen.
        let published = br#"{"8080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49155"}]}"#;
        assert_eq!(parse_inspect_ports(published, 8080), Some(49_155));

        // Two bindings: the loopback one wins over a wildcard one.
        let dual = br#"{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"7000"},{"HostIp":"127.0.0.1","HostPort":"49200"}]}"#;
        assert_eq!(parse_inspect_ports(dual, 8080), Some(49_200));

        // An unpublished port renders as a null binding.
        let unbound = br#"{"8080/tcp":null}"#;
        assert_eq!(parse_inspect_ports(unbound, 8080), None);

        // A different container port than the one asked for.
        let other = br#"{"9090/tcp":[{"HostIp":"127.0.0.1","HostPort":"49155"}]}"#;
        assert_eq!(parse_inspect_ports(other, 8080), None);

        // A container with no published ports at all.
        assert_eq!(parse_inspect_ports(b"{}", 8080), None);
        // Malformed output resolves to nothing rather than panicking.
        assert_eq!(parse_inspect_ports(b"not json", 8080), None);
    }

    #[test]
    fn container_id_parsing_accepts_hex_and_rejects_noise() {
        assert_eq!(
            parse_container_id(b"3f2a9c1d4e5b6a7c8d9e0f1a2b3c4d5e\n").as_deref(),
            Some("3f2a9c1d4e5b6a7c8d9e0f1a2b3c4d5e")
        );
        assert_eq!(
            parse_container_id(b"Unable to find image\n").as_deref(),
            None
        );
        assert_eq!(parse_container_id(b"\n").as_deref(), None);
    }

    #[test]
    fn tag_listing_parser_keeps_valid_tags_and_drops_foreign_ones() {
        let tag = SandboxTag::new();
        let stdout = format!("abc123abc123\t{tag}\ndef456def456\tnot-a-uuid\n\t{tag}\n");
        let handles = parse_tag_listing(stdout.as_bytes());
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].reference, "abc123abc123");
        assert_eq!(handles[0].tag, tag);
    }

    #[test]
    fn missing_container_is_recognized_across_runtimes() {
        assert!(is_no_such_container(b"Error: No such container: abc"));
        assert!(is_no_such_container(b"error: no such object abc"));
        assert!(!is_no_such_container(b"Error: container is restarting"));
    }

    // --- Live-Docker tests, skipped cleanly when no runtime is present. ---

    /// A backend on a trivial public image that stays running and holds its
    /// listener port.
    fn live_backend() -> DockerSandboxBackend {
        DockerSandboxBackend::new(DockerConfig {
            image: "alpine:3.20".to_owned(),
            listener_port: 8080,
            // A foreground shell loop that keeps the container alive and re-listens
            // on 8080 with busybox `nc` (present in alpine's base busybox, unlike
            // `httpd`). The loop never exits, so the container stays running and its
            // published port keeps a real listener behind it; `|| sleep 1` avoids a
            // busy spin if a listen attempt ever fails.
            command: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "while true; do nc -l -p 8080 || sleep 1; done".to_owned(),
            ],
            ..DockerConfig::default()
        })
    }

    /// Poll until the container reports `State.Running`, so addressing and the
    /// reachability probe do not race container startup.
    async fn wait_running(backend: &DockerSandboxBackend, reference: &str) -> bool {
        for _ in 0..50 {
            let mut command = backend.command();
            command.args(["inspect", "--format", "{{.State.Running}}", reference]);
            if let Ok(output) = command.output().await {
                if output.status.success()
                    && String::from_utf8_lossy(&output.stdout).trim() == "true"
                {
                    return true;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        false
    }

    /// Every tag currently stamped on the daemon, so a sweep in the test treats
    /// pre-existing containers (a dev machine's real ones) as live and never reaps
    /// anything but the orphan this test creates.
    async fn existing_tags(backend: &DockerSandboxBackend) -> HashSet<SandboxTag> {
        backend
            .list_tagged()
            .await
            .unwrap()
            .into_iter()
            .map(|handle| handle.tag)
            .collect()
    }

    /// Whether the runtime's daemon answers, so a present binary with a stopped
    /// daemon skips the live test rather than failing it.
    async fn daemon_ready(backend: &DockerSandboxBackend) -> bool {
        let mut command = backend.command();
        command.args(["version", "--format", "{{.Server.Version}}"]);
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await,
            Ok(Ok(output)) if output.status.success()
        )
    }

    async fn tcp_reachable(base_url: &str) -> bool {
        let authority = base_url.trim_start_matches("http://");
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(authority).await.is_ok() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn docker_lifecycle_provision_address_destroy_and_orphan_sweep() {
        let backend = live_backend();
        if !backend.is_available() {
            eprintln!("skipping: no container runtime on PATH");
            return;
        }
        if !daemon_ready(&backend).await {
            eprintln!("skipping: container runtime binary present but daemon unreachable");
            return;
        }

        // Snapshot pre-existing tags so the sweep below cannot touch them.
        let baseline = existing_tags(&backend).await;

        // Provision the run we keep alive across the sweep.
        let live_tag = SandboxTag::new();
        let live = backend
            .provision(ProvisionRequest {
                run_id: RunId::new(),
                tag: live_tag,
                lifetime_cap_secs: None,
                task: None,
            })
            .await
            .expect("provision live container");

        // Provision the orphan: a container whose intent the host will not recognize.
        let orphan_tag = SandboxTag::new();
        let orphan = backend
            .provision(ProvisionRequest {
                run_id: RunId::new(),
                tag: orphan_tag,
                lifetime_cap_secs: None,
                task: None,
            })
            .await
            .expect("provision orphan container");

        // Wait for the container to actually be running before addressing it, so the
        // published-port lookup and reachability probe do not race startup.
        assert!(
            wait_running(&backend, &live.reference).await,
            "live container never reached the running state"
        );

        // address resolves to a loopback URL with a live listener behind it.
        let address = backend.address(&live).await.expect("resolve live address");
        assert!(address.base_url.starts_with("http://127.0.0.1:"));
        assert!(!address.transport_secret.expose().is_empty());
        assert!(
            tcp_reachable(&address.base_url).await,
            "published port {} was not reachable",
            address.base_url
        );

        // The sweep's live set is everything present except the orphan, so exactly
        // the orphan is reclaimable.
        let mut live_tags = existing_tags(&backend).await;
        live_tags.remove(&orphan_tag);
        let _ = baseline; // documented intent: pre-existing tags are preserved.
        let reclaimed = backend
            .reclaim_orphans(&live_tags)
            .await
            .expect("sweep orphans");
        assert!(
            reclaimed.iter().any(|handle| handle.tag == orphan_tag),
            "the orphan was not reclaimed"
        );

        // The orphan is gone; the live container is untouched and still addressable.
        assert!(matches!(
            backend.address(&orphan).await,
            Err(BackendError::UnknownHandle)
        ));
        assert!(backend.address(&live).await.is_ok());

        // destroy is idempotent: a second removal of the same container succeeds.
        backend.destroy(&live).await.expect("destroy live");
        backend
            .destroy(&live)
            .await
            .expect("destroy is idempotent on a missing container");

        // Best-effort cleanup in case an assertion above left the orphan behind.
        let _ = backend.destroy(&orphan).await;
    }
}
