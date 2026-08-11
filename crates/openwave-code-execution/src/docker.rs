//! A [`CodeExecutionProvider`] over a host-local Docker-compatible runtime.
//!
//! # Why this exists
//!
//! [`crate::LocalExecutionProvider`] confines commands with macOS Seatbelt and
//! has no implementation anywhere else, so on Linux and Windows the only
//! execution backends are the two managed cloud sandboxes — both of which
//! upload the conversation's staged files to a vendor. This backend closes
//! that gap without leaving the machine: commands run in a container on the
//! host's own runtime.
//!
//! It is **opt-in and never the default**. A container runtime is a heavy
//! dependency to assume, and [`DockerExecutionProvider::availability`]
//! reports the two ways it can be absent — no CLI, or a CLI whose daemon does
//! not answer — so the settings surface can say which one it is.
//!
//! # Environment parity
//!
//! The container runs the same digest-pinned documents image the Daytona
//! adapter registers and the E2B template is built from (see
//! [`crate::sandbox_image`]): LibreOffice, the document skills' preinstalled
//! Python closure, Node with the deck library, and the bundled exec helper
//! scripts. A document skill that works on E2B works here, on the same
//! versions, with no in-run package install.
//!
//! # Shape
//!
//! One long-lived container per chat workspace, addressed by a deterministic
//! name derived from the workspace id, adopted rather than duplicated when it
//! already exists. Commands run through `docker exec`; workspace file
//! transfers run through `docker exec` with a small `sh` reader or writer on
//! the far side. The session, idempotency-receipt, and staging-digest
//! machinery is the same [`crate::remote`] layer the managed adapters use, so
//! a replayed execution id and an already-staged file behave identically on
//! every backend.
//!
//! The container's own identity for that machinery is its container **id**,
//! not its name: a recreated container reuses the name, and a staging ledger
//! keyed by the name would wrongly conclude that files staged into the dead
//! container are still present in its replacement.
//!
//! # Confinement
//!
//! The container is the boundary. It runs as the image's unprivileged uid,
//! forced from the host so an image that lost its own `USER` still cannot run
//! as root, with every Linux capability dropped, privilege escalation
//! refused, and process, memory, and CPU ceilings applied. No host path is
//! bind-mounted: the workspace is an anonymous volume that is removed with the
//! container, and the only way host files enter it is the explicit staging the
//! server performs for the paths a call listed.
//!
//! The root filesystem is deliberately **not** read-only, unlike the
//! sandbox-agent container backend in `openwave-server`. Foreground exec is
//! interactive work: a command legitimately `pip install --user`s, writes
//! scratch beside its inputs, and expects a writable `HOME`. A read-only root
//! turns those into unexplained failures, and the writable surface it would
//! save is the container's own ephemeral layer, which is discarded with the
//! container either way.
//!
//! # Egress
//!
//! Only the strictest policy class is enforced. A policy that permits nothing
//! — the chat's "no network" setting — creates the container with
//! `--network none`, so it has no interface but loopback: no route, no DNS,
//! nothing to negotiate with. That is an external boundary, enforced by the
//! runtime rather than by anything inside the container.
//!
//! Every other class is **not** enforced. An allowlist would need a
//! per-container internal network and the egress proxy the sandbox-agent
//! container tier already runs; until that lands, a container created under an
//! allowlist policy runs on the runtime's default network with ordinary
//! outbound access, and this backend declares no enforcement for it rather
//! than implying one. [`DockerExecutionProvider::egress_enforcement`] states
//! the split, and the settings surface derives its disclosure from it.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use openwave_egress::{EgressEnforcement, EgressPolicy};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::output::{Capture, StreamKind};
use crate::remote::{
    connect_remote_workspace, create_remote_workspace, destroy_remote_workspace,
    egress_policy_fingerprint, execute_remote, stage_remote_file, with_remote_session,
    RemoteSandboxAdapter, RemoteSession, RemoteSessionError, RemoteSessionPool,
    RemoteWorkspaceAdapter,
};
use crate::sandbox_image::{
    image_digest_pinned, DOCUMENTS_CPU, DOCUMENTS_IMAGE, DOCUMENTS_MEMORY_GB,
};
use crate::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse, CodeExecutionUnavailableReason, ExecutionWorkspaceId, StagedUpload,
    WorkspaceFileEntry, WorkspaceFilePath, WorkspaceLifecycle, WorkspaceListing,
    MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
};

/// The runtime binary, preferred on `PATH`. Any Docker-CLI-compatible runtime
/// (`podman`) works through the same code path.
const DEFAULT_BINARY: &str = "docker";
/// The in-container workspace root. The image creates it owned by the
/// unprivileged user and declares it as `OPENWAVE_SANDBOX_WORKSPACE`.
const WORKSPACE_ROOT: &str = "/workspace";
/// The `uid:gid` the container runs as, forced from the host so an image that
/// lost its own `USER` directive still cannot run commands as root. Kept in
/// sync with the sandbox-agent image's `USER`, and with the ownership of the
/// workspace the image provisioned — a mismatch would leave the running user
/// unable to write its own workspace.
const CONTAINER_USER: &str = "10001:10001";
/// Prefix of the per-workspace container name. The name is a pure function of
/// the workspace id, so a host that restarted finds its containers again
/// without persisting anything.
const CONTAINER_PREFIX: &str = "openwave-exec-";
/// Label carrying the workspace a container serves. Present on every container
/// this backend creates, so an operator (or a later host-driven sweep) can
/// enumerate them with `docker ps --filter label=openwave.exec-workspace`.
const WORKSPACE_LABEL: &str = "openwave.exec-workspace";
/// Label carrying the runtime/image/network contract a workspace was created
/// for. A deterministic workspace name alone is not enough to prove a
/// container belongs to this provider configuration: changing the image, or
/// the network shape the chat's policy compiles to, must replace the old
/// container rather than silently adopting it. See
/// [`DockerExecutionProvider::configuration_identity`].
const CONFIGURATION_LABEL: &str = "openwave.exec-configuration";
/// Bump when the `docker run` confinement or keepalive contract changes in a
/// way that makes an already-running container unsafe or incompatible to
/// reuse.
const CONTAINER_CONTRACT_REVISION: &str = "1";
/// Process-count ceiling: comfortably above a shell pipeline, a Python
/// helper, or a headless LibreOffice conversion, and far below what a fork
/// bomb needs to wedge the host.
const PIDS_LIMIT: u32 = 512;
/// How long an idle container may live before it exits on its own.
///
/// Docker enforces no TTL, and a host-side timer is worth nothing in the case
/// that matters — the host process dying is exactly what strands a container.
/// So the container's only process is a bounded `sleep`, and `--rm` removes
/// the container (and its anonymous workspace volume) when that sleep ends.
/// A chat still working after the cap simply gets a fresh container: every
/// execution re-stages the files it listed, so nothing durable is lost.
const CONTAINER_LIFETIME: Duration = Duration::from_secs(4 * 60 * 60);
/// Grace added to a command's own timeout before the runtime CLI invocation
/// itself is abandoned. The in-container `timeout` is what actually stops the
/// command; this only bounds a wedged daemon.
const CLI_GRACE: Duration = Duration::from_secs(15);
/// Bound on one control-plane invocation (`run`, `inspect`, `start`, `rm`).
const CONTROL_TIMEOUT: Duration = Duration::from_secs(120);
/// Bound on the daemon liveness probe. Short: this runs behind a settings
/// read, and an unresponsive daemon must report unavailable rather than hang
/// the surface.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an availability answer is reused before the runtime is probed
/// again. Long enough that a settings render costs one probe, short enough
/// that starting Docker Desktop is reflected without restarting OpenWave.
const AVAILABILITY_TTL: Duration = Duration::from_secs(10);
/// `timeout`'s exit status when it stopped the command.
const TIMEOUT_EXIT: i32 = 124;
/// How long the in-container `timeout` waits after `TERM` before `KILL`.
const TIMEOUT_KILL_AFTER_SECS: u64 = 5;
/// Exit statuses the file helpers use to report a bounded, expected outcome
/// rather than a failure. Chosen above the range a shell reports for signals
/// and command-not-found.
const FILE_MISSING_EXIT: i32 = 66;
const FILE_TOO_LARGE_EXIT: i32 = 67;
const DIR_MISSING_EXIT: i32 = 68;

/// The file name to look for while scanning `PATH`. Windows carries the
/// extension in the file name; the command spawned when `PATH` does hold the
/// runtime stays the bare name, which the loader resolves through `PATHEXT`.
#[cfg(windows)]
const PATH_FILE_NAME: &str = "docker.exe";
#[cfg(not(windows))]
const PATH_FILE_NAME: &str = DEFAULT_BINARY;

/// Absolute locations to try when `PATH` carries no runtime CLI at all, in
/// preference order.
///
/// An app launched from Finder inherits launchd's `PATH`
/// (`/usr/bin:/bin:/usr/sbin:/sbin`), which contains none of the directories
/// Docker Desktop and OrbStack install their CLI shim into. Without this
/// fallback a running daemon is indistinguishable from an uninstalled one,
/// and only for GUI launches — the same build started from a terminal works.
fn fallback_binary_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut candidates = vec![
            PathBuf::from("/usr/local/bin").join(DEFAULT_BINARY),
            PathBuf::from("/opt/homebrew/bin").join(DEFAULT_BINARY),
        ];
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            candidates.push(home.join(".orbstack/bin").join(DEFAULT_BINARY));
            candidates.push(home.join(".docker/bin").join(DEFAULT_BINARY));
        }
        candidates.push(
            PathBuf::from("/Applications/Docker.app/Contents/Resources/bin").join(DEFAULT_BINARY),
        );
        candidates
    }
    #[cfg(target_os = "linux")]
    {
        vec![
            PathBuf::from("/usr/local/bin").join(DEFAULT_BINARY),
            PathBuf::from("/usr/bin").join(DEFAULT_BINARY),
        ]
    }
    // Windows resolves installed CLIs through `PATH` reliably, including for
    // GUI launches, so there is nothing to fall back to.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// The runtime binary this host should invoke.
///
/// Resolved fresh on every construction and availability probe — a handful of
/// `stat` calls — so installing a runtime mid-session is picked up without an
/// app restart. Shared with the background-agent container backend, which has
/// the same problem for the same reason.
pub fn resolve_container_runtime_binary() -> String {
    resolve_binary(
        DEFAULT_BINARY,
        PATH_FILE_NAME,
        std::env::var_os("PATH").as_deref(),
        &fallback_binary_candidates(),
    )
}

/// `PATH` first, then the well-known locations; the bare name if neither has
/// it, so the spawn fails and classifies as a missing runtime as before.
fn resolve_binary(
    bare_name: &str,
    path_file_name: &str,
    path_var: Option<&OsStr>,
    fallbacks: &[PathBuf],
) -> String {
    let on_path = path_var.is_some_and(|var| {
        std::env::split_paths(var)
            .any(|dir| !dir.as_os_str().is_empty() && binary_exists(&dir.join(path_file_name)))
    });
    if on_path {
        return bare_name.to_owned();
    }
    fallbacks
        .iter()
        .find(|candidate| binary_exists(candidate))
        .map_or_else(
            || bare_name.to_owned(),
            |candidate| candidate.to_string_lossy().into_owned(),
        )
}

/// Whether a candidate path names a runnable file. Metadata follows symlinks,
/// so an installer shim pointing into an app bundle counts as present. The
/// probe itself establishes whether the daemon answers; this only decides
/// which binary to ask.
fn binary_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Direct-command adapter for a container on the host's own runtime.
pub struct DockerExecutionProvider {
    binary: String,
    image: String,
    timeout: Duration,
    pool: RemoteSessionPool,
    egress: Option<EgressPolicy>,
}

/// How a configured egress policy compiles into container networking.
///
/// Only two shapes exist today, and the gap between them is the whole of this
/// backend's egress story: a policy that permits nothing becomes a container
/// with no network, and everything else becomes the runtime's default network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerNetwork {
    /// `--network none`: loopback only. Nothing in the container can reach a
    /// destination, resolve a name, or route to the host.
    None,
    /// The runtime's default network — ordinary outbound access. No part of
    /// the configured policy is applied.
    Default,
}

/// Compile the host policy into the container's network shape.
///
/// A policy that permits nothing at all is the only class this backend can
/// enforce with a creation-time flag, and it is exactly the class the chat's
/// "no network" setting produces (an allowlist with no grants on either axis).
/// Any policy with a grant in it needs a proxy topology that does not exist
/// here yet, so it compiles to the default network and is disclosed as
/// unenforced rather than partially applied.
fn container_network(policy: Option<&EgressPolicy>) -> ContainerNetwork {
    match policy {
        None => ContainerNetwork::Default,
        Some(EgressPolicy::BlockAll) => ContainerNetwork::None,
        Some(EgressPolicy::Allowlist(allowlist)) => {
            if allowlist.is_empty() {
                ContainerNetwork::None
            } else {
                ContainerNetwork::Default
            }
        }
    }
}

impl DockerExecutionProvider {
    pub fn new(timeout: Duration) -> Result<Self, CodeExecutionError> {
        Self::with_session_pool(timeout, RemoteSessionPool::default())
    }

    pub fn with_session_pool(
        timeout: Duration,
        pool: RemoteSessionPool,
    ) -> Result<Self, CodeExecutionError> {
        if timeout.is_zero() {
            return Err(CodeExecutionError::InvalidRequest(
                "execution timeout must be positive".into(),
            ));
        }
        Ok(Self {
            binary: resolve_container_runtime_binary(),
            image: DOCUMENTS_IMAGE.to_owned(),
            timeout,
            pool,
            egress: None,
        })
    }

    /// Run containers on a different Docker-CLI-compatible binary (`podman`).
    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Apply an egress policy to every container this provider creates.
    ///
    /// Only a policy that permits nothing is compiled into anything: it
    /// becomes `--network none`. A policy with grants in it is accepted and
    /// deliberately *not* applied — this backend has no way to enforce a
    /// partial allowlist yet, and [`Self::egress_enforcement`] says so, so a
    /// caller passing one is not told it was enforced.
    #[must_use]
    pub fn with_egress_policy(mut self, policy: EgressPolicy) -> Self {
        self.egress = Some(policy);
        self
    }

    /// The egress policy applied to this provider's containers, if any.
    #[must_use]
    pub fn egress_policy(&self) -> Option<&EgressPolicy> {
        self.egress.as_ref()
    }

    /// Host knowledge about what container networking enforces for `policy`.
    ///
    /// `None` is the honest answer for every policy this backend does not
    /// compile into container creation: nothing is applied, so there is no
    /// enforcement to declare and the surface must not borrow one. A policy
    /// that permits nothing becomes `--network none`, which is enforced by the
    /// runtime outside the container and leaves no exception open — no name
    /// resolution, no host route, nothing a curated carve-out could hide in.
    #[must_use]
    pub fn egress_enforcement(policy: Option<&EgressPolicy>) -> Option<EgressEnforcement> {
        match container_network(policy) {
            ContainerNetwork::None => Some(EgressEnforcement::external(Vec::new())),
            ContainerNetwork::Default => None,
        }
    }

    /// Run a different image than the pinned documents one.
    ///
    /// An escape hatch, not the normal path. An override that names a tag
    /// rather than a digest gives up the content-addressed resolution the
    /// default has — see [`Self::verifies_image_integrity`] — and an image
    /// without the document tooling degrades the document skills to whatever
    /// it happens to contain.
    #[must_use]
    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Whether the configured image ref resolves content-addressed. True for
    /// the default pin; false for an override that names a mutable tag.
    #[must_use]
    pub fn verifies_image_integrity(&self) -> bool {
        image_digest_pinned(&self.image)
    }

    /// Whether a container runtime can actually serve this host right now.
    ///
    /// Two distinguishable failures, because the fix differs: no CLI to be
    /// found — neither on `PATH` nor in any of the well-known install
    /// locations [`resolve_container_runtime_binary`] falls back to — or a
    /// CLI whose daemon does not answer (Docker Desktop not started, the
    /// socket not permitted to this user). The answer is cached for
    /// [`AVAILABILITY_TTL`] in both directions, so a settings render costs at
    /// most one probe and starting the daemon is picked up without a restart.
    pub async fn availability() -> Result<(), CodeExecutionUnavailableReason> {
        Self::availability_of(&resolve_container_runtime_binary()).await
    }

    /// [`Self::availability`] for a named runtime binary.
    pub async fn availability_of(binary: &str) -> Result<(), CodeExecutionUnavailableReason> {
        static CACHE: LazyLock<Mutex<HashMap<String, (Instant, AvailabilityAnswer)>>> =
            LazyLock::new(|| Mutex::new(HashMap::new()));
        if let Ok(cache) = CACHE.lock() {
            if let Some((probed, answer)) = cache.get(binary) {
                if probed.elapsed() < AVAILABILITY_TTL {
                    return answer.into_result();
                }
            }
        }
        let answer = probe_runtime(binary).await;
        if let Ok(mut cache) = CACHE.lock() {
            cache.insert(binary.to_owned(), (Instant::now(), answer));
        }
        answer.into_result()
    }

    /// A fresh runtime invocation with stdio detached from this process.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// Run one control-plane invocation, returning its captured stdout on
    /// success and the classified failure otherwise.
    async fn control(&self, args: &[String]) -> Result<Vec<u8>, ControlFailure> {
        let mut command = self.command();
        command.args(args);
        let child = command.spawn().map_err(|_| ControlFailure::Runtime)?;
        let output = bounded_output(child, CONTROL_TIMEOUT, usize::MAX)
            .await
            .ok_or(ControlFailure::Runtime)?;
        if output.status == Some(0) {
            return Ok(output.stdout);
        }
        Err(ControlFailure::Refused(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }

    /// Ensure the workspace's container exists and is running, and report the
    /// container id that identifies this instance of it.
    ///
    /// Adopts an existing container rather than duplicating it, so a host that
    /// restarted reuses the files a previous session left in the workspace.
    async fn ensure_container(&self, workspace_id: &str) -> Result<String, CodeExecutionError> {
        let name = container_name(workspace_id);
        let configuration = self.configuration_identity();
        if let Some(existing) = self.inspect_container(&name).await? {
            match existing.disposition(workspace_id, &configuration) {
                ExistingContainerDisposition::Adopt => {
                    if existing.running {
                        return Ok(existing.id);
                    }
                    // A stopped container created with `--rm` is on its way
                    // out; only one created without it can be restarted.
                    // Either way, a start that fails is not fatal — a fresh
                    // container serves just as well.
                    if self.control(&start_args(&existing.id)).await.is_ok() {
                        if let Some(started) = self.inspect_container(&existing.id).await? {
                            if started.running
                                && matches!(
                                    started.disposition(workspace_id, &configuration),
                                    ExistingContainerDisposition::Adopt
                                )
                            {
                                return Ok(started.id);
                            }
                        }
                    }
                    self.remove_container(&existing.id).await?;
                }
                ExistingContainerDisposition::Replace => {
                    self.remove_container(&existing.id).await?;
                }
                ExistingContainerDisposition::Conflict => {
                    return Err(CodeExecutionError::Unavailable(
                        "the workspace container name is already in use".into(),
                    ));
                }
            }
        }
        match self.control(&self.run_args(workspace_id)).await {
            Ok(stdout) => parse_container_id(&stdout).ok_or_else(|| {
                CodeExecutionError::Unavailable(
                    "the container runtime returned no container id".into(),
                )
            }),
            // Another process (or another OpenWave window) won the race to
            // create this workspace's container; adopt what is there.
            Err(ControlFailure::Refused(stderr)) if is_name_conflict(&stderr) => {
                match self.inspect_container(&name).await? {
                    Some(existing)
                        if existing.running
                            && matches!(
                                existing.disposition(workspace_id, &configuration),
                                ExistingContainerDisposition::Adopt
                            ) =>
                    {
                        Ok(existing.id)
                    }
                    _ => Err(CodeExecutionError::Unavailable(
                        "the workspace container could not be started".into(),
                    )),
                }
            }
            Err(failure) => Err(failure.into_error()),
        }
    }

    /// Inspect one container by name or id. `None` means no such container.
    async fn inspect_container(
        &self,
        reference: &str,
    ) -> Result<Option<ContainerState>, CodeExecutionError> {
        let args = inspect_args(reference);
        match self.control(&args).await {
            Ok(stdout) => Ok(parse_inspect(&stdout)),
            Err(ControlFailure::Refused(stderr)) if is_no_such_container(&stderr) => Ok(None),
            Err(failure) => Err(failure.into_error()),
        }
    }

    /// `docker rm -f -v`, treating a missing container as success. `-v`
    /// removes the anonymous workspace volume; without it every torn-down
    /// workspace would leave its volume dangling on the host's disk.
    async fn remove_container(&self, reference: &str) -> Result<(), CodeExecutionError> {
        match self.control(&remove_args(reference)).await {
            Ok(_) => Ok(()),
            Err(ControlFailure::Refused(stderr)) if is_no_such_container(&stderr) => Ok(()),
            Err(failure) => Err(failure.into_error()),
        }
    }

    /// The `docker run` argument vector for one workspace container.
    ///
    /// Factored out so the confinement flags are one readable list and can be
    /// asserted without a daemon: a control dropped here is invisible at
    /// runtime, because a container missing it starts and serves exactly like
    /// a confined one.
    fn run_args(&self, workspace_id: &str) -> Vec<String> {
        let mut args = vec![
            "run".to_owned(),
            "--detach".to_owned(),
            // The container removes itself — and its anonymous workspace
            // volume — when its bounded sleep ends, so an abandoned chat does
            // not leave a container behind for a sweep to find.
            "--rm".to_owned(),
            "--name".to_owned(),
            container_name(workspace_id),
            "--label".to_owned(),
            format!("{WORKSPACE_LABEL}={workspace_id}"),
            "--label".to_owned(),
            format!("{CONFIGURATION_LABEL}={}", self.configuration_identity()),
            // Confinement. The container is the boundary; each of these is a
            // control the commands inside cannot recover.
            "--user".to_owned(),
            CONTAINER_USER.to_owned(),
            "--cap-drop".to_owned(),
            "ALL".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges:true".to_owned(),
            "--pids-limit".to_owned(),
            PIDS_LIMIT.to_string(),
            "--memory".to_owned(),
            format!("{DOCUMENTS_MEMORY_GB}g"),
            "--cpus".to_owned(),
            DOCUMENTS_CPU.to_string(),
            // Reap the children a command backgrounds; the keepalive process
            // is not an init and would collect zombies for the container's
            // whole life.
            "--init".to_owned(),
            // No host path is bind-mounted. The workspace is an anonymous
            // volume: initialized from the image's own ownership, removed
            // with the container by `--rm` and by `rm -v`.
            "--volume".to_owned(),
            WORKSPACE_ROOT.to_owned(),
            "--workdir".to_owned(),
            WORKSPACE_ROOT.to_owned(),
        ];
        // The one part of the egress policy this backend can enforce.
        // Deliberately absent — rather than spelled as the runtime's default
        // network by name — when nothing is enforced, so a host that runs its
        // containers on a custom default network keeps it.
        if self.container_network() == ContainerNetwork::None {
            args.extend(["--network".to_owned(), "none".to_owned()]);
        }
        args.extend([
            // The image's entrypoint is the sandbox agent, which is a
            // different tier's protocol entirely. This backend wants only a
            // container to exec into, so the entrypoint is replaced by a
            // bounded keepalive.
            "--entrypoint".to_owned(),
            "sleep".to_owned(),
            self.image.clone(),
            CONTAINER_LIFETIME.as_secs().to_string(),
        ]);
        args
    }

    /// The network shape the configured policy compiles into.
    fn container_network(&self) -> ContainerNetwork {
        container_network(self.egress.as_ref())
    }

    fn configuration_fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"docker\n");
        hasher.update(CONTAINER_CONTRACT_REVISION.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.binary.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.image.as_bytes());
        hasher.finalize().into()
    }

    /// The identity a created container is labelled with, and the one an
    /// existing container must match to be adopted.
    ///
    /// It carries the network shape as well as the runtime and image, because
    /// the pooled session's egress check only covers containers this process
    /// remembers creating. A host that restarted — or a second OpenWave
    /// window — finds the container by its deterministic name with no pooled
    /// handle at all, and adopting one created with ordinary network access
    /// into a chat whose network is off would silently undo the enforcement.
    fn configuration_identity(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.configuration_fingerprint());
        hasher.update(match self.container_network() {
            ContainerNetwork::None => b"\nnetwork:none".as_slice(),
            ContainerNetwork::Default => b"\nnetwork:default".as_slice(),
        });
        let identity: [u8; 32] = hasher.finalize().into();
        let mut encoded = String::with_capacity(64);
        for byte in identity {
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        encoded
    }

    async fn run_docker_command(
        &self,
        session: &RemoteSession,
        request: &CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, RemoteSessionError> {
        validate_container_id(&session.sandbox_id)?;
        let started = Instant::now();
        let mut command = self.command();
        command.args(exec_command_args(
            &session.sandbox_id,
            request,
            self.timeout,
        )?);
        let child = command
            .spawn()
            .map_err(|_| CodeExecutionError::Unavailable(RUNTIME_SPAWN_FAILED.into()))?;
        let Some(output) = bounded_output(
            child,
            self.timeout.saturating_add(CLI_GRACE),
            crate::MAX_CAPTURE_BYTES,
        )
        .await
        else {
            // The CLI itself was abandoned, so whether the command ran to
            // completion inside the container is genuinely unknown.
            return Err(CodeExecutionError::AmbiguousExecution.into());
        };
        if output.status.is_none() {
            return Err(CodeExecutionError::AmbiguousExecution.into());
        }
        if is_missing_container(output.status, &output.stderr) {
            return Err(RemoteSessionError::Missing);
        }
        let mut capture = Capture::default();
        capture.append(&output.stdout, StreamKind::Stdout);
        capture.append(&output.stderr, StreamKind::Stderr);
        // The in-container `timeout` is what stopped the command, and its own
        // exit status is how it says so. A command that exits 124 by itself is
        // indistinguishable from one that was stopped — the same conflation
        // every backend's timeout reporting carries.
        let timed_out = output.status == Some(TIMEOUT_EXIT);
        Ok(capture.response(
            CodeExecutionProviderKind::Docker,
            started,
            if timed_out { None } else { output.status },
            timed_out,
        ))
    }

    /// Run one `sh` helper inside the container, optionally feeding it stdin,
    /// and return its exit status with its captured output.
    async fn run_helper(
        &self,
        session: &RemoteSession,
        script: &str,
        argument: &str,
        stdin: Option<&[u8]>,
        capture_bytes: usize,
    ) -> Result<CapturedOutput, RemoteSessionError> {
        validate_container_id(&session.sandbox_id)?;
        let mut command = self.command();
        if stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        command.args(helper_args(&session.sandbox_id, script, argument));
        let mut child = command
            .spawn()
            .map_err(|_| CodeExecutionError::Unavailable(RUNTIME_SPAWN_FAILED.into()))?;
        if let Some(bytes) = stdin {
            let mut pipe = child
                .stdin
                .take()
                .ok_or_else(|| CodeExecutionError::Unavailable(RUNTIME_SPAWN_FAILED.into()))?;
            let bytes = bytes.to_vec();
            tokio::spawn(async move {
                let _ = pipe.write_all(&bytes).await;
                let _ = pipe.shutdown().await;
            });
        }
        let output = bounded_output(child, CONTROL_TIMEOUT, capture_bytes)
            .await
            .ok_or_else(|| CodeExecutionError::Unavailable(RUNTIME_SPAWN_FAILED.into()))?;
        if is_missing_container(output.status, &output.stderr) {
            return Err(RemoteSessionError::Missing);
        }
        Ok(output)
    }
}

#[async_trait]
impl RemoteSandboxAdapter for DockerExecutionProvider {
    fn kind(&self) -> CodeExecutionProviderKind {
        CodeExecutionProviderKind::Docker
    }

    /// There is no credential; what distinguishes one configuration's
    /// containers from another's is the runtime and the image. Binding the
    /// pooled session to those means changing either replaces the container
    /// rather than reusing one built from the previous image.
    fn credential_fingerprint(&self) -> [u8; 32] {
        self.configuration_fingerprint()
    }

    /// The pooled container is bound to the policy it was created under, so a
    /// policy change replaces it instead of reusing a container whose
    /// networking no longer matches. The fingerprint is over the policy
    /// itself, not over the network shape it compiles to: a container created
    /// under an unenforced allowlist and one created under open egress are
    /// networked identically today, but conflating them would mean a chat that
    /// moved between those settings kept a container the *next* slice's
    /// enforcement would have replaced.
    fn egress_fingerprint(&self) -> [u8; 32] {
        egress_policy_fingerprint(self.egress.as_ref())
    }

    async fn create_session(
        &self,
        workspace_id: &str,
    ) -> Result<RemoteSession, CodeExecutionError> {
        let id = self.ensure_container(workspace_id).await?;
        Ok(RemoteSession {
            sandbox_id: id,
            endpoint: None,
            access_token: None,
        })
    }

    async fn destroy_sandbox(&self, session: &RemoteSession) -> Result<(), CodeExecutionError> {
        validate_container_id(&session.sandbox_id)?;
        self.remove_container(&session.sandbox_id).await
    }

    async fn reconnect_session(
        &self,
        session: &RemoteSession,
    ) -> Result<Option<RemoteSession>, CodeExecutionError> {
        validate_container_id(&session.sandbox_id)?;
        let Some(state) = self.inspect_container(&session.sandbox_id).await? else {
            return Ok(None);
        };
        // A container that stopped is not reusable: `--rm` is removing it, and
        // its workspace volume goes with it.
        if !state.running {
            return Ok(None);
        }
        Ok(Some(session.clone()))
    }

    async fn run_command(
        &self,
        session: &RemoteSession,
        request: &CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, RemoteSessionError> {
        self.run_docker_command(session, request).await
    }
}

#[async_trait]
impl RemoteWorkspaceAdapter for DockerExecutionProvider {
    async fn upload_file(
        &self,
        session: &RemoteSession,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), RemoteSessionError> {
        let output = self
            .run_helper(
                session,
                WRITE_FILE_SCRIPT,
                path.as_str(),
                Some(content),
                4096,
            )
            .await?;
        if output.status == Some(0) {
            return Ok(());
        }
        Err(CodeExecutionError::Unavailable("could not write the workspace file".into()).into())
    }

    async fn download_file(
        &self,
        session: &RemoteSession,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, RemoteSessionError> {
        let output = self
            .run_helper(
                session,
                READ_FILE_SCRIPT,
                path.as_str(),
                None,
                MAX_WORKSPACE_FILE_BYTES,
            )
            .await?;
        match output.status {
            Some(0) => Ok(output.stdout),
            Some(FILE_MISSING_EXIT) => Err(CodeExecutionError::WorkspaceFileNotFound.into()),
            Some(FILE_TOO_LARGE_EXIT) => Err(CodeExecutionError::WorkspaceFileTooLarge.into()),
            _ => Err(
                CodeExecutionError::Unavailable("could not read the workspace file".into()).into(),
            ),
        }
    }

    async fn list_directory(
        &self,
        session: &RemoteSession,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, RemoteSessionError> {
        let listed = path.map_or(".", WorkspaceFilePath::as_str);
        let output = self
            .run_helper(
                session,
                LIST_DIR_SCRIPT,
                listed,
                None,
                LISTING_CAPTURE_BYTES,
            )
            .await?;
        match output.status {
            Some(0) => {}
            Some(DIR_MISSING_EXIT) => return Err(CodeExecutionError::WorkspaceFileNotFound.into()),
            _ => {
                return Err(CodeExecutionError::Unavailable(
                    "could not list the workspace directory".into(),
                )
                .into())
            }
        }
        Ok(parse_listing(&output.stdout, path))
    }
}

#[async_trait]
impl WorkspaceLifecycle for DockerExecutionProvider {
    async fn create_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError> {
        create_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn connect_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<bool, CodeExecutionError> {
        connect_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn destroy_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError> {
        destroy_remote_workspace(self, &self.pool, workspace.as_str()).await
    }

    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), CodeExecutionError> {
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(CodeExecutionError::WorkspaceFileTooLarge);
        }
        with_remote_session(
            self,
            &self.pool,
            workspace.as_str(),
            |adapter, session| async move { adapter.upload_file(&session, path, content).await },
        )
        .await
    }

    async fn stage_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<StagedUpload, CodeExecutionError> {
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(CodeExecutionError::WorkspaceFileTooLarge);
        }
        stage_remote_file(self, &self.pool, workspace.as_str(), path, content).await
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, CodeExecutionError> {
        with_remote_session(
            self,
            &self.pool,
            workspace.as_str(),
            |adapter, session| async move { adapter.download_file(&session, path).await },
        )
        .await
    }

    async fn list_workspace_files(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, CodeExecutionError> {
        with_remote_session(
            self,
            &self.pool,
            workspace.as_str(),
            |adapter, session| async move { adapter.list_directory(&session, path).await },
        )
        .await
    }
}

#[async_trait]
impl CodeExecutionProvider for DockerExecutionProvider {
    async fn execute(
        &self,
        request: CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError> {
        execute_remote(self, &self.pool, request).await
    }

    fn workspace_lifecycle(&self) -> Option<&dyn WorkspaceLifecycle> {
        Some(self)
    }
}

const RUNTIME_SPAWN_FAILED: &str = "could not invoke the container runtime";
/// Cap on a directory listing's captured bytes. Generous next to the entry
/// cap it feeds, so truncation is decided by [`MAX_WORKSPACE_LIST_ENTRIES`]
/// rather than by a half-read line.
const LISTING_CAPTURE_BYTES: usize = 512 * 1024;

/// Writes stdin to `$1`, creating its parent directories. `dirname` of a
/// bare filename is `.`, so the `mkdir` is harmless for a root-level file.
const WRITE_FILE_SCRIPT: &str = r#"set -e
mkdir -p -- "$(dirname -- "$1")"
cat > "$1""#;

/// Reads `$1`, reporting absence and over-bound size as distinct statuses
/// instead of as an unreadable failure.
const READ_FILE_SCRIPT: &str = r#"if [ ! -f "$1" ]; then exit 66; fi
size=$(wc -c < "$1") || exit 1
if [ "$size" -gt $MAX_BYTES ]; then exit 67; fi
cat -- "$1""#;

/// Lists one directory, one entry per line as `type<TAB>size<TAB>name`.
/// Non-recursive, matching the contract every other backend's listing keeps.
const LIST_DIR_SCRIPT: &str = r#"cd -- "$1" 2>/dev/null || exit 68
find . -mindepth 1 -maxdepth 1 -printf '%y\t%s\t%f\n'"#;

/// Whether a container runtime is usable, as the three states the probe can
/// establish.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AvailabilityAnswer {
    Ready,
    MissingRuntime,
    Unreachable,
}

impl AvailabilityAnswer {
    fn into_result(self) -> Result<(), CodeExecutionUnavailableReason> {
        match self {
            Self::Ready => Ok(()),
            Self::MissingRuntime => Err(CodeExecutionUnavailableReason::MissingContainerRuntime),
            Self::Unreachable => Err(CodeExecutionUnavailableReason::ContainerRuntimeUnreachable),
        }
    }
}

/// Ask the runtime for its *server* version: the cheapest call that proves the
/// daemon answered rather than only that a client binary exists.
async fn probe_runtime(binary: &str) -> AvailabilityAnswer {
    let mut command = Command::new(binary);
    command
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let Ok(child) = command.spawn() else {
        return AvailabilityAnswer::MissingRuntime;
    };
    match bounded_output(child, PROBE_TIMEOUT, 4096).await {
        Some(output) if output.status == Some(0) && !output.stdout.is_empty() => {
            AvailabilityAnswer::Ready
        }
        _ => AvailabilityAnswer::Unreachable,
    }
}

/// Why a control-plane invocation did not succeed. The runtime refusing with a
/// message is a different thing from being unable to run it at all, and only
/// the refusal carries text worth classifying.
enum ControlFailure {
    Runtime,
    Refused(String),
}

impl ControlFailure {
    fn into_error(self) -> CodeExecutionError {
        match self {
            Self::Runtime => CodeExecutionError::Unavailable(RUNTIME_SPAWN_FAILED.into()),
            // The runtime's own message is not surfaced: it is operator
            // diagnostics, and the model-facing error stays a stable sentence.
            Self::Refused(stderr) => {
                tracing::debug!(stderr = %stderr, "container runtime refused an invocation");
                CodeExecutionError::Unavailable("the container runtime refused the request".into())
            }
        }
    }
}

impl From<ControlFailure> for CodeExecutionError {
    fn from(failure: ControlFailure) -> Self {
        failure.into_error()
    }
}

/// One container's state, as much of it as this backend needs.
struct ContainerState {
    id: String,
    running: bool,
    workspace: Option<String>,
    configuration: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingContainerDisposition {
    Adopt,
    Replace,
    Conflict,
}

impl ContainerState {
    fn disposition(&self, workspace_id: &str, configuration: &str) -> ExistingContainerDisposition {
        if self.workspace.as_deref() != Some(workspace_id) {
            return ExistingContainerDisposition::Conflict;
        }
        if self.configuration.as_deref() != Some(configuration) {
            return ExistingContainerDisposition::Replace;
        }
        ExistingContainerDisposition::Adopt
    }
}

struct CapturedOutput {
    /// `None` when the process was killed by a signal.
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Drain a child's streams up to `capture_bytes` each and wait for it,
/// abandoning the whole thing after `deadline`.
///
/// Reading concurrently rather than sequentially matters: a command that fills
/// the stderr pipe while this waits on stdout would block forever. Bytes past
/// the cap are read and dropped rather than left in the pipe, for the same
/// reason.
async fn bounded_output(
    mut child: Child,
    deadline: Duration,
    capture_bytes: usize,
) -> Option<CapturedOutput> {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let drain = async {
        let (out, err) = tokio::join!(
            drain_stream(&mut stdout, capture_bytes),
            drain_stream(&mut stderr, capture_bytes),
        );
        let status = child.wait().await.ok()?;
        Some(CapturedOutput {
            status: status.code(),
            stdout: out,
            stderr: err,
        })
    };
    tokio::time::timeout(deadline, drain).await.ok()?
}

async fn drain_stream<S>(stream: &mut Option<S>, capture_bytes: usize) -> Vec<u8>
where
    S: AsyncReadExt + Unpin,
{
    let mut kept = Vec::new();
    let Some(stream) = stream.as_mut() else {
        return kept;
    };
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return kept,
            Ok(read) => {
                let available = capture_bytes.saturating_sub(kept.len());
                if available > 0 {
                    kept.extend_from_slice(&buffer[..read.min(available)]);
                }
            }
        }
    }
}

/// The container name serving one workspace. Workspace ids are validated to
/// ASCII alphanumerics, `-`, and `_`, so the prefix is what makes the result a
/// legal container name whatever the id starts with.
fn container_name(workspace_id: &str) -> String {
    format!("{CONTAINER_PREFIX}{workspace_id}")
}

fn start_args(reference: &str) -> Vec<String> {
    vec!["start".to_owned(), reference.to_owned()]
}

fn remove_args(reference: &str) -> Vec<String> {
    vec![
        "rm".to_owned(),
        "--force".to_owned(),
        "--volumes".to_owned(),
        reference.to_owned(),
    ]
}

fn inspect_args(reference: &str) -> Vec<String> {
    vec![
        "inspect".to_owned(),
        "--format".to_owned(),
        format!(
            "{{{{.Id}}}}\t{{{{.State.Running}}}}\t{{{{index .Config.Labels \"{WORKSPACE_LABEL}\"}}}}\t{{{{index .Config.Labels \"{CONFIGURATION_LABEL}\"}}}}"
        ),
        reference.to_owned(),
    ]
}

/// The `docker exec` argument vector for one command.
///
/// The argv crosses as argv — the runtime execs the named program directly, so
/// no shell parses the model's arguments and no quoting question arises. The
/// in-container `timeout` is prepended because killing the CLI on this side
/// would leave the command running inside the container.
fn exec_command_args(
    container: &str,
    request: &CodeExecutionRequest,
    timeout: Duration,
) -> Result<Vec<String>, CodeExecutionError> {
    let mut args = vec![
        "exec".to_owned(),
        "--workdir".to_owned(),
        container_cwd(&request.cwd)?,
        container.to_owned(),
        "timeout".to_owned(),
        format!("--kill-after={TIMEOUT_KILL_AFTER_SECS}"),
        timeout_seconds(timeout).to_string(),
        request.command.clone(),
    ];
    args.extend(request.arguments.iter().cloned());
    Ok(args)
}

/// The `docker exec` argument vector for one `sh` helper. The script is fixed
/// text from this file and the workspace-relative path crosses as `$1`, never
/// as interpolated script text.
fn helper_args(container: &str, script: &str, argument: &str) -> Vec<String> {
    vec![
        "exec".to_owned(),
        "--interactive".to_owned(),
        "--workdir".to_owned(),
        WORKSPACE_ROOT.to_owned(),
        container.to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.replace("$MAX_BYTES", &MAX_WORKSPACE_FILE_BYTES.to_string()),
        "sh".to_owned(),
        argument.to_owned(),
    ]
}

/// Resolve a workspace-relative working directory to its container path.
fn container_cwd(cwd: &str) -> Result<String, CodeExecutionError> {
    let mut resolved = WORKSPACE_ROOT.to_owned();
    for component in Path::new(cwd).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    CodeExecutionError::InvalidRequest("invalid working directory".into())
                })?;
                resolved.push('/');
                resolved.push_str(part);
            }
            _ => {
                return Err(CodeExecutionError::InvalidRequest(
                    "invalid working directory".into(),
                ))
            }
        }
    }
    Ok(resolved)
}

fn timeout_seconds(timeout: Duration) -> u64 {
    (timeout.as_millis().saturating_add(999) / 1_000)
        .try_into()
        .unwrap_or(u64::MAX)
        .max(1)
}

/// Parse `docker run --detach`'s stdout: one container id on its own line.
fn parse_container_id(stdout: &[u8]) -> Option<String> {
    let id = String::from_utf8_lossy(stdout).trim().to_owned();
    validate_container_id(&id).ok().map(|()| id)
}

fn parse_inspect(stdout: &[u8]) -> Option<ContainerState> {
    let line = String::from_utf8_lossy(stdout);
    let mut fields = line.trim_end_matches(['\r', '\n']).splitn(4, '\t');
    let (Some(id), Some(running), Some(workspace), Some(configuration)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };
    validate_container_id(id).ok()?;
    Some(ContainerState {
        id: id.to_owned(),
        running: running.trim() == "true",
        workspace: parsed_label(workspace),
        configuration: parsed_label(configuration),
    })
}

fn parsed_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "<no value>").then(|| value.to_owned())
}

/// Turn the helper's `type<TAB>size<TAB>name` lines into workspace entries.
///
/// Entry paths are prefixed with the listed directory, matching the contract
/// the other backends' listings keep — the caller resolves them as workspace
/// paths and re-validates each one.
fn parse_listing(stdout: &[u8], parent: Option<&WorkspaceFilePath>) -> WorkspaceListing {
    let mut entries = Vec::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(kind), Some(size), Some(name)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let directory = kind == "d";
        entries.push(WorkspaceFileEntry {
            path: match parent {
                None => name.to_owned(),
                Some(parent) => format!("{}/{name}", parent.as_str()),
            },
            directory,
            size_bytes: (!directory).then(|| size.parse().ok()).flatten(),
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let truncated = entries.len() > MAX_WORKSPACE_LIST_ENTRIES;
    entries.truncate(MAX_WORKSPACE_LIST_ENTRIES);
    WorkspaceListing { entries, truncated }
}

/// Container ids come back from the runtime and are then spliced into later
/// argument vectors, so each one is re-proved to be an identifier before use.
/// The leading character must be alphanumeric: an id that could start with
/// `-` would read as a flag to the next invocation it is passed to.
fn validate_container_id(value: &str) -> Result<(), CodeExecutionError> {
    if value.is_empty()
        || value.len() > 128
        || !value.starts_with(|first: char| first.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CodeExecutionError::Unavailable(
            "the container runtime returned an invalid container identity".into(),
        ));
    }
    Ok(())
}

fn is_no_such_container(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("no such container") || stderr.contains("no such object")
}

fn is_name_conflict(stderr: &str) -> bool {
    stderr.to_ascii_lowercase().contains("already in use")
}

/// Whether a `docker exec` failure means the container is gone rather than
/// that the command failed. The CLI reserves 125 for its own errors, so a
/// "no such container" there is the runtime speaking, not the command.
fn is_missing_container(status: Option<i32>, stderr: &[u8]) -> bool {
    status == Some(125) && is_no_such_container(&String::from_utf8_lossy(stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecutionId;
    use openwave_egress::{DomainPattern, EgressAllowlist};

    fn provider() -> DockerExecutionProvider {
        DockerExecutionProvider::new(Duration::from_secs(30)).unwrap()
    }

    fn request(cwd: &str) -> CodeExecutionRequest {
        CodeExecutionRequest::new(
            ExecutionId::parse("call-123").unwrap(),
            ExecutionWorkspaceId::parse("chat-123").unwrap(),
            "python3",
            vec!["-c".into(), "print('a; rm -rf /')".into()],
            cwd,
        )
        .unwrap()
    }

    /// The Finder-launch regression: a GUI process inherits launchd's `PATH`,
    /// which holds no runtime CLI, and reporting that as no runtime installed
    /// is indistinguishable to the user from an uninstalled Docker. `PATH`
    /// still wins wherever it does carry one, so nothing changes for a
    /// terminal launch.
    #[test]
    fn falls_back_to_a_well_known_location_only_when_path_has_no_runtime() {
        let root = tempfile::tempdir().unwrap();
        let on_path = root.path().join("bin");
        let installed = root.path().join("usr/local/bin");
        std::fs::create_dir_all(&on_path).unwrap();
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(on_path.join("docker"), b"").unwrap();
        std::fs::write(installed.join("docker"), b"").unwrap();
        let fallbacks = [root.path().join("absent/docker"), installed.join("docker")];
        let path_var = std::env::join_paths([on_path]).unwrap();

        assert_eq!(
            resolve_binary("docker", "docker", Some(&path_var), &fallbacks),
            "docker",
        );
        assert_eq!(
            resolve_binary("docker", "docker", None, &fallbacks),
            installed.join("docker").to_string_lossy(),
        );
    }

    /// The confinement flags are the whole security story of this backend: a
    /// container that starts without one serves exactly like a container that
    /// has it, so nothing at runtime would notice a control silently dropped
    /// from this vector.
    #[test]
    fn every_container_is_confined_and_carries_no_host_mount() {
        let args = provider().run_args("chat-123");
        let flag = |name: &str| {
            args.iter()
                .position(|arg| arg == name)
                .map(|at| &args[at + 1])
        };

        assert_eq!(
            flag("--name").map(String::as_str),
            Some("openwave-exec-chat-123")
        );
        assert_eq!(flag("--user").map(String::as_str), Some(CONTAINER_USER));
        assert_eq!(flag("--cap-drop").map(String::as_str), Some("ALL"));
        assert_eq!(
            flag("--security-opt").map(String::as_str),
            Some("no-new-privileges:true")
        );
        assert!(flag("--pids-limit").is_some());
        assert!(flag("--memory").is_some());
        assert!(args.contains(&"--rm".to_owned()));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--label", "openwave.exec-workspace=chat-123"]));
        let expected_configuration = format!(
            "{CONFIGURATION_LABEL}={}",
            provider().configuration_identity()
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--label", expected_configuration.as_str()]));
        // The workspace is an anonymous volume — a `--volume` argument with a
        // host side would be a bind mount, which this backend never makes.
        assert_eq!(flag("--volume").map(String::as_str), Some(WORKSPACE_ROOT));
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains(':') && arg.starts_with('/')),
            "no argument names a host path: {args:?}"
        );
        // The image is the shared pin, resolved content-addressed.
        assert!(args.contains(&DOCUMENTS_IMAGE.to_owned()));
        assert!(provider().verifies_image_integrity());
        assert!(!provider()
            .with_image("ghcr.io/example/image:latest")
            .verifies_image_integrity());
    }

    /// The security boundary this backend does enforce. A policy that permits
    /// nothing must reach the runtime as `--network none`; a policy with
    /// grants in it must not silently borrow that enforcement, and must not
    /// name a network either — the container keeps the runtime's default.
    #[test]
    fn a_deny_all_policy_creates_the_container_with_no_network() {
        let network = |args: &[String]| {
            args.iter()
                .position(|arg| arg == "--network")
                .map(|at| args[at + 1].clone())
        };

        for policy in [
            EgressPolicy::BlockAll,
            EgressPolicy::Allowlist(EgressAllowlist::new(Vec::new(), Vec::new())),
        ] {
            let blocked = provider().with_egress_policy(policy);
            assert_eq!(
                network(&blocked.run_args("chat-123")).as_deref(),
                Some("none"),
            );
            // `--network none` is a runtime-enforced boundary with nothing
            // left reachable, and the declaration is what the settings
            // surface derives its disclosure from.
            let enforcement = DockerExecutionProvider::egress_enforcement(blocked.egress_policy())
                .expect("a deny-all policy is enforced");
            assert!(enforcement.is_credential_boundary());
            assert!(enforcement.exceptions().is_empty());
        }

        // No policy, and a policy this backend cannot enforce, both leave the
        // container on the runtime's default network — and declare nothing.
        let allowlist = EgressPolicy::Allowlist(EgressAllowlist::new(
            vec![DomainPattern::parse("pypi.org").unwrap()],
            Vec::new(),
        ));
        for open in [None, Some(allowlist)] {
            let provider = match open {
                Some(policy) => provider().with_egress_policy(policy),
                None => provider(),
            };
            assert_eq!(network(&provider.run_args("chat-123")), None);
            assert!(
                DockerExecutionProvider::egress_enforcement(provider.egress_policy()).is_none()
            );
        }
    }

    /// Two mechanisms keep a container from outliving the policy it was
    /// created under: the pooled session's egress fingerprint, which replaces
    /// a container this process created, and the configuration label, which
    /// stops a container found by name — after a restart, or from another
    /// window — being adopted into a chat with different networking.
    #[test]
    fn a_policy_change_replaces_the_container_rather_than_reusing_it() {
        let open = provider();
        let blocked = provider().with_egress_policy(EgressPolicy::BlockAll);
        let allowlist = provider().with_egress_policy(EgressPolicy::Allowlist(
            EgressAllowlist::new(vec![DomainPattern::parse("pypi.org").unwrap()], Vec::new()),
        ));

        assert_ne!(open.egress_fingerprint(), blocked.egress_fingerprint());
        // An allowlist is not enforced yet, but it is still a distinct policy:
        // conflating it with open egress would leave a stale container behind
        // the moment it becomes enforceable.
        assert_ne!(open.egress_fingerprint(), allowlist.egress_fingerprint());
        assert_ne!(blocked.egress_fingerprint(), allowlist.egress_fingerprint());

        // The pooled session key is deliberately not policy-derived — the
        // fingerprint above is what replaces the container, so a chat keeps
        // one session slot across policy edits.
        assert_eq!(
            open.credential_fingerprint(),
            blocked.credential_fingerprint()
        );

        // Adoption is keyed on the network shape, so a no-network container
        // and an ordinary one can never be adopted for each other.
        assert_ne!(
            open.configuration_identity(),
            blocked.configuration_identity()
        );
        assert_eq!(
            open.configuration_identity(),
            allowlist.configuration_identity()
        );
    }

    /// The model's argv must reach the container as argv. If it were ever
    /// flattened into shell text, `a; rm -rf /` would stop being data.
    #[test]
    fn the_command_crosses_as_argv_under_an_in_container_timeout() {
        let args = exec_command_args(
            "container-id",
            &request("reports/2026"),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "exec",
                "--workdir",
                "/workspace/reports/2026",
                "container-id",
                "timeout",
                "--kill-after=5",
                "30",
                "python3",
                "-c",
                "print('a; rm -rf /')",
            ]
        );
        // A cwd that tries to leave the workspace never reaches the runtime.
        assert!(container_cwd("../escape").is_err());
        assert_eq!(container_cwd(".").unwrap(), WORKSPACE_ROOT);
    }

    /// The listing feeds a pull that writes into host scratch, so the shape of
    /// what comes back out of the container is a contract, not a detail.
    #[test]
    fn listings_are_parsed_with_their_parent_prefix_and_bounded() {
        let listing = parse_listing(
            b"f\t12\treport.csv\nd\t4096\tnested\nf\t0\t\ngarbage\n",
            Some(&WorkspaceFilePath::parse("output").unwrap()),
        );
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| (entry.path.as_str(), entry.directory, entry.size_bytes))
                .collect::<Vec<_>>(),
            [
                ("output/nested", true, None),
                ("output/report.csv", false, Some(12)),
            ]
        );
        assert!(!listing.truncated);

        let many = (0..MAX_WORKSPACE_LIST_ENTRIES + 5)
            .map(|index| format!("f\t1\tfile-{index:04}"))
            .collect::<Vec<_>>()
            .join("\n");
        let listing = parse_listing(many.as_bytes(), None);
        assert!(listing.truncated);
        assert_eq!(listing.entries.len(), MAX_WORKSPACE_LIST_ENTRIES);
    }

    /// Ids are spliced into later argument vectors, so anything that is not an
    /// identifier is refused where it enters rather than where it is used.
    #[test]
    fn container_identities_from_the_runtime_are_re_proved() {
        let state = parse_inspect(b"9f2c4a\ttrue\tchat-123\tconfiguration-123\n").unwrap();
        assert_eq!(state.id, "9f2c4a");
        assert!(state.running);
        assert_eq!(state.workspace.as_deref(), Some("chat-123"));
        assert_eq!(state.configuration.as_deref(), Some("configuration-123"));
        let unlabeled = parse_inspect(b"9f2c4a\tfalse\t<no value>\t\n").unwrap();
        assert!(!unlabeled.running);
        assert_eq!(unlabeled.workspace, None);
        assert_eq!(unlabeled.configuration, None);
        assert!(parse_inspect(b"--privileged\ttrue\tchat-123\tconfiguration-123").is_none());
        assert!(parse_container_id(b"  9f2c4a  \n").is_some());
        assert!(parse_container_id(b"/etc/passwd").is_none());
        assert!(parse_container_id(b"").is_none());
    }

    /// A workspace name is routing, not provenance. Only a container carrying
    /// both OpenWave's workspace label and this provider configuration may be
    /// adopted; legacy/mismatched OpenWave containers are replaceable, while
    /// an unrelated name collision must not be deleted.
    #[test]
    fn container_adoption_requires_workspace_and_configuration_identity() {
        let configuration = provider().configuration_identity();
        let state = |workspace: Option<&str>, configured: Option<&str>| ContainerState {
            id: "9f2c4a".to_owned(),
            running: true,
            workspace: workspace.map(str::to_owned),
            configuration: configured.map(str::to_owned),
        };

        assert_eq!(
            state(Some("chat-123"), Some(&configuration)).disposition("chat-123", &configuration),
            ExistingContainerDisposition::Adopt
        );
        assert_eq!(
            state(Some("chat-123"), Some("stale-configuration"))
                .disposition("chat-123", &configuration),
            ExistingContainerDisposition::Replace
        );
        assert_eq!(
            state(Some("chat-123"), None).disposition("chat-123", &configuration),
            ExistingContainerDisposition::Replace
        );
        assert_eq!(
            state(None, None).disposition("chat-123", &configuration),
            ExistingContainerDisposition::Conflict
        );
        assert_eq!(
            state(Some("other-chat"), Some(&configuration)).disposition("chat-123", &configuration),
            ExistingContainerDisposition::Conflict
        );

        assert_ne!(
            provider().configuration_identity(),
            provider()
                .with_image("ghcr.io/example/image:latest")
                .configuration_identity()
        );
        assert_ne!(
            provider().configuration_identity(),
            provider().with_binary("podman").configuration_identity()
        );
    }

    /// A container that vanished must reach the session layer as `Missing` so
    /// the pool replaces it, rather than as an exit status the model reads as
    /// its command having failed.
    #[test]
    fn a_vanished_container_is_distinguished_from_a_failing_command() {
        assert!(is_missing_container(
            Some(125),
            b"Error: No such container: x"
        ));
        assert!(!is_missing_container(Some(1), b"No such container: x"));
        assert!(!is_missing_container(Some(125), b"something else"));
        assert!(is_name_conflict(
            "The container name \"/openwave-exec-x\" is already in use"
        ));
    }

    /// Requires a working Docker daemon and pulls a multi-gigabyte image on
    /// first run. Run it with:
    ///
    /// ```text
    /// cargo test -p openwave-code-execution -- --ignored docker_container
    /// ```
    #[tokio::test]
    #[ignore = "requires a container runtime and pulls the documents image"]
    async fn docker_container_runs_commands_and_round_trips_workspace_files() {
        DockerExecutionProvider::availability()
            .await
            .expect("a container runtime is available");
        let provider = provider();
        let workspace = ExecutionWorkspaceId::parse("chat-docker-e2e").unwrap();

        let response = provider
            .execute(
                CodeExecutionRequest::new(
                    ExecutionId::parse("call-e2e-1").unwrap(),
                    workspace.clone(),
                    "python3",
                    vec!["-c".into(), "import openpyxl; print('ok')".into()],
                    ".",
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);
        assert_eq!(response.stdout.trim(), "ok");
        assert_eq!(response.provider, CodeExecutionProviderKind::Docker);

        let path = WorkspaceFilePath::parse("output/report.bin").unwrap();
        let content = b"\x00docker\xff".to_vec();
        provider
            .put_workspace_file(&workspace, &path, &content)
            .await
            .unwrap();
        assert_eq!(
            provider
                .get_workspace_file(&workspace, &path)
                .await
                .unwrap(),
            content
        );
        let listing = provider
            .list_workspace_files(
                &workspace,
                Some(&WorkspaceFilePath::parse("output").unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(listing.entries[0].path, "output/report.bin");
        assert!(matches!(
            provider
                .get_workspace_file(&workspace, &WorkspaceFilePath::parse("nope").unwrap())
                .await,
            Err(CodeExecutionError::WorkspaceFileNotFound)
        ));

        // The command is stopped inside the container, not by abandoning the
        // CLI on this side.
        let timed_out = DockerExecutionProvider::with_session_pool(
            Duration::from_secs(2),
            provider.pool.clone(),
        )
        .unwrap()
        .execute(
            CodeExecutionRequest::new(
                ExecutionId::parse("call-e2e-2").unwrap(),
                workspace.clone(),
                "sleep",
                vec!["30".into()],
                ".",
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(timed_out.timed_out);

        provider.destroy_workspace(&workspace).await.unwrap();
        assert!(!provider.connect_workspace(&workspace).await.unwrap());
    }
}
